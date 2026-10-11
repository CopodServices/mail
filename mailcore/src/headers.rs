//! Header-level edits applied to a stored message's bytes.

use std::borrow::Cow;

/// The same message with every Bcc header removed.
///
/// Bcc addresses belong in the SMTP envelope, not in the copy a recipient reads:
/// the header names people who are meant to stay unnamed, and the sending side
/// deletes it before the message leaves. The sender's own copy, and only that
/// one, is where it is kept.
///
/// Only the header block is searched, so the word in a body is left alone, and a
/// message that carries no Bcc header comes back borrowed, byte for byte.
pub fn strip_bcc(raw: &[u8]) -> Cow<'_, [u8]> {
    let (block, separator, body) = split_message(raw);
    let mut kept: Vec<u8> = Vec::with_capacity(raw.len());
    let mut dropping = false;
    let mut dropped = false;
    let mut start = 0;
    while start < block.len() {
        let end = match find_bytes(&block[start..], b"\n") {
            Some(offset) => start + offset + 1,
            None => block.len(),
        };
        let line = &block[start..end];
        // A folded continuation belongs to the header above it.
        if !matches!(line.first(), Some(b' ' | b'\t')) {
            dropping = names_bcc(line);
            dropped |= dropping;
        }
        if !dropping {
            kept.extend_from_slice(line);
        }
        start = end;
    }
    if !dropped {
        return Cow::Borrowed(raw);
    }
    // The separator carries the newline that ended the last header line, so a
    // kept block still ending with one would gain a blank line where the header
    // that was dropped used to be.
    if kept.ends_with(b"\r\n") {
        kept.truncate(kept.len() - 2);
    } else if kept.ends_with(b"\n") {
        kept.truncate(kept.len() - 1);
    }
    kept.extend_from_slice(separator);
    kept.extend_from_slice(body);
    Cow::Owned(kept)
}

/// The same message with every Bcc header dropped and one address named.
///
/// This is the copy a blind recipient reads. It names them and nobody else,
/// which is how a blind copy tells its recipient it was one without naming the
/// other blind recipients; a copy that carried the whole list would tell each of
/// them about all the others. Only the sender's own copy keeps the list.
pub fn bcc_for(raw: &[u8], address: &str) -> Vec<u8> {
    let stripped = strip_bcc(raw);
    let (block, separator, body) = split_message(&stripped);
    // The message's own line ending, so the header block stays uniform.
    let ending: &[u8] = if raw.windows(2).any(|window| window == b"\r\n") {
        b"\r\n"
    } else {
        b"\n"
    };
    let mut out = Vec::with_capacity(raw.len() + address.len() + 8);
    out.extend_from_slice(block);
    if !block.is_empty() && !block.ends_with(b"\n") {
        out.extend_from_slice(ending);
    }
    out.extend_from_slice(format!("Bcc: <{address}>").as_bytes());
    if separator.is_empty() {
        // A message with no blank line has no body to be separated from.
        out.extend_from_slice(ending);
    } else {
        // The separator ends this header line and is the blank line after it.
        out.extend_from_slice(separator);
    }
    out.extend_from_slice(body);
    out
}

/// True when a header line starts a Bcc header, whatever its case or spacing.
fn names_bcc(line: &[u8]) -> bool {
    let Some(colon) = line.iter().position(|byte| *byte == b':') else {
        return false;
    };
    trim_ascii(&line[..colon]).eq_ignore_ascii_case(b"bcc")
}

/// A slice with the ASCII whitespace around it removed.
fn trim_ascii(value: &[u8]) -> &[u8] {
    let start = value
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(value.len());
    let end = value
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(start, |position| position + 1);
    &value[start..end]
}

/// The header block, the blank line that ends it, and the body after it.
pub(crate) fn split_message(raw: &[u8]) -> (&[u8], &[u8], &[u8]) {
    for separator in [b"\r\n\r\n".as_slice(), b"\n\n".as_slice()] {
        if let Some(position) = find_bytes(raw, separator) {
            let body = position + separator.len();
            return (&raw[..position], separator, &raw[body..]);
        }
    }
    (raw, b"", b"")
}

