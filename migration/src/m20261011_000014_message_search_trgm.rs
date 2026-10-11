use sea_orm::ConnectionTrait;
use sea_orm_migration::prelude::*;

/// Trigram indexes for the list's `ILIKE '%text%'` search over the subject and
/// the envelope addresses, so a search matches an index instead of scanning
/// every one of a mailbox's messages.
#[derive(DeriveMigrationName)]
pub struct Migration;

/// The trigram index per searched column. `gin_trgm_ops` turns `ILIKE '%x%'`
/// into an index lookup.
const INDEXES: [(&str, &str); 3] = [
    ("idx_messages_subject_trgm", "subject"),
    ("idx_messages_envelope_from_trgm", "envelope_from"),
    ("idx_messages_envelope_to_trgm", "envelope_to"),
];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let connection = manager.get_connection();
        // pg_trgm is a trusted extension, so the database owner may install it.
        connection
            .execute_unprepared("CREATE EXTENSION IF NOT EXISTS pg_trgm")
            .await?;
        for (index, column) in INDEXES {
            connection
                .execute_unprepared(&format!(
                    "CREATE INDEX IF NOT EXISTS {index} ON messages USING gin ({column} gin_trgm_ops)"
                ))
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let connection = manager.get_connection();
        for (index, _) in INDEXES {
            connection
                .execute_unprepared(&format!("DROP INDEX IF EXISTS {index}"))
                .await?;
        }
        Ok(())
    }
}
