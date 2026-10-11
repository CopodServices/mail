use chrono::Utc;
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "mailboxes")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    #[sea_orm(unique)]
    pub address: String,
    pub domain_id: Uuid,
    pub local_part: String,
    pub password_hash: String,
    /// IMAP UIDVALIDITY for this mailbox's UID space.
    pub uid_validity: i32,
    /// Appended to new messages composed from this mailbox.
    pub signature: Option<String>,
    /// The name recipients see beside the address in the From header.
    pub display_name: Option<String>,
    /// Address every arriving message is copied to, when set.
    pub forward_to: Option<String>,
    pub quota_bytes: i64,
    pub active: bool,
    pub created_at: chrono::DateTime<Utc>,
    pub updated_at: chrono::DateTime<Utc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
