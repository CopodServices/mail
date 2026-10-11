use mail_parser::MimeHeaders;

/// Text, HTML and attachments pulled out of a stored raw message.
///
/// The MTA keeps the raw RFC 5322 bytes in object storage and only indexes
/// metadata, so the reader (the web client) needs the body and attachment list
/// re-derived on demand.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub filename: Option<String>,
    pub content_type: Option<String>,
    /// Content-ID of an inline part, without its angle brackets. A body that
    /// embeds a picture writes it as a cid: reference, and the reader rewrites
    /// that reference to the part's bytes.
    pub content_id: Option<String>,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Body {
    pub text: Option<String>,
    pub html: Option<String>,
    pub attachments: Vec<Attachment>,
    /// To/Cc/Bcc as comma-separated addresses, used to reload a draft.
    pub to: Option<String>,
    pub cc: Option<String>,
    pub bcc: Option<String>,
    /// First http(s) URL from List-Unsubscribe, for the client's unsubscribe action.
    pub unsubscribe_url: Option<String>,
}

/// The addresses a message was blind copied to: what its Bcc header names and its
/// To and Cc headers do not.
///
/// An address a visible header also names was addressed openly, whoever else was
/// blind copied, so it is not one of these.
pub fn blind_recipients(raw: &[u8]) -> Vec<String> {
    let Some(parsed) = mail_parser::MessageParser::default().parse(raw) else {
        return Vec::new();
    };
    let normalized = |value: Option<&mail_parser::Address<'_>>| -> Vec<String> {
        value
            .into_iter()
            .flat_map(|addresses| addresses.iter())
            .filter_map(|entry| entry.address())
            .filter_map(crate::address::parse)
            .map(|parsed| parsed.normalized())
            .collect()
    };
    let mut named = normalized(parsed.to());
    named.extend(normalized(parsed.cc()));
    normalized(parsed.bcc())
        .into_iter()
        .filter(|address| !named.contains(address))
        .collect()
}

/// Parses the raw message with mail-parser and returns its readable parts.
pub fn extract(raw: &[u8]) -> Body {
    let Some(parsed) = mail_parser::MessageParser::default().parse(raw) else {
        return Body::default();
    };

    let attachments = parsed
        .attachments()
        .map(|part| Attachment {
            filename: part.attachment_name().map(str::to_string),
            content_type: part.content_type().map(|content_type| {
                format!(
                    "{}/{}",
                    content_type.ctype(),
                    content_type.subtype().unwrap_or("octet-stream")
                )
            }),
            content_id: content_id(part),
            bytes: part.contents().to_vec(),
        })
        .collect();

    Body {
        text: parsed.body_text(0).map(|text| text.to_string()),
        html: parsed.body_html(0).map(|html| html.to_string()),
        attachments,
        to: address_list(parsed.to()),
        cc: address_list(parsed.cc()),
        bcc: address_list(parsed.bcc()),
        unsubscribe_url: unsubscribe_url(raw),
    }
}

/// The Content-ID a part carries, in the shape an HTML cid: reference uses: the
/// angle brackets belong to the header syntax, not to the value a body writes.
fn content_id(part: &mail_parser::MessagePart<'_>) -> Option<String> {
    let value = part.content_id()?;
    let value = value
        .trim()
        .strip_prefix('<')
        .unwrap_or(value)
        .strip_suffix('>')
        .unwrap_or(value)
        .trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// Pulls the first http(s) URL out of the List-Unsubscribe header, if present.
fn unsubscribe_url(raw: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(raw);
    let header = text
        .lines()
        .find(|line| line.to_ascii_lowercase().starts_with("list-unsubscribe:"))?;
    let start = header.find("http")?;
    let rest = &header[start..];
    let end = rest
        .find(|ch: char| ch == '>' || ch == ',' || ch.is_whitespace())
        .unwrap_or(rest.len());
    let url = rest[..end].trim().to_string();
    // A mailto entry is not something the client can open as a link.
    (!url.is_empty()).then_some(url)
}

/// Joins the addresses of a header into one comma-separated string.
fn address_list(value: Option<&mail_parser::Address<'_>>) -> Option<String> {
    let joined = value?
        .iter()
        .filter_map(|entry| entry.address())
        .collect::<Vec<_>>()
        .join(", ");
    if joined.is_empty() {
        None
    } else {
        Some(joined)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blind_recipients_are_what_no_visible_header_names() {
        let raw = concat!(
            "From: alice@example.test\r\n",
            "To: Bob <bob@example.test>, carol@example.test\r\n",
            "Cc: dave@example.test\r\n",
            "Bcc: carol@example.test, Eve <eve@example.test>\r\n",
            "\r\n",
            "body",
        )
        .as_bytes();
        assert_eq!(
            blind_recipients(raw),
            vec!["eve@example.test".to_string()],
            "carol was addressed openly, so she is not a blind recipient"
        );
    }

    #[test]
    fn extracts_text_and_attachment() {
        let raw = b"From: a@example.test
To: b@example.test
Subject: Hi
MIME-Version: 1.0
Content-Type: multipart/mixed; boundary=xyz

--xyz
Content-Type: text/plain; charset=utf-8

hello world
--xyz
Content-Type: text/plain; name=note.txt
Content-Disposition: attachment; filename=note.txt

file body
--xyz--
";
        let body = extract(raw);
        assert_eq!(body.text.as_deref(), Some("hello world"));
        assert_eq!(body.attachments.len(), 1);
        assert_eq!(body.attachments[0].filename.as_deref(), Some("note.txt"));
        assert_eq!(body.attachments[0].bytes, b"file body");
    }

    #[test]
    fn exposes_the_content_id_of_an_inline_image() {
        let raw = b"From: a@example.test
To: b@example.test
Subject: Hi
MIME-Version: 1.0
Content-Type: multipart/related; boundary=xyz

--xyz
Content-Type: text/html; charset=utf-8

<img src=\"cid:logo@example.test\">
--xyz
Content-Type: image/png
Content-Transfer-Encoding: base64
Content-ID: <logo@example.test>
Content-Disposition: inline

aGk=
--xyz--
";
        let body = extract(raw);
        assert_eq!(body.attachments.len(), 1);
        let image = &body.attachments[0];
        assert_eq!(image.content_id.as_deref(), Some("logo@example.test"));
        assert_eq!(image.content_type.as_deref(), Some("image/png"));
        assert!(
            body.html
                .as_deref()
                .is_some_and(|html| html.contains("cid:logo@example.test"))
        );
    }

    #[test]
    fn reads_the_unsubscribe_link() {
        let raw = b"From: a@example.test
List-Unsubscribe: <mailto:leave@example.test>, <https://example.test/u/1>
Subject: Hi

body
";
        assert_eq!(
            extract(raw).unsubscribe_url.as_deref(),
            Some("https://example.test/u/1")
        );
        assert!(
            extract(
                b"Subject: Hi

body
"
            )
            .unsubscribe_url
            .is_none()
        );
    }

    #[test]
    fn tolerates_unparseable_input() {
        let body = extract(b"not a message");
        assert!(body.text.is_none() && body.attachments.is_empty());
    }
}
