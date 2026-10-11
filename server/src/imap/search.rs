//! The IMAP SEARCH criteria the reader understands.
//!
//! Evaluation is three-valued: a criterion that needs the stored bytes answers
//! None until they are fetched, so a SEARCH like UNSEEN never reads S3 and one
//! that does read a body only reads it for a message the rest of the criteria
//! already matched.

use super::wire::{Arg, parse_sequence_set, sequence_set_matches};
use chrono::NaiveDate;
use mail_parser::Message;
use mailcore::query::FolderEntry;

/// One search criterion, already parsed.
#[derive(Debug, Clone, PartialEq)]
pub enum Criterion {
    All,
    /// A flag the reader persists, and whether it must be set.
    Flag(Flag, bool),
    /// A header field and the substring its value must carry.
    Header(String, String),
    Body(String),
    Text(String),
    /// A date compared against the message's own date when sent.
    Date(DateOp, NaiveDate, bool),
    Larger(u64),
    Smaller(u64),
    Uid(Vec<(u32, u32)>),
    Not(Box<Criterion>),
    Or(Box<Criterion>, Box<Criterion>),
    And(Vec<Criterion>),
}

/// The flags a SEARCH can test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flag {
    Seen,
    Answered,
    Flagged,
    Deleted,
    Draft,
    Recent,
}

/// How a date criterion compares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateOp {
    Before,
    On,
    Since,
}

/// Parses a SEARCH argument list, skipping the optional CHARSET clause.
pub fn parse(args: &[Arg]) -> Option<Criterion> {
    let mut pos = 0;
    let args = skip_charset(args)?;
    let mut items = Vec::new();
    while pos < args.len() {
        items.push(parse_one(args, &mut pos)?);
    }
    if items.is_empty() {
        return None;
    }
    Some(Criterion::And(items))
}

fn skip_charset(args: &[Arg]) -> Option<&[Arg]> {
    if args.first().map(|arg| arg.upper()) == Some("CHARSET".to_string()) {
        return args.get(2..);
    }
    Some(args)
}

fn parse_one(args: &[Arg], pos: &mut usize) -> Option<Criterion> {
    let argument = args.get(*pos)?;
    // A parenthesized group is one criterion of its own.
    if let Some(items) = argument.list() {
        *pos += 1;
        let mut inner = 0;
        let mut parsed = Vec::new();
        while inner < items.len() {
            parsed.push(parse_one(items, &mut inner)?);
        }
        return Some(Criterion::And(parsed));
    }
    let key = argument.upper();
    *pos += 1;
    let string = |args: &[Arg], pos: &mut usize| -> Option<String> {
        let value = args.get(*pos)?.text().to_string();
        *pos += 1;
        Some(value)
    };
    Some(match key.as_str() {
        "ALL" => Criterion::All,
        "NOT" => Criterion::Not(Box::new(parse_one(args, pos)?)),
        "OR" => Criterion::Or(
            Box::new(parse_one(args, pos)?),
            Box::new(parse_one(args, pos)?),
        ),
        "SEEN" => Criterion::Flag(Flag::Seen, true),
        "UNSEEN" => Criterion::Flag(Flag::Seen, false),
        "ANSWERED" => Criterion::Flag(Flag::Answered, true),
        "UNANSWERED" => Criterion::Flag(Flag::Answered, false),
        "FLAGGED" => Criterion::Flag(Flag::Flagged, true),
        "UNFLAGGED" => Criterion::Flag(Flag::Flagged, false),
        "DELETED" => Criterion::Flag(Flag::Deleted, true),
        "UNDELETED" => Criterion::Flag(Flag::Deleted, false),
        "DRAFT" => Criterion::Flag(Flag::Draft, true),
        "UNDRAFT" => Criterion::Flag(Flag::Draft, false),
        "RECENT" | "NEW" => Criterion::Flag(Flag::Recent, true),
        "OLD" => Criterion::Flag(Flag::Recent, false),
        "KEYWORD" => {
            string(args, pos)?;
            Criterion::All
        }
        "UNKEYWORD" => {
            string(args, pos)?;
            Criterion::All
        }
        "FROM" => Criterion::Header("from".to_string(), string(args, pos)?),
        "TO" => Criterion::Header("to".to_string(), string(args, pos)?),
        "CC" => Criterion::Header("cc".to_string(), string(args, pos)?),
        "BCC" => Criterion::Header("bcc".to_string(), string(args, pos)?),
        "SUBJECT" => Criterion::Header("subject".to_string(), string(args, pos)?),
        "BODY" => Criterion::Body(string(args, pos)?),
        "TEXT" => Criterion::Text(string(args, pos)?),
        "HEADER" => {
            let field = string(args, pos)?;
            let value = string(args, pos)?;
            Criterion::Header(field.to_ascii_lowercase(), value)
        }
        "BEFORE" => Criterion::Date(DateOp::Before, parse_date(&string(args, pos)?)?, false),
        "ON" => Criterion::Date(DateOp::On, parse_date(&string(args, pos)?)?, false),
        "SINCE" => Criterion::Date(DateOp::Since, parse_date(&string(args, pos)?)?, false),
        "SENTBEFORE" => Criterion::Date(DateOp::Before, parse_date(&string(args, pos)?)?, true),
        "SENTON" => Criterion::Date(DateOp::On, parse_date(&string(args, pos)?)?, true),
        "SENTSINCE" => Criterion::Date(DateOp::Since, parse_date(&string(args, pos)?)?, true),
        "LARGER" => Criterion::Larger(string(args, pos)?.parse().ok()?),
        "SMALLER" => Criterion::Smaller(string(args, pos)?.parse().ok()?),
        "UID" => Criterion::Uid(parse_sequence_set(&string(args, pos)?)?),
        _ => return None,
    })
}

