#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Metadata {
    pub message_id: Option<String>,
    pub subject: Option<String>,
    pub header_from: Option<String>,
}

/// Pulls the display metadata out of the raw message with mail-parser.
pub fn extract(raw: &[u8]) -> Metadata {
    let Some(parsed) = mail_parser::MessageParser::default().parse(raw) else {
        return Metadata::default();
    };
    Metadata {
        message_id: parsed.message_id().map(str::to_string),
        subject: parsed.subject().map(str::to_string),
        header_from: parsed
            .from()
            .and_then(|addresses| addresses.first())
            .and_then(|address| address.address())
            .map(str::to_string),
    }
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
    fn tolerates_unparseable_input() {
        assert_eq!(extract(b"no headers here"), Metadata::default());
    }
}
