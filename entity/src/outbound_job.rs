use chrono::Utc;
use sea_orm::entity::prelude::*;
use sea_orm::{DeriveActiveEnum, EnumIter};

#[derive(Clone, Debug, PartialEq, Eq, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::N(16))")]
pub enum JobStatus {
    #[sea_orm(string_value = "pending")]
    Pending,
    #[sea_orm(string_value = "sending")]
    Sending,
    #[sea_orm(string_value = "sent")]
    Sent,
    #[sea_orm(string_value = "failed")]
    Failed,
}

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "outbound_jobs")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub mailbox_id: Option<Uuid>,
    pub envelope_from: String,
    #[sea_orm(column_type = "Json")]
    pub recipients: Json,
    pub storage_key: String,
    pub message_id: Option<String>,
    /// The Scheduled-folder copy to move to Sent once the message goes out.
    pub scheduled_message_id: Option<Uuid>,
    /// The copy this job delivers: the row shown in the sender's Sent folder,
    /// which reports the job's state.
    pub copy_message_id: Option<Uuid>,
    pub attempts: i32,
    pub next_attempt_at: chrono::DateTime<Utc>,
    pub status: JobStatus,
    pub last_error: Option<String>,
    pub created_at: chrono::DateTime<Utc>,
    pub updated_at: chrono::DateTime<Utc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
