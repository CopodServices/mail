use crate::state::AppState;
use anyhow::{Context, Result, bail};
use chrono::Utc;
use entity::{mailbox, message, outbound_job};
use mailcore::policy::AuthOutcome;
use mailcore::{meta, repo, store};
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

    Ok(inserted.id)
}

/// Stores an outgoing copy and queues it for relay delivery.
pub async fn enqueue_remote(
    state: &AppState,
    mailbox_id: Option<Uuid>,
    envelope_from: &str,
    recipients: &[String],
    raw: &[u8],
) -> Result<Uuid> {
    enqueue_remote_at(
        state,
        mailbox_id,
        envelope_from,
        recipients,
        raw,
        None,
        None,
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
    scheduled_message_id: Option<Uuid>,
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
            scheduled_message_id: Set(scheduled_message_id),
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
