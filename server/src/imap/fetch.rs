//! ENVELOPE, BODYSTRUCTURE and BODY[section] for the IMAP reader.
//!
//! Everything here is derived from the stored raw message with mail-parser, so
//! the reader never re-implements MIME decoding: it only maps the parsed tree
//! onto the shapes IMAP asks for.

use super::wire::{nstring, quote};
use mail_parser::{Message, MessagePart, MimeHeaders};

/// The IMAP ENVELOPE of a parsed message.
pub fn envelope(parsed: &Message<'_>) -> String {
    let in_reply_to = parsed.in_reply_to().as_text().and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    });
    let message_id = parsed.message_id().map(|id| format!("<{id}>"));
    format!(
        "({} {} {} {} {} {} {} {} {} {})",
        nstring(parsed.date().map(format_date).as_deref()),
        nstring(parsed.subject()),
        addresses(parsed.from()),
        addresses(parsed.sender()),
        addresses(parsed.reply_to()),
        addresses(parsed.to()),
        addresses(parsed.cc()),
        addresses(parsed.bcc()),
        nstring(in_reply_to.as_deref()),
        nstring(message_id.as_deref()),
    )
}

/// The IMAP date-time a client can read back from an ENVELOPE.
fn format_date(date: &mail_parser::DateTime) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let month = MONTHS
        .get(date.month.saturating_sub(1) as usize)
        .copied()
        .unwrap_or("Jan");
    // mail-parser reports a zone west of GMT as "before" it, which is the
    // negative offset.
    let sign = if date.tz_before_gmt { '-' } else { '+' };
    format!(
        "{:02}-{month}-{:04} {:02}:{:02}:{:02} {sign}{:02}{:02}",
        date.day, date.year, date.hour, date.minute, date.second, date.tz_hour, date.tz_minute,
    )
}

/// One IMAP address list, or NIL when the header is absent.
fn addresses(value: Option<&mail_parser::Address<'_>>) -> String {
    let Some(list) = value else {
        return "NIL".to_string();
    };
    let items: Vec<String> = list
        .iter()
        .map(|entry| {
            let address = entry.address().unwrap_or("");
            let (mailbox, host) = match address.rsplit_once('@') {
                Some((mailbox, host)) => (mailbox.to_string(), host.to_string()),
                None => (address.to_string(), String::new()),
            };
            format!(
                "({} NIL {} {})",
                nstring(entry.name()),
                quote(&mailbox),
                quote(&host)
            )
        })
        .collect();
    if items.is_empty() {
        "NIL".to_string()
    } else {
        format!("({})", items.join(""))
    }
}

/// The IMAP BODYSTRUCTURE of a parsed message. BODY carries the same shape:
/// the extension data is optional and this reader has none to add.
pub fn body_structure(parsed: &Message<'_>, raw: &[u8]) -> String {
    structure(parsed, raw, parsed.root_part())
}

fn structure(parsed: &Message<'_>, raw: &[u8], part: &MessagePart<'_>) -> String {
    if let Some(children) = part.sub_parts() {
        let mut out = String::from("(");
        for child in children {
            if let Some(child) = parsed.part(*child) {
                out.push_str(&structure(parsed, raw, child));
            }
        }
        out.push(' ');
        out.push_str(&quote(&subtype(part)));
        let params = parameters(part);
        if !params.is_empty() {
            out.push(' ');
            out.push_str(&params);
        }
        if let Some(disposition) = disposition(part) {
            out.push(' ');
            out.push_str(&disposition);
        }
        out.push(')');
        return out;
    }

    let content_type = part
        .content_type()
        .map(|value| value.ctype().to_ascii_uppercase())
        .unwrap_or_else(|| "TEXT".to_string());
    let params = parameters(part);
    // The size a client is told is the size of the bytes it will fetch, so it
    // comes from the part's own range in the stored message, not a decode.
    let (_, body, end) = range(raw, part);
    let size = end.saturating_sub(body);
    let mut out = format!(
        "({} {} {} {} {} {} {}",
        quote(&content_type),
        quote(&subtype(part)),
        if params.is_empty() {
            "NIL".to_string()
        } else {
            params
        },
        nstring(part.content_id()),
        nstring(part.content_description()),
        quote(&encoding(part)),
        size,
    );
    if content_type == "TEXT" {
        out.push_str(&format!(" {}", line_count(&raw[body..end])));
    }
    if content_type == "MESSAGE"
        && let Some(inner) = part.message()
    {
        out.push(' ');
        out.push_str(&envelope(inner));
        out.push(' ');
        out.push_str(&structure(inner, raw, inner.root_part()));
        out.push_str(&format!(" {}", line_count(&raw[body..end])));
    }
    // The MD5 field is always NIL: this reader does not sign body parts.
    out.push_str(" NIL");
    if let Some(disposition) = disposition(part) {
        out.push(' ');
        out.push_str(&disposition);
    }
    out.push(')');
    out
}

