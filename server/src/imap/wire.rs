//! The IMAP wire: reading one command and writing the responses.
//!
//! The parser is deliberately small. It understands the argument shapes
//! IMAP4rev1 clients send plus literals, which is everything Apple Mail,
//! Thunderbird and Outlook use.

use mailcore::stream::{Conn, InboundStream};
use std::io;

/// Longest command line we accept before refusing the command.
const MAX_LINE: usize = 64 * 1024;

/// The largest literal we accept, so a hostile client cannot exhaust memory.
const MAX_LITERAL: usize = 32 * 1024 * 1024;

/// One argument of a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Arg {
    Atom(String),
    Quoted(String),
    /// A parenthesized list.
    List(Vec<Arg>),
    /// The bytes after a `{n}` marker.
    Literal(Vec<u8>),
}

impl Arg {
    /// The argument as text. A literal is read as UTF-8 when it is text at all,
    /// which is the case for every literal a client sends in a command.
    pub fn text(&self) -> &str {
        match self {
            Arg::Atom(value) | Arg::Quoted(value) => value,
            Arg::Literal(bytes) => std::str::from_utf8(bytes).unwrap_or(""),
            Arg::List(_) => "",
        }
    }

    /// The argument as text, upper-cased, for comparing command words.
    pub fn upper(&self) -> String {
        self.text().to_ascii_uppercase()
    }

    pub fn list(&self) -> Option<&[Arg]> {
        match self {
            Arg::List(items) => Some(items),
            _ => None,
        }
    }
}

/// One parsed command: its tag, its upper-cased name and its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub tag: String,
    pub name: String,
    pub args: Vec<Arg>,
}

/// What one read produced.
#[derive(Debug)]
pub enum Incoming {
    Command(Command),
    /// The line could not be parsed; the tag is still reported so the session
    /// can answer BAD with it.
    Bad {
        tag: String,
        reason: &'static str,
    },
    /// The connection closed cleanly.
    Eof,
}

/// Derived for the readable panic in the tests.
#[derive(Debug)]
enum Parsed {
    Done(Vec<Arg>),
    /// Arguments so far plus the size of the literal that follows.
    Literal(Vec<Arg>, usize),
}

/// Reads one command, following any literals it carries.
pub async fn read_command(conn: &mut Conn<InboundStream>) -> io::Result<Incoming> {
    let Some(first) = conn.read_line(MAX_LINE).await? else {
        return Ok(Incoming::Eof);
    };
    let tag = first_tag(&first);
    let mut args: Vec<Arg> = Vec::new();
    let mut line = first;
    loop {
        let mut pos = 0;
        match parse_args(&line, &mut pos, 0) {
            Ok(Parsed::Done(mut tokens)) => {
                args.append(&mut tokens);
                break;
            }
            Ok(Parsed::Literal(mut tokens, size)) => {
                args.append(&mut tokens);
                if size > MAX_LITERAL {
                    return Ok(Incoming::Bad {
                        tag,
                        reason: "literal too large",
                    });
                }
                let bytes = conn.read_exact_n(size).await?;
                args.push(Arg::Literal(bytes));
                match conn.read_line(MAX_LINE).await? {
                    Some(next) => line = next,
                    None => break,
                }
            }
            Err(reason) => return Ok(Incoming::Bad { tag, reason }),
        }
    }

    match (args.first(), args.get(1)) {
        (Some(tag_arg), Some(name_arg)) => Ok(Incoming::Command(Command {
            tag: tag_arg.text().to_string(),
            name: name_arg.upper(),
            args: args[2..].to_vec(),
        })),
        _ => Ok(Incoming::Bad {
            tag,
            reason: "incomplete command",
        }),
    }
}

/// The tag out of a raw line, for a response that has to name it.
fn first_tag(line: &[u8]) -> String {
    let end = line
        .iter()
        .position(|byte| *byte == b' ' || *byte == b'\t')
        .unwrap_or(line.len());
    String::from_utf8_lossy(&line[..end]).to_string()
}

