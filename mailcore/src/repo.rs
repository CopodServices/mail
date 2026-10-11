use anyhow::{Context, Result, anyhow};
use chrono::Utc;
use entity::{domain, mailbox, message, outbound_job};
use sea_orm::sea_query::{Alias, Expr};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, DatabaseConnection, EntityTrait, ExprTrait,
    FromQueryResult, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Set,
};
use std::collections::HashMap;
use uuid::Uuid;

pub async fn find_domain(db: &DatabaseConnection, name: &str) -> Result<Option<domain::Model>> {
    Ok(domain::Entity::find()
        .filter(domain::Column::Name.eq(name.trim().to_ascii_lowercase()))
        .one(db)
        .await?)
}

pub async fn list_domains(db: &DatabaseConnection) -> Result<Vec<domain::Model>> {
    Ok(domain::Entity::find()
        .order_by_asc(domain::Column::Name)
        .all(db)
        .await?)
}

/// Creates the domain when it does not exist yet.
pub async fn ensure_domain(db: &DatabaseConnection, name: &str) -> Result<domain::Model> {
    let name = name.trim().to_ascii_lowercase();
    if let Some(existing) = find_domain(db, &name).await? {
        return Ok(existing);
    }

    let now = Utc::now();
    let model = domain::ActiveModel {
        id: Set(Uuid::now_v7()),
        name: Set(name),
        active: Set(true),
        created_at: Set(now),
        updated_at: Set(now),
    };
    Ok(model.insert(db).await?)
}

/// Finds a mailbox only when both it and its domain are active.
pub async fn find_active_mailbox(
    db: &DatabaseConnection,
    address: &str,
) -> Result<Option<mailbox::Model>> {
    let Some(mailbox) = find_mailbox(db, address).await? else {
        return Ok(None);
    };
    if !mailbox.active {
        return Ok(None);
    }
    let domain = domain::Entity::find_by_id(mailbox.domain_id)
        .one(db)
        .await?;
    Ok(domain.filter(|domain| domain.active).map(|_| mailbox))
}

/// Lists the mailboxes whose address belongs to a domain.
pub async fn list_mailboxes_in_domain(
    db: &DatabaseConnection,
    name: &str,
) -> Result<Vec<mailbox::Model>> {
    let Some(domain) = find_domain(db, name).await? else {
        return Ok(Vec::new());
    };
    Ok(mailbox::Entity::find()
        .filter(mailbox::Column::DomainId.eq(domain.id))
        .order_by_asc(mailbox::Column::Address)
        .all(db)
        .await?)
}

/// Applies the operator-supplied mailbox fields; absent fields are left alone.
/// The mutable fields of a mailbox an update may set. A top-level None leaves a
/// field alone; the nested Option sets or clears a nullable one.
#[derive(Debug, Default)]
pub struct MailboxUpdate {
    pub password: Option<String>,
    pub signature: Option<Option<String>>,
    pub display_name: Option<Option<String>>,
    pub forward_to: Option<Option<String>>,
    pub quota_bytes: Option<i64>,
    pub active: Option<bool>,
}

pub async fn update_mailbox(
    db: &DatabaseConnection,
    address: &str,
    update: MailboxUpdate,
) -> Result<Option<mailbox::Model>> {
    let Some(existing) = find_mailbox(db, address).await? else {
        return Ok(None);
    };
    let mut model: mailbox::ActiveModel = existing.into();
    if let Some(password) = update.password.as_deref() {
        model.password_hash = Set(crate::auth::hash_password(password)?);
    }
    if let Some(signature) = update.signature {
        model.signature = Set(signature);
    }
    if let Some(display_name) = update.display_name {
        model.display_name = Set(display_name);
    }
    if let Some(forward_to) = update.forward_to {
        model.forward_to = Set(forward_to);
    }
    if let Some(quota_bytes) = update.quota_bytes {
        model.quota_bytes = Set(quota_bytes);
    }
    if let Some(active) = update.active {
        model.active = Set(active);
    }
    model.updated_at = Set(Utc::now());
    Ok(Some(model.update(db).await?))
}

pub async fn set_domain_active(
    db: &DatabaseConnection,
    name: &str,
    active: bool,
) -> Result<Option<domain::Model>> {
    let Some(existing) = find_domain(db, name).await? else {
        return Ok(None);
    };
    let mut model: domain::ActiveModel = existing.into();
    model.active = Set(active);
    model.updated_at = Set(Utc::now());
    Ok(Some(model.update(db).await?))
}

pub async fn delete_domain(db: &DatabaseConnection, name: &str) -> Result<bool> {
    let result = domain::Entity::delete_many()
        .filter(domain::Column::Name.eq(name.trim().to_ascii_lowercase()))
        .exec(db)
        .await?;
    Ok(result.rows_affected > 0)
}

pub async fn find_mailbox(
    db: &DatabaseConnection,
    address: &str,
) -> Result<Option<mailbox::Model>> {
    Ok(mailbox::Entity::find()
        .filter(mailbox::Column::Address.eq(address.trim().to_ascii_lowercase()))
        .one(db)
        .await?)
}

