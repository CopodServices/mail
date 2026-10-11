use chrono::Utc;
use sea_orm::entity::prelude::*;
use sea_orm::{DeriveActiveEnum, EnumIter};

#[derive(Clone, Debug, PartialEq, Eq, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::N(16))")]
pub enum Direction {
    #[sea_orm(string_value = "inbound")]
    Inbound,
    #[sea_orm(string_value = "outbound")]
    Outbound,
}

#[derive(Clone, Debug, PartialEq, Eq, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::N(16))")]
pub enum Status {
    #[sea_orm(string_value = "stored")]
    Stored,
    #[sea_orm(string_value = "quarantined")]
    Quarantined,
}

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "messages")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    /// Stable per-message number the IMAP and POP3 readers address mail by.
    /// Assigned from a sequence, so it only ever grows.
    pub uid: i64,
    pub mailbox_id: Option<Uuid>,
    pub direction: Direction,
    pub status: Status,
    pub envelope_from: String,
    pub envelope_to: String,
    pub message_id: Option<String>,
    pub subject: Option<String>,
    pub header_from: Option<String>,
    /// In-Reply-To header, normalized to the bare message id.
    pub in_reply_to: Option<String>,
    /// References header, the conversation ancestors, space separated.
    pub references_header: Option<String>,
    pub size_bytes: i64,
    pub storage_key: String,
    pub spf_result: Option<String>,
    pub dkim_result: Option<String>,
    pub dmarc_result: Option<String>,
    pub dmarc_policy: Option<String>,
    pub received_at: chrono::DateTime<Utc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