fn parse_args(bytes: &[u8], pos: &mut usize, depth: usize) -> Result<Parsed, &'static str> {
    let mut args = Vec::new();
    loop {
        while *pos < bytes.len() && (bytes[*pos] == b' ' || bytes[*pos] == b'\t') {
            *pos += 1;
        }
        if *pos >= bytes.len() {
            return Ok(Parsed::Done(args));
        }
        match bytes[*pos] {
            b'(' => {
                *pos += 1;
                match parse_args(bytes, pos, depth + 1)? {
                    Parsed::Done(items) => args.push(Arg::List(items)),
                    Parsed::Literal(_, _) => return Err("a literal cannot open a list"),
                }
                while *pos < bytes.len() && (bytes[*pos] == b' ' || bytes[*pos] == b'\t') {
                    *pos += 1;
                }
                if *pos >= bytes.len() || bytes[*pos] != b')' {
                    return Err("unbalanced parenthesis");
                }
                *pos += 1;
            }
            b')' => {
                if depth == 0 {
                    return Err("unbalanced parenthesis");
                }
                return Ok(Parsed::Done(args));
            }
            b'"' => {
                *pos += 1;
                let mut value = String::new();
                loop {
                    if *pos >= bytes.len() {
                        return Err("unterminated quoted string");
                    }
                    match bytes[*pos] {
                        b'"' => {
                            *pos += 1;
                            break;
                        }
                        b'\\' => {
                            *pos += 1;
                            if *pos >= bytes.len() {
                                return Err("unterminated escape");
                            }
                            value.push(bytes[*pos] as char);
                            *pos += 1;
                        }
                        byte => {
                            value.push(byte as char);
                            *pos += 1;
                        }
                    }
                }
                args.push(Arg::Quoted(value));
            }
            b'{' => {
                let end = bytes[*pos + 1..]
                    .iter()
                    .position(|byte| *byte == b'}')
                    .map(|offset| *pos + 1 + offset)
                    .ok_or("unterminated literal")?;
                let digits: String = bytes[*pos + 1..end]
                    .iter()
                    .filter(|byte| **byte != b'+')
                    .map(|byte| *byte as char)
                    .collect();
                let size: usize = digits.parse().map_err(|_| "bad literal size")?;
                *pos = end + 1;
                while *pos < bytes.len() && (bytes[*pos] == b' ' || bytes[*pos] == b'\t') {
                    *pos += 1;
                }
                if *pos != bytes.len() {
                    return Err("a literal must end the line");
                }
                return Ok(Parsed::Literal(args, size));
            }
            _ => {
                let start = *pos;
                // A fetch attribute can carry a bracketed section that holds
                // spaces and parentheses, as in BODY.PEEK[HEADER.FIELDS (DATE)],
                // so brackets keep the atom together.
                let mut in_brackets = false;
                while *pos < bytes.len() {
                    match bytes[*pos] {
                        b'[' => in_brackets = true,
                        b']' => in_brackets = false,
                        b' ' | b'\t' | b'(' | b')' | b'"' if !in_brackets => break,
                        _ => {}
                    }
                    *pos += 1;
                }
                if start == *pos {
                    return Err("empty argument");
                }
                args.push(Arg::Atom(
                    String::from_utf8_lossy(&bytes[start..*pos]).to_string(),
                ));
            }
        }
    }
}

/// A sequence set like `2,4:7,9`, with `*` left as 0 for the caller to resolve.
/// Tokenizes a line for the sibling modules' tests.
#[cfg(test)]
pub fn test_args(line: &str) -> Vec<Arg> {
    let mut pos = 0;
    match parse_args(line.as_bytes(), &mut pos, 0) {
        Ok(Parsed::Done(args)) => args,
        other => panic!("expected plain arguments, got {other:?}"),
    }
}

pub fn parse_sequence_set(input: &str) -> Option<Vec<(u32, u32)>> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut ranges = Vec::new();
    for part in trimmed.split(',') {
        let (start, end) = match part.trim().split_once(':') {
            Some((start, end)) => (number(start)?, number(end)?),
            None => {
                let value = number(part)?;
                (value, value)
            }
        };
        if start != 0 && end != 0 && start > end {
            return None;
        }
        ranges.push((start, end));
    }
    Some(ranges)
}

fn number(value: &str) -> Option<u32> {
    let value = value.trim();
    if value == "*" {
        return Some(0);
    }
    value.parse().ok()
}

/// True when `value` falls in the set, with 0 standing for `max`.
pub fn sequence_set_matches(set: &[(u32, u32)], value: u32, max: u32) -> bool {
    set.iter().any(|(start, end)| {
        let start = if *start == 0 { max } else { *start };
        let end = if *end == 0 { max } else { *end };
        value >= start.min(end) && value <= start.max(end)
    })
}