/// The offset of the first occurrence of a byte needle in a haystack, if any.
fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::{bcc_for, strip_bcc};
    use std::borrow::Cow;

    #[test]
    fn a_bcc_header_and_its_continuation_go_while_the_rest_is_untouched() {
        let raw = concat!(
            "From: Alice <alice@example.test>\r\n",
            "To: bob@example.test\r\n",
            "Cc: carol@example.test\r\n",
            "Bcc: dave@example.test,\r\n",
            " eve@example.test\r\n",
            "Subject: hi\r\n",
            "\r\n",
            "body line\r\n",
        )
        .as_bytes();
        let stripped = strip_bcc(raw);
        let text = String::from_utf8_lossy(&stripped).to_string();
        assert!(!text.to_lowercase().contains("bcc"), "{text}");
        assert!(!text.contains("dave@example.test"), "{text}");
        assert!(!text.contains("eve@example.test"), "{text}");
        assert_eq!(
            text,
            concat!(
                "From: Alice <alice@example.test>\r\n",
                "To: bob@example.test\r\n",
                "Cc: carol@example.test\r\n",
                "Subject: hi\r\n",
                "\r\n",
                "body line\r\n",
            )
        );
    }

    #[test]
    fn a_message_without_a_bcc_header_comes_back_byte_for_byte() {
        let raw = b"From: a@example.test\r\nSubject: hi\r\n\r\nbody";
        assert!(matches!(strip_bcc(raw), Cow::Borrowed(_)));
    }

    #[test]
    fn the_word_bcc_outside_the_header_block_is_left_alone() {
        let raw = b"Subject: hi\r\n\r\nBcc: not a header\r\n";
        assert!(matches!(strip_bcc(raw), Cow::Borrowed(_)));
    }

    #[test]
    fn another_header_that_mentions_bcc_is_kept() {
        let raw = b"X-Bcc-Note: hi\r\nSubject: bcc\r\n\r\nbody";
        assert!(matches!(strip_bcc(raw), Cow::Borrowed(_)));
    }

    #[test]
    fn an_lf_only_message_loses_its_bcc_header() {
        let raw = b"To: bob@example.test\nBcc: dave@example.test\nSubject: hi\n\nbody\n";
        assert_eq!(
            String::from_utf8_lossy(&strip_bcc(raw)),
            "To: bob@example.test\nSubject: hi\n\nbody\n"
        );
    }

    #[test]
    fn a_message_that_is_only_a_bcc_header_keeps_its_body() {
        let raw = b"Bcc: dave@example.test\r\n\r\nbody";
        assert_eq!(String::from_utf8_lossy(&strip_bcc(raw)), "\r\n\r\nbody");
    }

    #[test]
    fn a_bcc_that_ends_the_header_block_leaves_no_blank_line() {
        let raw = b"To: bob@example.test\r\nBcc: dave@example.test\r\n\r\nbody\r\n";
        assert_eq!(
            String::from_utf8_lossy(&strip_bcc(raw)),
            "To: bob@example.test\r\n\r\nbody\r\n"
        );
    }

    #[test]
    fn a_blind_copy_names_its_own_recipient_alone() {
        let raw = concat!(
            "From: alice@example.test\r\n",
            "To: bob@example.test\r\n",
            "Bcc: carol@example.test, dave@example.test\r\n",
            "Subject: hi\r\n",
            "\r\n",
            "body\r\n",
        )
        .as_bytes();
        let text = String::from_utf8_lossy(&bcc_for(raw, "carol@example.test")).to_string();
        assert_eq!(
            text,
            concat!(
                "From: alice@example.test\r\n",
                "To: bob@example.test\r\n",
                "Subject: hi\r\n",
                "Bcc: <carol@example.test>\r\n",
                "\r\n",
                "body\r\n",
            )
        );
        assert!(!text.contains("dave@example.test"), "{text}");
    }

    #[test]
    fn a_message_without_a_bcc_header_gains_the_one_it_is_given() {
        let raw = b"To: bob@example.test\r\nSubject: hi\r\n\r\nbody";
        let text = String::from_utf8_lossy(&bcc_for(raw, "carol@example.test")).to_string();
        assert!(text.contains("\r\nBcc: <carol@example.test>\r\n"), "{text}");
        assert!(text.ends_with("\r\n\r\nbody"), "{text}");
    }

    #[test]
    fn an_lf_only_message_keeps_its_own_line_ending() {
        let raw = b"To: bob@example.test\nSubject: hi\n\nbody\n";
        let text = String::from_utf8_lossy(&bcc_for(raw, "carol@example.test")).to_string();
        assert_eq!(
            text,
            "To: bob@example.test\nSubject: hi\nBcc: <carol@example.test>\n\nbody\n"
        );
    }
}