/// A date in the dd-Mmm-yyyy form an IMAP client sends.
pub fn parse_date(value: &str) -> Option<NaiveDate> {
    const MONTHS: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let mut pieces = value.trim().split('-');
    let day: u32 = pieces.next()?.trim().parse().ok()?;
    let month_name = pieces.next()?.trim().to_ascii_lowercase();
    let month = MONTHS.iter().position(|name| *name == month_name)? as u32 + 1;
    let year: i32 = pieces.next()?.trim().parse().ok()?;
    NaiveDate::from_ymd_opt(year, month, day)
}

/// Evaluates one criterion. None means the stored bytes are needed.
pub fn evaluate(
    criterion: &Criterion,
    entry: &FolderEntry,
    raw: Option<&Message<'_>>,
    draft: bool,
) -> Option<bool> {
    Some(match criterion {
        Criterion::All => true,
        Criterion::Flag(flag, want) => {
            let value = match flag {
                Flag::Seen => entry.read,
                Flag::Answered => entry.answered,
                Flag::Flagged => entry.starred,
                Flag::Deleted => entry.deleted,
                Flag::Draft => draft,
                // This reader keeps no server-side recent state.
                Flag::Recent => false,
            };
            value == *want
        }
        Criterion::Larger(size) => entry.size_bytes as u64 > *size,
        Criterion::Smaller(size) => (entry.size_bytes as u64) < *size,
        Criterion::Uid(set) => sequence_set_matches(set, entry.uid as u32, u32::MAX),
        Criterion::Not(inner) => !evaluate(inner, entry, raw, draft)?,
        Criterion::Or(left, right) => {
            let left = evaluate(left, entry, raw, draft);
            let right = evaluate(right, entry, raw, draft);
            match (left, right) {
                (Some(true), _) | (_, Some(true)) => true,
                (Some(false), Some(false)) => false,
                _ => return None,
            }
        }
        Criterion::And(items) => {
            let mut pending = false;
            for item in items {
                match evaluate(item, entry, raw, draft) {
                    Some(false) => return Some(false),
                    Some(true) => {}
                    None => pending = true,
                }
            }
            if pending {
                return None;
            }
            true
        }
        Criterion::Date(op, date, sent) => {
            let value = if *sent {
                let parsed = raw?;
                parsed.date().and_then(|date| {
                    NaiveDate::from_ymd_opt(date.year as i32, date.month as u32, date.day as u32)
                })?
            } else {
                entry.received_at.date_naive()
            };
            match op {
                DateOp::Before => value < *date,
                DateOp::On => value == *date,
                DateOp::Since => value >= *date,
            }
        }
        Criterion::Header(field, value) => {
            let parsed = raw?;
            header_value(parsed, field)
                .map(|header| contains_ignore_case(&header, value))
                .unwrap_or(false)
        }
        Criterion::Body(value) => {
            let parsed = raw?;
            contains_ignore_case(
                &String::from_utf8_lossy(body_of(parsed.raw_message())),
                value,
            )
        }
        Criterion::Text(value) => {
            let parsed = raw?;
            contains_ignore_case(&String::from_utf8_lossy(parsed.raw_message()), value)
        }
    })
}

/// The bytes after the blank line that ends the headers.
fn body_of(raw: &[u8]) -> &[u8] {
    for (index, window) in raw.windows(4).enumerate() {
        if window == b"\r\n\r\n" {
            return &raw[index + 4..];
        }
        if window == b"\n\n" {
            return &raw[index + 2..];
        }
    }
    &[]
}

