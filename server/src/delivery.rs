use crate::state::AppState;
use anyhow::{Context, Result, bail};
use chrono::Utc;
use entity::{mailbox, message, outbound_job};
use mailcore::forward::{self, already_forwarded};
use mailcore::policy::AuthOutcome;
use mailcore::{address, meta, repo, srs, store};
use sea_orm::Set;
use uuid::Uuid;

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
    if inbound && let Err(err) = forward_inbound(state, mailbox, envelope_from, raw).await {
        tracing::warn!(error = %err, mailbox = %mailbox.address, "failed to queue the auto-forward");
    }

    Ok(inserted.id)
}

/// Queues one copy of an arriving message to the mailbox's auto-forward address.
///
/// The envelope sender is rewritten with SRS on the mailbox's own domain, which
/// is what the relay asks of forwarded mail, and the message goes out as it
/// arrived so its own DKIM signature still authenticates it. A relay that
/// refuses the sender's domain is answered by the queue, which resends the copy
/// as the mailbox itself. Returns the queued job id, or None when there is
/// nothing to forward.
pub async fn forward_inbound(
    state: &AppState,
    mailbox: &mailbox::Model,
    envelope_from: &str,
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

    // The rewritten sender has to live on a domain the relay sends for, so the
    // mailbox's own domain is used. A sender that cannot be rewritten, the null
    // sender of a bounce, falls back to the mailbox itself.
    let forward_domain = mailbox
        .address
        .rsplit_once('@')
        .map(|(_, domain)| domain)
        .unwrap_or(&mailbox.address);
    let srs_key = srs::key(&state.config.session_secret);
    let envelope = srs::encode(
        envelope_from,
        forward_domain,
        &srs_key,
        Utc::now().timestamp(),
    )
    .unwrap_or_else(|| mailbox.address.clone());

    // The message goes out byte for byte, so the sender's DKIM survives.
    let forwarded = forward::mark(raw, &mailbox.address);
    let job = enqueue_remote(
        state,
        Some(mailbox.id),
        &envelope,
        std::slice::from_ref(&target),
        &forwarded,
        None,
    )
    .await?;
    Ok(Some(job))
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
    use super::exceeds_quota;

    #[test]
    fn a_zero_quota_is_unlimited() {
        assert!(!exceeds_quota(10_000, 10_000, 0));
    }

    #[test]
    fn a_message_that_fits_is_allowed_and_one_byte_over_is_not() {
        assert!(!exceeds_quota(100, 50, 150));
        assert!(exceeds_quota(100, 51, 150));
    }
}
