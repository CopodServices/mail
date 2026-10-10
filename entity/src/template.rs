use chrono::Utc;
use sea_orm::entity::prelude::*;

/// A composer template: canned subject and body the client can drop in.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "templates")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub mailbox_id: Option<Uuid>,
    pub from_addr: String,
    pub title: String,
    pub text_body: String,
    pub html_body: String,
    pub created_at: chrono::DateTime<Utc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