fn subtype(part: &MessagePart<'_>) -> String {
    part.content_type()
        .and_then(|value| value.subtype())
        .map(|value| value.to_ascii_uppercase())
        .unwrap_or_else(|| "PLAIN".to_string())
}

fn encoding(part: &MessagePart<'_>) -> String {
    use mail_parser::Encoding;
    match part.encoding {
        Encoding::QuotedPrintable => "QUOTED-PRINTABLE".to_string(),
        Encoding::Base64 => "BASE64".to_string(),
        Encoding::None => "7BIT".to_string(),
    }
}

/// The RFC 2231 parameters of a part's content type, as an IMAP list.
fn parameters(part: &MessagePart<'_>) -> String {
    let Some(content_type) = part.content_type() else {
        return String::new();
    };
    let Some(attributes) = content_type.attributes() else {
        return String::new();
    };
    let items: Vec<String> = attributes
        .iter()
        .map(|attribute| {
            format!(
                "{} {}",
                quote(&attribute.name.to_ascii_uppercase()),
                quote(&attribute.value)
            )
        })
        .collect();
    if items.is_empty() {
        String::new()
    } else {
        format!("({})", items.join(" "))
    }
}

/// The Content-Disposition of a part, as IMAP's optional disposition field.
fn disposition(part: &MessagePart<'_>) -> Option<String> {
    let disposition = part.content_disposition()?;
    let params = disposition
        .attributes()
        .map(|attributes| {
            let items: Vec<String> = attributes
                .iter()
                .map(|attribute| {
                    format!(
                        "{} {}",
                        quote(&attribute.name.to_ascii_uppercase()),
                        quote(&attribute.value)
                    )
                })
                .collect();
            items.join(" ")
        })
        .filter(|value| !value.is_empty())
        .map(|value| format!("({value})"))
        .unwrap_or_else(|| "NIL".to_string());
    Some(format!(
        "({} {})",
        quote(&disposition.ctype().to_ascii_uppercase()),
        params
    ))
}

/// The number of lines IMAP reports for a text body.
fn line_count(bytes: &[u8]) -> usize {
    if bytes.is_empty() {
        return 0;
    }
    let newlines = bytes.iter().filter(|byte| **byte == b'\n').count();
    if bytes.ends_with(b"\n") {
        newlines
    } else {
        newlines + 1
    }
}

/// What BODY[section] asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Kind {
    /// The whole part, or the whole message when there is no path.
    Full,
    Header,
    Text,
    Mime,
    /// HEADER.FIELDS, with the boolean saying whether the list is excluded.
    Fields(Vec<String>, bool),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SectionSpec {
    path: Vec<u32>,
    kind: Kind,
}

/// The bytes of one BODY[section] request, or None when the section is absent.
pub fn section(parsed: &Message<'_>, raw: &[u8], spec: &str) -> Option<Vec<u8>> {
    let spec = parse_section(spec)?;
    let root = parsed.root_part();
    if spec.path.is_empty() {
        let (start, body, end) = range(raw, root);
        return Some(match spec.kind {
            Kind::Full => raw[..end].to_vec(),
            Kind::Header => raw[start..body].to_vec(),
            Kind::Text => raw[body..end].to_vec(),
            Kind::Mime => return None,
            Kind::Fields(fields, exclude) => header_fields(&raw[start..body], &fields, exclude),
        });
    }

    let part = resolve(parsed, &spec.path)?;
    let (start, body, end) = range(raw, part);
    Some(match spec.kind {
        Kind::Full => raw[body..end].to_vec(),
        Kind::Text => raw[body..end].to_vec(),
        Kind::Header | Kind::Mime => raw[start..body].to_vec(),
        Kind::Fields(fields, exclude) => header_fields(&raw[start..body], &fields, exclude),
    })
}

