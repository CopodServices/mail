use crate::state::AppState;
use anyhow::{Context, Result, bail};
use chrono::Utc;
use entity::{mailbox, message, outbound_job};
use mailcore::policy::AuthOutcome;
use mailcore::{address, meta, repo, store};
use sea_orm::Set;
use uuid::Uuid;

/// Header this service writes onto a forwarded copy so a forward that loops
/// back is delivered but never forwarded again.
const FORWARD_MARKER: &str = "X-Copod-Auto-Forwarded";

/// Stores a message in a local mailbox and indexes it in PostgreSQL.
///
/// The envelope recipient is this mailbox for mail delivered here, or the
/// addressed recipients for the copy filed in the sender's Sent folder.
pub async fn store_local(
    state: &AppState,
    mailbox: &mailbox::Model,
    direction: message::Direction,
    envelope_from: &str,
    envelope_to: &str,
    raw: &[u8],
    outcome: &AuthOutcome,
) -> Result<Uuid> {
    // Refuse a message that would push the mailbox past its storage quota; a
    // zero quota means unlimited. Checked before the object-store write so an
    // over-quota message leaves nothing behind.
    if mailbox.quota_bytes > 0 {
        let used = repo::mailbox_usage_bytes(&state.db, mailbox.id)
            .await
            .context("failed to read the mailbox usage")?;
        if exceeds_quota(used, raw.len() as i64, mailbox.quota_bytes) {
            bail!("mailbox {} is over its storage quota", mailbox.address);
        }
    }

    let id = Uuid::now_v7();
    let now = Utc::now();
    let domain = mailbox
        .address
        .rsplit_once('@')
        .map(|(_, domain)| domain.to_string())
        .unwrap_or_else(|| mailbox.address.clone());
    let key = store::inbound_key(&domain, &mailbox.local_part, &id, now);

    state
        .store
        .put(&key, raw.to_vec())
        .await
        .context("failed to write the message to object storage")?;

    let metadata = meta::extract(raw);
    // Kept for the live-update frame after the row is inserted.
    let sender = metadata
        .header_from
        .clone()
        .unwrap_or_else(|| envelope_from.to_string());
    let status = match outcome.action {
        mailcore::policy::AuthAction::Quarantine => message::Status::Quarantined,
        _ => message::Status::Stored,
    };
    // A copy filed in the sender's own Sent folder is not an arrival, so only an
    // inbound message tells connected clients that new mail landed.
    let inbound = matches!(direction, message::Direction::Inbound);

    let inserted = repo::insert_message(
        &state.db,
        message::ActiveModel {
            id: Set(id),
            mailbox_id: Set(Some(mailbox.id)),
            direction: Set(direction),
            status: Set(status),
            envelope_from: Set(envelope_from.to_string()),
            envelope_to: Set(envelope_to.to_string()),
            message_id: Set(metadata.message_id),
            subject: Set(metadata.subject),
            header_from: Set(metadata.header_from),
            in_reply_to: Set(metadata.in_reply_to),
            references_header: Set(metadata.references),
            size_bytes: Set(raw.len() as i64),
            storage_key: Set(key),
            spf_result: Set(outcome.spf.clone()),
            dkim_result: Set(outcome.dkim.clone()),
            dmarc_result: Set(outcome.dmarc.clone()),
            dmarc_policy: Set(outcome.dmarc_policy.clone()),
            received_at: Set(now),
        },
    )
    .await?;

    // Tell connected clients that mail arrived for this mailbox.
    if inbound {
        state.publish(
            serde_json::json!({
                "type": "new_message",
                "messageId": inserted.id.to_string(),
                "mailboxId": mailbox.id.to_string(),
                "from": sender,
                "fromName": serde_json::Value::Null,
                "subject": inserted.subject,
            })
            .to_string(),
        );
    }

    // Auto-forward: a copy of an arriving message goes to the mailbox's
    // configured address. Losing the forward must not lose the delivery, so a
    // failure is logged and the stored copy is left alone.
    if inbound && let Err(err) = forward_inbound(state, mailbox, raw).await {
        tracing::warn!(error = %err, mailbox = %mailbox.address, "failed to queue the auto-forward");
    }

    Ok(inserted.id)
}