pub async fn list_mailboxes(db: &DatabaseConnection) -> Result<Vec<mailbox::Model>> {
    Ok(mailbox::Entity::find()
        .order_by_asc(mailbox::Column::Address)
        .all(db)
        .await?)
}

/// Bytes stored for one mailbox: the sum of its messages' sizes.
pub async fn mailbox_usage_bytes(db: &DatabaseConnection, mailbox_id: Uuid) -> Result<i64> {
    #[derive(FromQueryResult)]
    struct Usage {
        used_bytes: Option<i64>,
    }

    // Postgres returns SUM(bigint) as numeric, so cast to bigint for an i64 read.
    let usage: Option<Usage> = message::Entity::find()
        .filter(message::Column::MailboxId.eq(mailbox_id))
        .select_only()
        .column_as(sum_size_bytes(), "used_bytes")
        .into_model::<Usage>()
        .one(db)
        .await?;
    Ok(usage.and_then(|usage| usage.used_bytes).unwrap_or(0))
}

/// Bytes stored for every mailbox that holds at least one message.
pub async fn mailbox_usage_totals(db: &DatabaseConnection) -> Result<HashMap<Uuid, i64>> {
    #[derive(FromQueryResult)]
    struct Usage {
        mailbox_id: Uuid,
        used_bytes: Option<i64>,
    }

    let rows: Vec<Usage> = message::Entity::find()
        .filter(message::Column::MailboxId.is_not_null())
        .select_only()
        .column(message::Column::MailboxId)
        .column_as(sum_size_bytes(), "used_bytes")
        .group_by(message::Column::MailboxId)
        .into_model::<Usage>()
        .all(db)
        .await?;
    Ok(rows
        .into_iter()
        .map(|row| (row.mailbox_id, row.used_bytes.unwrap_or(0)))
        .collect())
}

/// `SUM(size_bytes)::bigint`, the stored size of a group of messages.
fn sum_size_bytes() -> sea_orm::sea_query::SimpleExpr {
    Expr::col(message::Column::SizeBytes)
        .sum()
        .cast_as(Alias::new("bigint"))
}

pub async fn create_mailbox(
    db: &DatabaseConnection,
    address: &str,
    password: &str,
) -> Result<mailbox::Model> {
    let parsed = crate::address::parse(address)
        .ok_or_else(|| anyhow!("invalid email address: {address}"))?;
    let normalized = parsed.normalized();
    if find_mailbox(db, &normalized).await?.is_some() {
        return Err(anyhow!("mailbox {normalized} already exists"));
    }

    let parent = ensure_domain(db, &parsed.domain).await?;
    let now = Utc::now();
    let model = mailbox::ActiveModel {
        id: Set(Uuid::now_v7()),
        address: Set(normalized),
        domain_id: Set(parent.id),
        local_part: Set(parsed.local_part),
        password_hash: Set(crate::auth::hash_password(password)?),
        signature: Set(None),
        display_name: Set(None),
        forward_to: Set(None),
        quota_bytes: Set(0),
        active: Set(true),
        created_at: Set(now),
        updated_at: Set(now),
    };
    Ok(model.insert(db).await?)
}

pub async fn delete_mailbox(db: &DatabaseConnection, address: &str) -> Result<bool> {
    let result = mailbox::Entity::delete_many()
        .filter(mailbox::Column::Address.eq(address.trim().to_ascii_lowercase()))
        .exec(db)
        .await?;
    Ok(result.rows_affected > 0)
}

pub async fn insert_message(
    db: &DatabaseConnection,
    model: message::ActiveModel,
) -> Result<message::Model> {
    Ok(model.insert(db).await?)
}

pub async fn list_messages(
    db: &DatabaseConnection,
    mailbox_address: Option<&str>,
    limit: u64,
) -> Result<Vec<message::Model>> {
    let mut query = message::Entity::find()
        .order_by_desc(message::Column::ReceivedAt)
        .limit(limit.clamp(1, 500));

    if let Some(address) = mailbox_address {
        let mailbox_id = find_mailbox(db, address)
            .await?
            .map(|mailbox| mailbox.id)
            .ok_or_else(|| anyhow!("unknown mailbox: {address}"))?;
        query = query.filter(message::Column::MailboxId.eq(mailbox_id));
    }

    Ok(query.all(db).await?)
}

pub async fn get_message(db: &DatabaseConnection, id: Uuid) -> Result<Option<message::Model>> {
    Ok(message::Entity::find_by_id(id).one(db).await?)
}

/// The stored message carrying a given RFC Message-ID in one mailbox. Threads
/// walk their ancestors with this, so it stays a point lookup.
pub async fn find_message_by_message_id(
    db: &DatabaseConnection,
    mailbox_id: Uuid,
    message_id: &str,
) -> Result<Option<message::Model>> {
    Ok(message::Entity::find()
        .filter(message::Column::MailboxId.eq(mailbox_id))
        .filter(message::Column::MessageId.eq(message_id))
        .one(db)
        .await?)
}

