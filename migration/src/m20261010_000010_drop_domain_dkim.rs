use sea_orm_migration::prelude::*;

/// Outbound DKIM is the configured relay's job now, so the service no longer
/// generates or stores per-domain DKIM keys.
///
/// A separate migration rather than an edit to the init migration, because the
/// init migration has already been applied in existing databases.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for column in ["dkim_selector", "dkim_private_key", "dkim_public_key"] {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("domains"))
                        .drop_column(Alias::new(column))
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for column in ["dkim_selector", "dkim_private_key", "dkim_public_key"] {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("domains"))
                        .add_column(
                            ColumnDef::new(Alias::new(column))
                                .string()
                                .not_null()
                                .default(""),
                        )
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}
