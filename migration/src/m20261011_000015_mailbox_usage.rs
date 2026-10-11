use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

/// A running total of the bytes stored for a mailbox, so the per-delivery quota
/// check does not sum the whole mailbox on every message.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("mailboxes"))
                    .add_column(
                        ColumnDef::new(Alias::new("used_bytes"))
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .get_connection()
            .execute_unprepared(
                "UPDATE mailboxes m SET used_bytes = COALESCE(\
                 (SELECT SUM(size_bytes) FROM messages WHERE mailbox_id = m.id), 0)",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("mailboxes"))
                    .drop_column(Alias::new("used_bytes"))
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}
