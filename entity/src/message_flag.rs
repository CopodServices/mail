use chrono::Utc;
use sea_orm::entity::prelude::*;

/// Reader state for a stored message: read, starred and the folder it lives in.
///
/// The MTA's message row is immutable and only knows direction and delivery
/// status, so everything the mail client changes lives here, keyed by message.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "message_flags")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub message_id: Uuid,
    pub read: bool,
    pub starred: bool,
    /// inbox, sent, spam, trash, archived, snoozed, scheduled, or a folder id.
    pub folder: String,
    /// While this is in the future the message shows in Snoozed, not its folder.
    pub snoozed_until: Option<chrono::DateTime<Utc>>,
    pub updated_at: chrono::DateTime<Utc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
