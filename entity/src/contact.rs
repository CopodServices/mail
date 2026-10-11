use chrono::Utc;
use sea_orm::entity::prelude::*;

/// An address book entry for one mailbox, with an optional picture.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "contacts")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub mailbox_id: Uuid,
    pub address: String,
    pub display_name: Option<String>,
    pub avatar_key: Option<String>,
    pub preview_key: Option<String>,
    pub avatar_content_type: Option<String>,
    pub blocked: bool,
    /// Never file this sender as junk, whatever the score says.
    pub allowed: bool,
    /// manual, inbound or outbound.
    pub source: String,
    pub last_seen_at: chrono::DateTime<Utc>,
    pub created_at: chrono::DateTime<Utc>,
    pub updated_at: chrono::DateTime<Utc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