/// An IMAP quoted string, escaped as the grammar requires.
pub fn quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        if ch == '"' || ch == '\\' {
            out.push('\\');
        }
        out.push(ch);
    }
    out.push('"');
    out
}

/// NIL for an absent string, otherwise a quoted string.
pub fn nstring(value: Option<&str>) -> String {
    match value {
        Some(value) => quote(value),
        None => "NIL".to_string(),
    }
}

/// A FETCH/STORE flag list.
pub fn flag_list(flags: &[String]) -> String {
    if flags.is_empty() {
        return "()".to_string();
    }
    format!("({})", flags.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &str) -> Vec<Arg> {
        let mut pos = 0;
        match parse_args(line.as_bytes(), &mut pos, 0) {
            Ok(Parsed::Done(args)) => args,
            other => panic!("expected plain arguments, got {other:?}"),
        }
    }

    #[test]
    fn parses_atoms_quoted_strings_and_lists() {
        let args = parse("LOGIN \"alice@example.test\" secret");
        assert_eq!(
            args,
            vec![
                Arg::Atom("LOGIN".to_string()),
                Arg::Quoted("alice@example.test".to_string()),
                Arg::Atom("secret".to_string()),
            ]
        );
        let args = parse("FETCH 1:3 (FLAGS UID BODY.PEEK[HEADER])");
        assert_eq!(args[1], Arg::Atom("1:3".to_string()));
        let list = args[2].list().expect("a list");
        assert_eq!(list[0], Arg::Atom("FLAGS".to_string()));
        assert_eq!(list[2], Arg::Atom("BODY.PEEK[HEADER]".to_string()));
    }

    #[test]
    fn a_quoted_string_can_hold_escapes_and_spaces() {
        let args = parse("SELECT \"My \\\"Folder\\\"\"");
        assert_eq!(
            args,
            vec![
                Arg::Atom("SELECT".to_string()),
                Arg::Quoted("My \"Folder\"".to_string())
            ]
        );
    }

    #[test]
    fn a_trailing_literal_asks_for_its_bytes() {
        let mut pos = 0;
        match parse_args(b"APPEND Drafts {42}", &mut pos, 0) {
            Ok(Parsed::Literal(args, size)) => {
                assert_eq!(size, 42);
                assert_eq!(
                    args,
                    vec![
                        Arg::Atom("APPEND".to_string()),
                        Arg::Atom("Drafts".to_string())
                    ]
                );
            }
            other => panic!("expected a literal, got {other:?}"),
        }
        // LITERAL+ announces the same bytes without waiting for a continuation.
        let mut pos = 0;
        assert!(matches!(
            parse_args(b"APPEND Drafts {42+}", &mut pos, 0),
            Ok(Parsed::Literal(_, 42))
        ));
    }

    #[test]
    fn rejects_malformed_lines() {
        for line in [
            "(FETCH",
            "FETCH )",
            "\"unterminated",
            "{x}",
            "FETCH 1 {5} extra",
        ] {
            let mut pos = 0;
            assert!(parse_args(line.as_bytes(), &mut pos, 0).is_err(), "{line}");
        }
    }

    #[test]
    fn a_sequence_set_carries_star_as_zero() {
        assert_eq!(
            parse_sequence_set("1,3:5,*").unwrap(),
            vec![(1, 1), (3, 5), (0, 0)]
        );
        assert_eq!(parse_sequence_set("7").unwrap(), vec![(7, 7)]);
        assert!(parse_sequence_set("").is_none());
        assert!(parse_sequence_set("5:3").is_none());
        assert!(parse_sequence_set("abc").is_none());
    }

    #[test]
    fn a_sequence_set_matches_ranges_and_star() {
        let set = parse_sequence_set("2,4:6,*").unwrap();
        assert!(!sequence_set_matches(&set, 1, 9));
        assert!(sequence_set_matches(&set, 2, 9));
        assert!(sequence_set_matches(&set, 5, 9));
        assert!(sequence_set_matches(&set, 9, 9));
        assert!(!sequence_set_matches(&set, 8, 9));
    }

    #[test]
    fn quotes_and_escapes_a_string() {
        assert_eq!(quote("Inbox"), "\"Inbox\"");
        assert_eq!(quote("a\"b"), "\"a\\\"b\"");
        assert_eq!(nstring(None), "NIL");
        assert_eq!(flag_list(&["\\Seen".to_string()]), "(\\Seen)");
        assert_eq!(flag_list(&[]), "()");
    }
}
