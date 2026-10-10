use anyhow::{Context, Result};

/// Builds a simple message (optional text and HTML parts) with mail-builder.
pub fn simple(
    from: &str,
    to: &[String],
    subject: &str,
    text: Option<&str>,
    html: Option<&str>,
) -> Result<Vec<u8>> {
    let recipients: Vec<&str> = to.iter().map(String::as_str).collect();
    let mut builder = mail_builder::MessageBuilder::new()
        .from(from)
        .to(recipients)
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

/// Builds a message with recipients, threading headers and attachments.
pub fn build(message: &Compose<'_>) -> Result<Vec<u8>> {
    let to: Vec<&str> = message.to.iter().map(String::as_str).collect();
    let mut builder = mail_builder::MessageBuilder::new()
        .from(message.from)
        .to(to)
        .subject(message.subject);

    let cc: Vec<&str> = message.cc.iter().map(String::as_str).collect();
    if !cc.is_empty() {
        builder = builder.cc(cc);
    }
    let bcc: Vec<&str> = message.bcc.iter().map(String::as_str).collect();
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
            message_id: Some("<id@example.test>"),
            in_reply_to: Some("<root@example.test>"),
            references: Some("<root@example.test>"),
        })
        .unwrap();
        let text = String::from_utf8_lossy(&raw);
        assert!(text.contains("Cc:"), "{text}");
        assert!(text.contains("In-Reply-To:"), "{text}");
        assert!(text.contains("note.txt"), "{text}");

        // The stored message round-trips through the body parser.
        let parsed = crate::body::extract(&raw);
        assert_eq!(parsed.text.as_deref(), Some("body"));
        assert_eq!(parsed.attachments.len(), 1);
        assert_eq!(parsed.attachments[0].filename.as_deref(), Some("note.txt"));
        assert_eq!(parsed.attachments[0].bytes, b"hi");
    }
}
