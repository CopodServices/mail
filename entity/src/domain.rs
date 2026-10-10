use chrono::Utc;
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "domains")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    #[sea_orm(unique)]
    pub name: String,
    pub dkim_selector: String,
    /// AES-256-GCM ciphertext (base64) of the PKCS#1 DER private key.
    pub dkim_private_key: String,
    /// Base64 of the PKCS#1 DER public key, i.e. the DKIM `p=` tag value.
    pub dkim_public_key: String,
    pub active: bool,
    pub created_at: chrono::DateTime<Utc>,
    pub updated_at: chrono::DateTime<Utc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
