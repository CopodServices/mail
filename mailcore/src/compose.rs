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
}
