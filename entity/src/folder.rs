use chrono::Utc;
use sea_orm::entity::prelude::*;

/// A user-created mailbox folder. System folders (inbox, sent, trash, ...) are
/// implied, so only custom folders are stored.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "folders")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub mailbox_id: Uuid,
    pub name: String,
    pub color: String,
    pub created_at: chrono::DateTime<Utc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
