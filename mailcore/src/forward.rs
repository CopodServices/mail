//! Turning a received message into a forwarded copy.
//!
//! The relay accepts a forward as long as the SMTP user may send from the
//! sender's domain, and the message itself is left alone: its own DKIM
//! signature is what authenticates it at the destination. A sender domain the
//! SMTP user is not allowed to use is what the rewrite below answers, by sending
//! the copy as the mailbox instead.

use crate::{address, meta};

/// Header this service writes onto a forwarded copy, so a copy that loops back
/// is delivered but never forwarded again.
pub const MARKER: &str = "X-Copod-Auto-Forwarded";

/// True when the stored message already carries the auto-forward marker. Only
/// the header block is searched, so the string in a body cannot stop a forward.
pub fn already_forwarded(raw: &[u8]) -> bool {
    for line in String::from_utf8_lossy(raw).lines() {
        if line.trim().is_empty() {
            return false;
        }
        if line
            .trim_start()
            .to_ascii_lowercase()
            .starts_with("x-copod-auto-forwarded:")
        {
            return true;
        }
    }
    false
}

/// The forwarded bytes for a sender the relay accepts: the received message with
/// the loop marker on top, and nothing else touched.
pub fn mark(raw: &[u8], address: &str) -> Vec<u8> {
    let mut marked = format!("{MARKER}: {address}\r\n").into_bytes();
    marked.extend_from_slice(raw);
    marked
}

/// True when a relay refusal says the SMTP user may not send from the sender's
/// domain, which is the one refusal the rewrite below can get past.
pub fn sender_domain_refused(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("[acl]") || message.contains("not verified or active")
}

/// Headers of the received copy that must not survive a rewrite: the original
/// sender's identity, and the signatures and traces taken over it. The copy goes
/// out again as the mailbox's own message, so a signature over the old From
/// would not validate and a Received chain would describe a different delivery.
const DROPPED_HEADERS: [&str; 15] = [
    "from",
    "sender",
    "return-path",
    "received",
    "authentication-results",
    "dkim-signature",
    "arc-seal",
    "arc-message-signature",
    "arc-authentication-results",
    // The marker of an earlier pass, so a copy is never marked twice.
    "x-copod-auto-forwarded",
    // The sender's own provider, which no longer says anything about this copy.
    "x-received",
    "x-google-dkim-signature",
    "x-gm-message-state",
    "x-gm-gg",
    "x-gm-features",
];

