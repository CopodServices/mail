#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Metadata {
    pub message_id: Option<String>,
    pub subject: Option<String>,
    pub header_from: Option<String>,
    /// In-Reply-To header, used to place a reply in its conversation.
    pub in_reply_to: Option<String>,
    /// References header, the full ancestor chain of a conversation.
    pub references: Option<String>,
}

/// Parses a raw message once, so several consumers can share the parsed form.
pub fn parse(raw: &[u8]) -> Option<mail_parser::Message<'_>> {
    mail_parser::MessageParser::default().parse(raw)
}

/// Pulls the display metadata out of the raw message with mail-parser.
pub fn extract(raw: &[u8]) -> Metadata {
    parse(raw).as_ref().map(from_message).unwrap_or_default()
}

/// Pulls the display metadata out of an already parsed message.
pub fn from_message(parsed: &mail_parser::Message<'_>) -> Metadata {
    Metadata {
        message_id: parsed.message_id().map(str::to_string),
        subject: parsed.subject().map(str::to_string),
        header_from: parsed
            .from()
            .and_then(|addresses| addresses.first())
            .and_then(|address| address.address())
            .map(str::to_string),
        in_reply_to: header_ids(parsed.in_reply_to()),
        references: header_ids(parsed.references()),
    }
}

/// Message-id headers can hold one id or a list; join them into one string.
fn header_ids(value: &mail_parser::HeaderValue<'_>) -> Option<String> {
    if let Some(list) = value.as_text_list() {
        let joined = list
            .iter()
            .map(|item| item.as_ref())
            .collect::<Vec<_>>()
            .join(" ");
        if !joined.trim().is_empty() {
            return Some(joined);
        }
    }
    value
        .as_text()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_subject_sender_and_id() {
        let raw = b"From: Alice <alice@example.test>\r\nTo: bob@example.test\r\nMessage-ID: <abc@example.test>\r\nSubject: Hello\r\n\r\nbody\r\n";
        let metadata = extract(raw);
        assert_eq!(metadata.message_id.as_deref(), Some("abc@example.test"));
        assert_eq!(metadata.subject.as_deref(), Some("Hello"));
        assert_eq!(metadata.header_from.as_deref(), Some("alice@example.test"));
    }

    #[test]
    fn extracts_threading_headers() {
        let raw = b"Message-ID: <reply@example.test>
In-Reply-To: <root@example.test>
References: <root@example.test> <mid@example.test>
Subject: Re: Hello

body
";
        let metadata = extract(raw);
        // mail-parser normalizes message ids: no angle brackets, space separated.
        assert_eq!(metadata.in_reply_to.as_deref(), Some("root@example.test"));
        let references = metadata.references.unwrap_or_default();
        assert!(references.contains("root@example.test"), "{references}");
        assert!(references.contains("mid@example.test"), "{references}");
    }

    #[test]
    fn tolerates_unparseable_input() {
        assert_eq!(extract(b"no headers here"), Metadata::default());
    }
}
