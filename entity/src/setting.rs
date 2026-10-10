use chrono::Utc;
use sea_orm::entity::prelude::*;

/// Client preferences. There are no accounts yet, so a single default row
/// holds the settings for the whole installation.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "settings")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    #[sea_orm(unique)]
    pub scope: String,
    pub keyboard_shortcuts_enabled: bool,
    pub show_full_recipient_addresses: bool,
    pub updated_at: chrono::DateTime<Utc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