/// Rewrites a forwarded copy for a sender domain the relay refuses.
///
/// The mailbox's own address becomes the From; the original sender moves to
/// Reply-To so replies still reach them; the marker keeps a copy that loops back
/// from being forwarded again; and the headers that no longer describe the
/// message are dropped. The body is copied byte for byte.
pub fn rewrite_sender(raw: &[u8], address: &str, display_name: Option<&str>) -> Vec<u8> {
    let (headers, body) = split_headers(raw);
    let mut kept: Vec<&str> = Vec::new();
    let mut original_from: Option<String> = None;
    let mut dropping = false;
    for line in headers.lines() {
        // A folded continuation belongs to the header above it.
        if !line.starts_with(' ') && !line.starts_with('\t') {
            let name = line
                .split_once(':')
                .map(|(name, _)| name.trim().to_ascii_lowercase())
                .unwrap_or_default();
            if name == "from" && original_from.is_none() {
                original_from = line
                    .split_once(':')
                    .map(|(_, value)| value.trim().to_string());
            }
            dropping = DROPPED_HEADERS.contains(&name.as_str());
        }
        if !dropping {
            kept.push(line);
        }
    }

    // A From header may carry a display name, which the envelope parser does not
    // read; the metadata extractor returns the bare address.
    let sender = meta::extract(raw)
        .header_from
        .and_then(|value| address::parse(&value))
        .or_else(|| original_from.as_deref().and_then(address::parse));
    let reply_to = sender.as_ref().map(|parsed| parsed.normalized());
    let original_name = original_from
        .as_deref()
        .and_then(|value| value.split('<').next())
        .map(|name| name.trim().trim_matches('"').trim().to_string())
        .filter(|name| !name.is_empty());
    let mailbox_name = display_name
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|name| name.replace('"', ""));
    // The recipient has to see who the message came from even though the mailbox
    // has to own the From, so the original sender is named in the display name.
    let from = match (original_name.or_else(|| reply_to.clone()), mailbox_name) {
        (Some(original), Some(name)) => {
            format!("\"{} via {name}\" <{address}>", original.replace('"', ""))
        }
        (Some(original), None) => {
            format!(
                "\"{} via {address}\" <{address}>",
                original.replace('"', "")
            )
        }
        (None, Some(name)) => format!("\"{name}\" <{address}>"),
        (None, None) => address.to_string(),
    };

    let mut out = Vec::with_capacity(raw.len() + 160);
    out.extend_from_slice(format!("{MARKER}: {address}\r\nFrom: {from}\r\n").as_bytes());
    if let Some(reply_to) = reply_to {
        out.extend_from_slice(format!("Reply-To: {reply_to}\r\n").as_bytes());
    }
    for line in kept {
        out.extend_from_slice(line.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(body);
    out
}

/// Splits a stored message into its header block and the body after it.
fn split_headers(raw: &[u8]) -> (String, &[u8]) {
    if let Some(position) = find_bytes(raw, b"\r\n\r\n") {
        let (headers, rest) = raw.split_at(position);
        return (String::from_utf8_lossy(headers).to_string(), &rest[4..]);
    }
    if let Some(position) = find_bytes(raw, b"\n\n") {
        let (headers, rest) = raw.split_at(position);
        return (String::from_utf8_lossy(headers).to_string(), &rest[2..]);
    }
    (String::from_utf8_lossy(raw).to_string(), b"")
}

/// The offset of the first occurrence of a byte needle in a haystack, if any.
fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::{already_forwarded, mark, rewrite_sender, sender_domain_refused};

    #[test]
    fn a_marked_message_is_recognised_even_past_a_folded_header() {
        let raw = b"From: a@example.test\r\nSubject: hi\r\n\r\nbody\r\n";
        assert!(!already_forwarded(raw));
        assert!(already_forwarded(&mark(raw, "inbox@example.test")));
        assert!(already_forwarded(&rewrite_sender(
            raw,
            "inbox@example.test",
            None
        )));
    }

    #[test]
    fn a_marker_in_the_body_does_not_count() {
        let raw = b"From: a@example.test\r\n\r\nX-Copod-Auto-Forwarded: no\r\n";
        assert!(!already_forwarded(raw));
    }

    #[test]
    fn a_rewrite_is_sent_as_the_mailbox_with_the_sender_as_reply_to() {
        let raw = concat!(
            "Received: from mail.remote.test\r\n",
            "From: Alice <alice@remote.test>\r\n",
            "To: inbox@example.test\r\n",
            "Subject: hi\r\n",
            "DKIM-Signature: v=1; d=remote.test\r\n",
            "\r\n",
            "body line\r\n",
        )
        .as_bytes();
        let rewritten = rewrite_sender(raw, "inbox@example.test", Some("Inbox"));
        let text = String::from_utf8_lossy(&rewritten);
        assert!(
            text.starts_with("X-Copod-Auto-Forwarded: inbox@example.test\r\n"),
            "{text}"
        );
        assert!(text.contains("\r\nFrom: \"Alice via Inbox\" <inbox@example.test>\r\n"));
        assert!(text.contains("\r\nReply-To: alice@remote.test\r\n"));
        // The identity the relay would refuse, and the traces over it, are gone.
        assert!(!text.contains("From: Alice"));
        assert!(!text.to_lowercase().contains("dkim-signature"));
        assert!(!text.to_lowercase().contains("received:"));
        // The rest of the message survives, body included.
        assert!(text.contains("\r\nSubject: hi\r\n"));
        assert!(text.ends_with("body line\r\n"));
    }

    #[test]
    fn a_folded_sender_header_is_dropped_whole() {
        let raw = b"From: Alice\r\n <alice@remote.test>\r\nSubject: hi\r\n\r\nbody";
        let rewritten = rewrite_sender(raw, "inbox@example.test", None);
        let text = String::from_utf8_lossy(&rewritten);
        // The folded original goes away (the address is read out of it first).
        assert!(!text.contains("From: Alice"), "{text}");
        assert!(
            text.contains("\r\nFrom: \"Alice via inbox@example.test\" <inbox@example.test>\r\n"),
            "{text}"
        );
        assert!(!text.contains("<alice@remote.test>"), "{text}");
        assert!(text.contains("\r\nReply-To: alice@remote.test\r\n"));
        assert!(text.contains("\r\nSubject: hi\r\n"));
    }

    #[test]
    fn a_rewrite_replaces_the_marker_it_was_given() {
        let raw = b"From: Alice <alice@remote.test>\r\nSubject: hi\r\n\r\nbody";
        let rewritten =
            rewrite_sender(&mark(raw, "inbox@example.test"), "inbox@example.test", None);
        let text = String::from_utf8_lossy(&rewritten);
        assert_eq!(
            text.matches("X-Copod-Auto-Forwarded:").count(),
            1,
            "a copy must carry the marker once: {text}"
        );
    }

    #[test]
    fn a_rewrite_drops_the_senders_own_provider_headers() {
        let raw = concat!(
            "From: Alice <alice@remote.test>\r\n",
            "X-Received: by 2002:a05 with SMTP id af79\r\n",
            "X-Google-DKIM-Signature: v=1; d=1e100.net\r\n",
            "X-Gm-Message-State: AFq9\r\n",
            "Subject: hi\r\n",
            "\r\n",
            "body",
        )
        .as_bytes();
        let rewritten = rewrite_sender(raw, "inbox@example.test", None);
        let text = String::from_utf8_lossy(&rewritten).to_lowercase();
        for header in [
            "x-received:",
            "x-google-dkim-signature:",
            "x-gm-message-state:",
        ] {
            assert!(!text.contains(header), "{header} survived: {text}");
        }
        assert!(text.contains("subject: hi"));
    }

    #[test]
    fn a_rewrite_of_a_message_without_a_sender_invents_no_reply_to() {
        let raw = b"Subject: hi\r\n\r\nbody";
        let rewritten = rewrite_sender(raw, "inbox@example.test", None);
        let text = String::from_utf8_lossy(&rewritten);
        assert!(text.contains("\r\nFrom: inbox@example.test\r\n"));
        assert!(!text.contains("Reply-To:"));
        assert!(text.ends_with("body"));
    }

    #[test]
    fn a_refused_sender_domain_is_recognised() {
        let jetemail = "relay replied 550 Message rejected: 550 This message was removed as the domain is not verified or active. See more: https://jetemail.com/docs/outbound/smtp-error-codes/550-[ACL]-Block";
        assert!(sender_domain_refused(jetemail));
        assert!(sender_domain_refused(
            "550 [ACL] Blocked. See https://reports.jetsmtp.net/"
        ));
        // Other refusals are not the sender's domain and must not be re-sent.
        assert!(!sender_domain_refused(
            "relay replied 550 5.1.1 no such user"
        ));
        assert!(!sender_domain_refused(
            "relay replied 554 message rejected as spam"
        ));
    }
}