/// The candidates of a conversation: the root itself, anything that replies to
/// it, and anything whose References header mentions it. The caller re-checks
/// membership, because the References match is a substring one.
pub async fn thread_candidates(
    db: &DatabaseConnection,
    mailbox_id: Uuid,
    root: &str,
) -> Result<Vec<message::Model>> {
    Ok(message::Entity::find()
        .filter(message::Column::MailboxId.eq(mailbox_id))
        .filter(
            Condition::any()
                .add(message::Column::MessageId.eq(root))
                .add(message::Column::InReplyTo.eq(root))
                .add(message::Column::ReferencesHeader.like(format!("%{root}%"))),
        )
        .order_by_asc(message::Column::ReceivedAt)
        .all(db)
        .await?)
}

pub async fn delete_message(db: &DatabaseConnection, id: Uuid) -> Result<Option<message::Model>> {
    let Some(existing) = get_message(db, id).await? else {
        return Ok(None);
    };
    message::Entity::delete_by_id(id).exec(db).await?;
    Ok(Some(existing))
}

/// Moves a message to another folder, keeping its other reader state.
pub async fn set_message_folder(
    db: &DatabaseConnection,
    message_id: Uuid,
    folder: &str,
) -> Result<()> {
    let existing = entity::message_flag::Entity::find_by_id(message_id)
        .one(db)
        .await?;
    let model = entity::message_flag::ActiveModel {
        message_id: Set(message_id),
        read: Set(existing.as_ref().map(|flag| flag.read).unwrap_or(true)),
        starred: Set(existing.as_ref().map(|flag| flag.starred).unwrap_or(false)),
        folder: Set(folder.to_string()),
        snoozed_until: Set(existing.as_ref().and_then(|flag| flag.snoozed_until)),
        updated_at: Set(Utc::now()),
    };
    if existing.is_some() {
        model.update(db).await?;
    } else {
        model.insert(db).await?;
    }
    Ok(())
}

pub async fn enqueue_job(
    db: &DatabaseConnection,
    model: outbound_job::ActiveModel,
) -> Result<outbound_job::Model> {
    Ok(model.insert(db).await?)
}

/// Atomically claims due jobs by flipping them to sending.
pub async fn claim_due_jobs(
    db: &DatabaseConnection,
    limit: u64,
) -> Result<Vec<outbound_job::Model>> {
    let due = outbound_job::Entity::find()
        .filter(outbound_job::Column::Status.eq(outbound_job::JobStatus::Pending))
        .filter(Expr::col(outbound_job::Column::NextAttemptAt).lte(Utc::now()))
        .order_by_asc(outbound_job::Column::NextAttemptAt)
        .limit(limit)
        .all(db)
        .await?;

    let mut claimed = Vec::with_capacity(due.len());
    for job in due {
        let mut active: outbound_job::ActiveModel = job.into();
        active.status = Set(outbound_job::JobStatus::Sending);
        active.updated_at = Set(Utc::now());
        claimed.push(
            active
                .update(db)
                .await
                .context("failed to claim a queue job")?,
        );
    }
    Ok(claimed)
}

/// Writes back the mutable state of a claimed job. Only the columns named here
/// are persisted: sea-orm's update emits just the fields a caller marks as set.
pub async fn save_job(
    db: &DatabaseConnection,
    id: Uuid,
    envelope_from: &str,
    status: outbound_job::JobStatus,
    attempts: i32,
    next_attempt_at: chrono::DateTime<Utc>,
    last_error: Option<String>,
) -> Result<()> {
    let active = outbound_job::ActiveModel {
        id: Set(id),
        envelope_from: Set(envelope_from.to_string()),
        status: Set(status),
        attempts: Set(attempts),
        next_attempt_at: Set(next_attempt_at),
        last_error: Set(last_error),
        updated_at: Set(Utc::now()),
        ..Default::default()
    };
    match active.update(db).await {
        Ok(_) => Ok(()),
        // The job can be gone because its send was cancelled while in flight.
        Err(sea_orm::DbErr::RecordNotUpdated) => Ok(()),
        Err(err) => Err(err).context("failed to save a queue job"),
    }
}

pub async fn count_pending(db: &DatabaseConnection) -> Result<u64> {
    let count = outbound_job::Entity::find()
        .filter(outbound_job::Column::Status.eq(outbound_job::JobStatus::Pending))
        .count(db)
        .await?;
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DbBackend, QueryTrait};

    /// Postgres returns `SUM(bigint)` as `numeric`, so the meter query must cast
    /// the total to bigint for an `i64` read.
    #[test]
    fn usage_query_casts_the_sum_to_bigint() {
        let sql = message::Entity::find()
            .select_only()
            .column_as(sum_size_bytes(), "used_bytes")
            .build(DbBackend::Postgres)
            .to_string();
        assert!(
            sql.contains("CAST(SUM(") && sql.contains("bigint"),
            "unexpected SQL: {sql}"
        );
    }
}
