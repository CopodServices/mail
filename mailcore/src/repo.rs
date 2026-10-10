use crate::secrets::SecretBox;
use anyhow::{Context, Result, anyhow};
use chrono::Utc;
use entity::{domain, mailbox, message, outbound_job};
use sea_orm::ExprTrait;
use sea_orm::sea_query::Expr;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder, QuerySelect, Set,
};
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

/// Creates the domain with a fresh DKIM key pair when it does not exist yet.
pub async fn ensure_domain(
    db: &DatabaseConnection,
    name: &str,
    secrets: &SecretBox,
) -> Result<domain::Model> {
    let name = name.trim().to_ascii_lowercase();
    if let Some(existing) = find_domain(db, &name).await? {
        return Ok(existing);
    }

    let key = crate::dkim::generate_key()?;
    let now = Utc::now();
    let model = domain::ActiveModel {
        id: Set(Uuid::now_v7()),
        name: Set(name),
        dkim_selector: Set("default".to_string()),
        dkim_private_key: Set(secrets.encrypt_bytes(&key.private_key)?),
        dkim_public_key: Set(key.public_key_b64),
        active: Set(true),
        created_at: Set(now),
        updated_at: Set(now),
    };
    Ok(model.insert(db).await?)
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

pub async fn create_mailbox(
    db: &DatabaseConnection,
    address: &str,
    password: &str,
    secrets: &SecretBox,
) -> Result<mailbox::Model> {
    let parsed = crate::address::parse(address)
        .ok_or_else(|| anyhow!("invalid email address: {address}"))?;
    let normalized = parsed.normalized();
    if find_mailbox(db, &normalized).await?.is_some() {
        return Err(anyhow!("mailbox {normalized} already exists"));
    }

    let parent = ensure_domain(db, &parsed.domain, secrets).await?;
    let now = Utc::now();
    let model = mailbox::ActiveModel {
        id: Set(Uuid::now_v7()),
        address: Set(normalized),
        domain_id: Set(parent.id),
        local_part: Set(parsed.local_part),
        password_hash: Set(crate::auth::hash_password(password)?),
        quota_bytes: Set(0),
        active: Set(true),
        created_at: Set(now),
        updated_at: Set(now),
    };
    Ok(model.insert(db).await?)
}

pub async fn set_mailbox_password(
    db: &DatabaseConnection,
    address: &str,
    password: &str,
) -> Result<bool> {
    let Some(existing) = find_mailbox(db, address).await? else {
        return Ok(false);
    };
    let mut active: mailbox::ActiveModel = existing.into();
    active.password_hash = Set(crate::auth::hash_password(password)?);
    active.updated_at = Set(Utc::now());
    active.update(db).await?;
    Ok(true)
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

pub async fn delete_message(db: &DatabaseConnection, id: Uuid) -> Result<Option<message::Model>> {
    let Some(existing) = get_message(db, id).await? else {
        return Ok(None);
    };
    message::Entity::delete_by_id(id).exec(db).await?;
    Ok(Some(existing))
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
    status: outbound_job::JobStatus,
    attempts: i32,
    next_attempt_at: chrono::DateTime<Utc>,
    last_error: Option<String>,
) -> Result<()> {
    let active = outbound_job::ActiveModel {
        id: Set(id),
        status: Set(status),
        attempts: Set(attempts),
        next_attempt_at: Set(next_attempt_at),
        last_error: Set(last_error),
        updated_at: Set(Utc::now()),
        ..Default::default()
    };
    active
        .update(db)
        .await
        .context("failed to save a queue job")?;
    Ok(())
}

pub async fn count_pending(db: &DatabaseConnection) -> Result<u64> {
    let count = outbound_job::Entity::find()
        .filter(outbound_job::Column::Status.eq(outbound_job::JobStatus::Pending))
        .count(db)
        .await?;
    Ok(count)
}
