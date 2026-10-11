use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

/// Trigram indexes so the webmail's text search index-scans instead of
/// sequentially scanning every message. pg_trgm is a trusted extension, so the
/// database owner may create it.
#[derive(DeriveMigrationName)]
pub struct Migration;

const COLUMNS: [&str; 3] = ["subject", "envelope_from", "envelope_to"];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared("CREATE EXTENSION IF NOT EXISTS pg_trgm")
            .await?;
        for column in COLUMNS {
            db.execute_unprepared(&format!(
                "CREATE INDEX IF NOT EXISTS idx_messages_{column}_trgm \
                 ON messages USING gin ({column} gin_trgm_ops)"
            ))
            .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        for column in COLUMNS {
            db.execute_unprepared(&format!("DROP INDEX IF EXISTS idx_messages_{column}_trgm"))
                .await?;
        }
        Ok(())
    }
}