/// The header, body and end offsets of a part, clamped to the raw message.
fn range(raw: &[u8], part: &MessagePart<'_>) -> (usize, usize, usize) {
    let start = (part.offset_header as usize).min(raw.len());
    let body = (part.offset_body as usize).clamp(start, raw.len());
    let mut end = (part.offset_end as usize).clamp(body, raw.len());
    // A leaf's body ends on its last content line; the CRLF that introduces the
    // next boundary line is part of the part a client reassembles.
    if !part.is_multipart() {
        if raw[end..].starts_with(b"\r\n") {
            end += 2;
        } else if raw[end..].starts_with(b"\n") {
            end += 1;
        }
    }
    (start, body, end)
}

/// Follows a section path like 1.2 down the parsed tree.
fn resolve<'a>(parsed: &'a Message<'a>, path: &[u32]) -> Option<&'a MessagePart<'a>> {
    let root = parsed.root_part();
    if !root.is_multipart() {
        // A single part message numbers its own body as part 1.
        return match path.first() {
            Some(1) => walk(parsed, root, &path[1..]),
            _ => None,
        };
    }
    walk(parsed, root, path)
}

fn walk<'a>(
    parsed: &'a Message<'a>,
    part: &'a MessagePart<'a>,
    path: &[u32],
) -> Option<&'a MessagePart<'a>> {
    let Some((first, rest)) = path.split_first() else {
        return Some(part);
    };
    let children = part.sub_parts()?;
    let index = (*first as usize).checked_sub(1)?;
    let child = parsed.part(*children.get(index)?)?;
    walk(parsed, child, rest)
}

fn parse_section(spec: &str) -> Option<SectionSpec> {
    let upper = spec.to_ascii_uppercase();
    let mut path = Vec::new();
    let mut kind = Kind::Full;

    if let Some(position) = upper.find("HEADER.FIELDS") {
        for piece in upper[..position].trim().trim_end_matches('.').split('.') {
            if piece.is_empty() {
                continue;
            }
            path.push(piece.parse().ok()?);
        }
        let after = &spec[position + "HEADER.FIELDS".len()..];
        let exclude = after.trim_start().to_ascii_uppercase().starts_with(".NOT");
        let open = after.find('(')?;
        let close = after.find(')')?;
        let fields = after[open + 1..close]
            .split_whitespace()
            .map(|field| field.to_string())
            .collect();
        return Some(SectionSpec {
            path,
            kind: Kind::Fields(fields, exclude),
        });
    }

    let pieces: Vec<&str> = upper.split('.').filter(|piece| !piece.is_empty()).collect();
    let mut index = 0;
    while index < pieces.len() {
        match pieces[index].parse::<u32>() {
            Ok(number) => path.push(number),
            Err(_) => break,
        }
        index += 1;
    }
    if index < pieces.len() {
        kind = match pieces[index] {
            "MIME" => Kind::Mime,
            "HEADER" => Kind::Header,
            "TEXT" => Kind::Text,
            _ => return None,
        };
        if index + 1 != pieces.len() {
            return None;
        }
    }
    Some(SectionSpec { path, kind })
}