/// One header's value, unfolded, joined when the header repeats.
fn header_value(parsed: &Message<'_>, field: &str) -> Option<String> {
    let values: Vec<&str> = parsed
        .headers_raw()
        .filter(|(name, _)| name.eq_ignore_ascii_case(field))
        .map(|(_, value)| value)
        .collect();
    if values.is_empty() {
        None
    } else {
        Some(values.join(" "))
    }
}

fn contains_ignore_case(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use uuid::Uuid;

    fn entry() -> FolderEntry {
        FolderEntry {
            id: Uuid::now_v7(),
            uid: 7,
            read: true,
            starred: false,
            answered: false,
            deleted: false,
            size_bytes: 120,
            received_at: DateTime::parse_from_rfc3339("2026-03-04T10:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            subject: Some("Hello".to_string()),
            header_from: Some("alice@example.test".to_string()),
            message_id: Some("abc@example.test".to_string()),
            envelope_from: "alice@example.test".to_string(),
            envelope_to: "bob@example.test".to_string(),
            in_reply_to: None,
            storage_key: "key".to_string(),
            direction: "inbound".to_string(),
            status: "stored".to_string(),
        }
    }

    fn args(value: &str) -> Vec<Arg> {
        // Reuse the wire tokenizer so the tests exercise the same shapes.
        super::super::wire::test_args(value)
    }

    #[test]
    fn parses_the_criteria_clients_send() {
        let parsed = parse(&args("UNSEEN")).unwrap();
        assert_eq!(
            parsed,
            Criterion::And(vec![Criterion::Flag(Flag::Seen, false)])
        );

        let parsed = parse(&args("CHARSET UTF-8 SINCE 1-Feb-2026 FROM alice")).unwrap();
        assert_eq!(
            parsed,
            Criterion::And(vec![
                Criterion::Date(
                    DateOp::Since,
                    NaiveDate::from_ymd_opt(2026, 2, 1).unwrap(),
                    false
                ),
                Criterion::Header("from".to_string(), "alice".to_string()),
            ])
        );

        let parsed = parse(&args("OR FLAGGED (LARGER 100 UID 3:9)")).unwrap();
        assert!(matches!(parsed, Criterion::And(_)));
        assert!(parse(&args("NONSENSE")).is_none());
    }

    #[test]
    fn evaluates_flags_sizes_and_dates_without_the_bytes() {
        let entry = entry();
        let seen = Criterion::Flag(Flag::Seen, true);
        assert_eq!(evaluate(&seen, &entry, None, false), Some(true));
        let flagged = Criterion::Flag(Flag::Flagged, true);
        assert_eq!(evaluate(&flagged, &entry, None, false), Some(false));
        assert_eq!(
            evaluate(&Criterion::Larger(100), &entry, None, false),
            Some(true)
        );
        assert_eq!(
            evaluate(&Criterion::Smaller(100), &entry, None, false),
            Some(false)
        );
        assert_eq!(
            evaluate(
                &Criterion::Date(
                    DateOp::On,
                    NaiveDate::from_ymd_opt(2026, 3, 4).unwrap(),
                    false
                ),
                &entry,
                None,
                false
            ),
            Some(true)
        );
        assert_eq!(
            evaluate(&Criterion::Uid(vec![(7, 7)]), &entry, None, false),
            Some(true)
        );
        assert_eq!(
            evaluate(&Criterion::Body("x".into()), &entry, None, false),
            None
        );
    }

    #[test]
    fn evaluates_text_criteria_from_the_message() {
        let raw = b"From: Alice <alice@example.test>\r\nSubject: Hello\r\n\r\nhello world\r\n";
        let parsed = mail_parser::MessageParser::default().parse(raw).unwrap();
        let entry = entry();
        assert_eq!(
            evaluate(
                &Criterion::Header("from".to_string(), "alice".to_string()),
                &entry,
                Some(&parsed),
                false
            ),
            Some(true)
        );
        assert_eq!(
            evaluate(
                &Criterion::Body("world".to_string()),
                &entry,
                Some(&parsed),
                false
            ),
            Some(true)
        );
        assert_eq!(
            evaluate(
                &Criterion::Text("nothing".to_string()),
                &entry,
                Some(&parsed),
                false
            ),
            Some(false)
        );
        assert_eq!(
            evaluate(
                &Criterion::Date(
                    DateOp::Since,
                    NaiveDate::from_ymd_opt(2000, 1, 1).unwrap(),
                    true
                ),
                &entry,
                Some(&parsed),
                false
            ),
            None
        );
    }
}