/// Queues one copy of an arriving message to the mailbox's auto-forward address.
///
/// The relay refuses a message whose From domain it has not verified, so the
/// mailbox's own address becomes the From and the original sender moves to
/// Reply-To. The message is otherwise copied through, without the headers that
/// no longer describe it. Returns the queued job id, or None when there is
/// nothing to forward.
pub async fn forward_inbound(
    state: &AppState,
    mailbox: &mailbox::Model,
    raw: &[u8],
) -> Result<Option<Uuid>> {
    let Some(target) = mailbox
        .forward_to
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    let Some(parsed) = address::parse(target) else {
        bail!(
            "mailbox {} has an invalid auto-forward address",
            mailbox.address
        );
    };
    let target = parsed.normalized();
    // Forwarding to itself, or a copy that already came through here, would
    // loop; deliver it and stop.
    if target == mailbox.address || already_forwarded(raw) {
        return Ok(None);
    }

    let forwarded = rewrite_for_forward(raw, &mailbox.address, mailbox.display_name.as_deref());
    let job = enqueue_remote(
        state,
        Some(mailbox.id),
        &mailbox.address,
        std::slice::from_ref(&target),
        &forwarded,
        None,
    )
    .await?;
    Ok(Some(job))
}

/// True when the stored message already carries the auto-forward marker. Only
/// the header block is searched, so the string in a body cannot stop a forward.
fn already_forwarded(raw: &[u8]) -> bool {
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

/// Headers of the received copy that must not survive a forward: the original
/// sender's identity, and the signatures and traces taken over it. The copy goes
/// out again as the mailbox's own message, so a signature over the old From
/// would not validate and a Received chain would describe a different delivery.
const DROPPED_FORWARD_HEADERS: [&str; 9] = [
    "from",
    "sender",
    "return-path",
    "received",
    "authentication-results",
    "dkim-signature",
    "arc-seal",
    "arc-message-signature",
    "arc-authentication-results",
];

/// Rewrites a received message for an auto-forward.
///
/// The mailbox's own address becomes the From, because the relay refuses a
/// message whose From domain it has not verified; the original sender moves to
/// Reply-To so replying still reaches them; the marker keeps a copy that loops
/// back from being forwarded again; and the headers that no longer describe the
/// message are dropped. The body is copied byte for byte.
fn rewrite_for_forward(raw: &[u8], address: &str, display_name: Option<&str>) -> Vec<u8> {
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
            dropping = DROPPED_FORWARD_HEADERS.contains(&name.as_str());
        }
        if !dropping {
            kept.push(line);
        }
    }

    let from = match display_name.map(str::trim).filter(|name| !name.is_empty()) {
        Some(name) => format!("\"{}\" <{address}>", name.replace('"', "")),
        None => address.to_string(),
    };
    // A From header may carry a display name, which the envelope parser does not
    // read; the metadata extractor returns the bare address.
    let reply_to = meta::extract(raw)
        .header_from
        .and_then(|value| address::parse(&value))
        .or_else(|| original_from.as_deref().and_then(address::parse))
        .map(|parsed| parsed.normalized());

    let mut out = Vec::with_capacity(raw.len() + 160);
    out.extend_from_slice(format!("{FORWARD_MARKER}: {address}\r\nFrom: {from}\r\n").as_bytes());
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

/// Stores an outgoing copy and queues it for relay delivery.
pub async fn enqueue_remote(
    state: &AppState,
    mailbox_id: Option<Uuid>,
    envelope_from: &str,
    recipients: &[String],
    raw: &[u8],
    copy_message_id: Option<Uuid>,
) -> Result<Uuid> {
    enqueue_remote_at(
        state,
        mailbox_id,
        envelope_from,
        recipients,
        raw,
        None,
        SendCopies {
            copy_message_id,
            scheduled_message_id: None,
        },
    )
    .await
}

