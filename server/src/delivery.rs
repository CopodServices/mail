use crate::state::AppState;
use anyhow::{Context, Result};
use chrono::Utc;
use entity::{mailbox, message, outbound_job};
use mailcore::policy::AuthOutcome;
use mailcore::{meta, repo, store};
use sea_orm::Set;
use uuid::Uuid;

/// Stores a message in a local mailbox and indexes it in PostgreSQL.
pub async fn store_local(
    state: &AppState,
    mailbox: &mailbox::Model,
    direction: message::Direction,
    envelope_from: &str,
    raw: &[u8],
    outcome: &AuthOutcome,
) -> Result<Uuid> {
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
    let status = match outcome.action {
        mailcore::policy::AuthAction::Quarantine => message::Status::Quarantined,
        _ => message::Status::Stored,
    };

    let inserted = repo::insert_message(
        &state.db,
        message::ActiveModel {
            id: Set(id),
            mailbox_id: Set(Some(mailbox.id)),
            direction: Set(direction),
            status: Set(status),
            envelope_from: Set(envelope_from.to_string()),
            envelope_to: Set(mailbox.address.clone()),
            message_id: Set(metadata.message_id),
            subject: Set(metadata.subject),
            header_from: Set(metadata.header_from),
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
            attempts: Set(0),
            next_attempt_at: Set(now),
            status: Set(outbound_job::JobStatus::Pending),
            last_error: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        },
    )
    .await?;

    Ok(id)
}