/// The header lines a HEADER.FIELDS request selects, ending with a blank line.
pub fn header_fields(raw: &[u8], fields: &[String], exclude: bool) -> Vec<u8> {
    let text = String::from_utf8_lossy(raw);
    let mut out: Vec<u8> = Vec::new();
    let mut keep = false;
    for line in text.split_inclusive('\n') {
        // The blank line ends the header block; nothing after it is a header.
        if line.trim_end_matches(['\r', '\n']).is_empty() {
            break;
        }
        let continuation = line.starts_with(' ') || line.starts_with('\t');
        if !continuation {
            let name = line
                .split(':')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_uppercase();
            keep = fields.iter().any(|field| field.eq_ignore_ascii_case(&name)) != exclude;
        }
        if keep {
            out.extend_from_slice(line.as_bytes());
        }
    }
    if !out.is_empty() && !out.ends_with(b"\n") {
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"\r\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const MESSAGE: &[u8] = b"From: Alice <alice@example.test>\r\n\
To: bob@example.test\r\n\
Subject: Hello\r\n\
Date: Wed, 04 Mar 2026 10:00:00 +0700\r\n\
Message-ID: <abc@example.test>\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=xyz\r\n\
\r\n\
--xyz\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
hello world\r\n\
--xyz\r\n\
Content-Type: text/plain; name=note.txt\r\n\
Content-Disposition: attachment; filename=note.txt\r\n\
\r\n\
file body\r\n\
--xyz--\r\n";

    fn parsed() -> Message<'static> {
        mail_parser::MessageParser::default()
            .parse(MESSAGE)
            .expect("a parsable message")
            .into_owned()
    }

    #[test]
    fn builds_the_envelope() {
        let envelope = envelope(&parsed());
        assert!(
            envelope.starts_with("(\"04-Mar-2026 10:00:00 +0700\" \"Hello\""),
            "{envelope}"
        );
        assert!(
            envelope.contains("((\"Alice\" NIL \"alice\" \"example.test\"))"),
            "{envelope}"
        );
        assert!(envelope.contains("\"<abc@example.test>\""), "{envelope}");
    }

    #[test]
    fn builds_a_multipart_body_structure() {
        let structure = body_structure(&parsed(), MESSAGE);
        assert!(structure.starts_with("((\"TEXT\" \"PLAIN\""), "{structure}");
        assert!(
            structure.contains("\"ALTERNATIVE\"") || structure.contains("\"MIXED\""),
            "{structure}"
        );
        assert!(structure.contains("\"note.txt\""), "{structure}");
    }

    #[test]
    fn addresses_every_requested_section() {
        let parsed = parsed();
        assert_eq!(section(&parsed, MESSAGE, "").unwrap(), MESSAGE);
        // A multipart message's TEXT is the whole multipart body, boundary lines
        // and all, which is what a client reassembles from.
        let text = section(&parsed, MESSAGE, "TEXT").unwrap();
        assert!(
            text.starts_with(b"--xyz\r\n"),
            "{:?}",
            String::from_utf8_lossy(&text)
        );
        assert!(text.ends_with(b"--xyz--\r\n"));
        let header = section(&parsed, MESSAGE, "HEADER").unwrap();
        assert!(header.starts_with(b"From: Alice"));
        let first = section(&parsed, MESSAGE, "1").unwrap();
        assert_eq!(first, b"hello world\r\n");
        let mime = section(&parsed, MESSAGE, "1.MIME").unwrap();
        // A part's MIME header block carries its terminating blank line, as
        // BODY[HEADER] does.
        assert_eq!(mime, b"Content-Type: text/plain; charset=utf-8\r\n\r\n");
        let second = section(&parsed, MESSAGE, "2").unwrap();
        assert_eq!(second, b"file body\r\n");
        assert!(section(&parsed, MESSAGE, "9").is_none());
        assert!(section(&parsed, MESSAGE, "nonsense").is_none());
    }

    #[test]
    fn header_fields_select_and_exclude() {
        let fields = header_fields(
            b"From: a@b.test\r\nTo: c@d.test\r\nSubject: Hi\r\n\r\n",
            &["from".to_string()],
            false,
        );
        assert_eq!(fields, b"From: a@b.test\r\n\r\n");
        let fields = header_fields(
            b"From: a@b.test\r\nTo: c@d.test\r\n\r\n",
            &["From".to_string()],
            true,
        );
        assert_eq!(fields, b"To: c@d.test\r\n\r\n");
    }

    #[test]
    fn counts_lines_the_way_a_client_expects() {
        assert_eq!(line_count(b""), 0);
        assert_eq!(line_count(b"one\r\n"), 1);
        assert_eq!(line_count(b"one\r\ntwo"), 2);
    }
}
