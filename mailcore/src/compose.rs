use anyhow::{Context, Result};
use mail_builder::headers::address::Address;

/// The header values the composer was given, as RFC5322 addresses.
///
/// A value is a bare address or carries a display name (`"Info" <info@x>`);
/// mail-builder would take the whole string as one address and wrap it in
/// another pair of angle brackets, so the name and the address come out first.
fn to_addresses(values: &[String]) -> Vec<Address<'_>> {
    values
        .iter()
        .map(|value| {
            let (name, email) = crate::address::split_display_name(value);
            Address::new_address(name, email)
        })
        .collect()
}

/// One header value as an RFC5322 address, display name kept.
fn single_address(value: &str) -> Address<'_> {
    let (name, email) = crate::address::split_display_name(value);
    Address::new_address(name, email)
}

/// Builds a simple message (optional text and HTML parts) with mail-builder.
pub fn simple(
    from: &str,
    to: &[String],
    subject: &str,
    text: Option<&str>,
    html: Option<&str>,
) -> Result<Vec<u8>> {
    let mut builder = mail_builder::MessageBuilder::new()
        .from(single_address(from))
        .to(to_addresses(to))
        .subject(subject);

    if let Some(text) = text {
        builder = builder.text_body(text);
    }
    if let Some(html) = html {
        builder = builder.html_body(html);
    }

    builder
        .write_to_vec()
        .context("failed to build the message")
}

/// A binary attachment to include in a composed message.
pub struct Attachment<'a> {
    pub filename: &'a str,
    pub content_type: &'a str,
    pub bytes: &'a [u8],
}

/// Everything the composer needs to build a message or a stored draft.
pub struct Compose<'a> {
    pub from: &'a str,
    pub to: &'a [String],
    pub cc: &'a [String],
    pub bcc: &'a [String],
    pub subject: &'a str,
    pub text: Option<&'a str>,
    pub html: Option<&'a str>,
    pub attachments: &'a [Attachment<'a>],
    pub message_id: Option<&'a str>,
    pub in_reply_to: Option<&'a str>,
    pub references: Option<&'a str>,
}

/// A Message-ID for a message this service composes.
///
/// mail-builder would build one from the local hostname, which in a container is
/// the pod name: it is not a domain, it says where the mail was composed, and it
/// changes with every rollout. The configured hostname is what belongs there.
///
/// The value is bare, the way mail-builder and the parser both want it: the
/// builder wraps the header in the angle brackets itself.
pub fn message_id(hostname: &str) -> String {
    format!("{}@{}", uuid::Uuid::now_v7(), hostname)
}

/// Builds a message with recipients, threading headers and attachments.
pub fn build(message: &Compose<'_>) -> Result<Vec<u8>> {
    let mut builder = mail_builder::MessageBuilder::new()
        .from(single_address(message.from))
        .to(to_addresses(message.to))
        .subject(message.subject);

    let cc = to_addresses(message.cc);
    if !cc.is_empty() {
        builder = builder.cc(cc);
    }
    let bcc = to_addresses(message.bcc);
    if !bcc.is_empty() {
        builder = builder.bcc(bcc);
    }
    if let Some(message_id) = message.message_id {
        builder = builder.message_id(message_id);
    }
    if let Some(in_reply_to) = message.in_reply_to {
        builder = builder.in_reply_to(in_reply_to);
    }
    if let Some(references) = message.references {
        builder = builder.references(references);
    }
    if let Some(text) = message.text {
        builder = builder.text_body(text);
    }
    if let Some(html) = message.html {
        builder = builder.html_body(html);
    }
    for attachment in message.attachments {
        builder = builder.attachment(
            attachment.content_type,
            attachment.filename,
            attachment.bytes.to_vec(),
        );
    }

    builder
        .write_to_vec()
        .context("failed to build the message")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_composed_message_ids_itself_on_the_configured_hostname() {
        let id = message_id("mx.example.test");
        assert!(id.ends_with("@mx.example.test"), "{id}");
        assert!(!id.contains('<'), "mail-builder adds the brackets: {id}");
        assert_ne!(id, message_id("mx.example.test"), "two messages, two ids");
    }

    #[test]
    fn keeps_a_display_name_in_the_from_header() {
        let raw = build(&Compose {
            from: "\"Info\" <info@example.test>",
            to: &["bob@example.test".to_string()],
            cc: &[],
            bcc: &[],
            subject: "Hello",
            text: Some("body"),
            html: None,
            attachments: &[],
            message_id: None,
            in_reply_to: None,
            references: None,
        })
        .unwrap();
        let text = String::from_utf8_lossy(&raw).to_string();
        // The name must be a name, not part of an angle-bracketed address.
        assert!(!text.contains("<<"), "{text}");
        assert!(
            text.contains("From: \"Info\" <info@example.test>"),
            "{text}"
        );
        assert_eq!(
            crate::meta::extract(&raw).header_from.as_deref(),
            Some("info@example.test"),
            "{text}"
        );
    }

    #[test]
    fn builds_a_message_with_a_subject_and_body() {
        let raw = simple(
            "alice@example.test",
            &["bob@example.test".to_string()],
            "Reset your password",
            Some("Use this link"),
            None,
        )
        .unwrap();
        let text = String::from_utf8_lossy(&raw);
        assert!(text.contains("Subject: Reset your password"));
        assert!(text.contains("To: <bob@example.test>"));
        assert!(text.contains("Use this link"));
    }

    #[test]
    fn builds_a_draft_with_recipients_attachments_and_threading() {
        let raw = build(&Compose {
            from: "alice@example.test",
            to: &["bob@example.test".to_string()],
            cc: &["carol@example.test".to_string()],
            bcc: &[],
            subject: "With attachment",
            text: Some("body"),
            html: None,
            attachments: &[Attachment {
                filename: "note.txt",
                content_type: "text/plain",
                bytes: b"hi",
            }],
            message_id: Some("id@example.test"),
            in_reply_to: Some("root@example.test"),
            references: Some("root@example.test"),
        })
        .unwrap();
        let text = String::from_utf8_lossy(&raw);
        assert!(text.contains("Cc:"), "{text}");
        assert!(text.contains("Message-ID: <id@example.test>"), "{text}");
        assert!(text.contains("In-Reply-To: <root@example.test>"), "{text}");
        assert!(text.contains("References: <root@example.test>"), "{text}");
        assert!(text.contains("note.txt"), "{text}");

        // The stored message round-trips through the body parser.
        let parsed = crate::body::extract(&raw);
        assert_eq!(parsed.text.as_deref(), Some("body"));
        assert_eq!(parsed.attachments.len(), 1);
        assert_eq!(parsed.attachments[0].filename.as_deref(), Some("note.txt"));
        assert_eq!(parsed.attachments[0].bytes, b"hi");
    }
}