/// Queues a message for delivery, optionally not before a given time and linked
/// to the Scheduled-folder copy that should move to Sent once it goes out.
pub async fn enqueue_remote_at(
    state: &AppState,
    mailbox_id: Option<Uuid>,
    envelope_from: &str,
    recipients: &[String],
    raw: &[u8],
    not_before: Option<chrono::DateTime<Utc>>,
    copies: SendCopies,
) -> Result<Uuid> {
    let id = Uuid::now_v7();
    let now = Utc::now();
    let key = store::outbound_key(&id, now);

    state
        .store
        .put(&key, raw.to_vec())
        .await
        .context("failed to write the outgoing message to object storage")?;

    let metadata = meta::extract(raw);
    repo::enqueue_job(
        &state.db,
        outbound_job::ActiveModel {
            id: Set(id),
            mailbox_id: Set(mailbox_id),
            envelope_from: Set(envelope_from.to_string()),
            recipients: Set(serde_json::json!(recipients)),
            storage_key: Set(key),
            message_id: Set(metadata.message_id),
            scheduled_message_id: Set(copies.scheduled_message_id),
            copy_message_id: Set(copies.copy_message_id),
            attempts: Set(0),
            next_attempt_at: Set(not_before.unwrap_or(now)),
            status: Set(outbound_job::JobStatus::Pending),
            last_error: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        },
    )
    .await?;

    Ok(id)
}

/// The stored copies a queued send is linked to: the one the sender owns, which
/// reports the job's state, and the one waiting in Scheduled (the same row),
/// which the worker moves to Sent once it goes out.
#[derive(Debug, Clone, Copy, Default)]
pub struct SendCopies {
    pub copy_message_id: Option<Uuid>,
    pub scheduled_message_id: Option<Uuid>,
}

/// True when storing `incoming` more bytes would exceed a positive quota. A
/// zero quota is unlimited.
fn exceeds_quota(used: i64, incoming: i64, quota: i64) -> bool {
    quota > 0 && used + incoming > quota
}

#[cfg(test)]
mod tests {
    use super::{already_forwarded, exceeds_quota, rewrite_for_forward};

    #[test]
    fn a_zero_quota_is_unlimited() {
        assert!(!exceeds_quota(10_000, 10_000, 0));
    }

    #[test]
    fn a_message_that_fits_is_allowed_and_one_byte_over_is_not() {
        assert!(!exceeds_quota(100, 50, 150));
        assert!(exceeds_quota(100, 51, 150));
    }

    #[test]
    fn a_marked_message_is_recognised_even_past_a_folded_header() {
        let raw = b"From: a@example.test\r\nSubject: hi\r\n\r\nbody\r\n";
        assert!(!already_forwarded(raw));
        let forwarded = rewrite_for_forward(raw, "inbox@example.test", None);
        assert!(already_forwarded(&forwarded));
    }

    #[test]
    fn a_forward_is_sent_as_the_mailbox_with_the_sender_as_reply_to() {
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
        let forwarded = rewrite_for_forward(raw, "inbox@example.test", Some("Inbox"));
        let text = String::from_utf8_lossy(&forwarded);
        assert!(
            text.starts_with("X-Copod-Auto-Forwarded: inbox@example.test\r\n"),
            "{text}"
        );
        assert!(text.contains("\r\nFrom: \"Inbox\" <inbox@example.test>\r\n"));
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
        let forwarded = rewrite_for_forward(raw, "inbox@example.test", None);
        let text = String::from_utf8_lossy(&forwarded);
        assert!(text.contains("\r\nFrom: inbox@example.test\r\n"));
        // The folded original goes away (the address is read out of it first).
        assert!(!text.contains("From: Alice"), "{text}");
        assert!(text.contains("\r\nReply-To: alice@remote.test\r\n"));
        assert!(text.contains("\r\nSubject: hi\r\n"));
    }

    #[test]
    fn a_forward_of_a_message_without_a_sender_invents_no_reply_to() {
        let raw = b"Subject: hi\r\n\r\nbody";
        let forwarded = rewrite_for_forward(raw, "inbox@example.test", None);
        let text = String::from_utf8_lossy(&forwarded);
        assert!(text.contains("\r\nFrom: inbox@example.test\r\n"));
        assert!(!text.contains("Reply-To:"));
        assert!(text.ends_with("body"));
    }

    #[test]
    fn a_marker_in_the_body_does_not_count() {
        let raw = b"From: a@example.test\r\n\r\nX-Copod-Auto-Forwarded: no\r\n";
        assert!(!already_forwarded(raw));
    }
}
