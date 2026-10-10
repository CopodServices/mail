use sea_orm_migration::prelude::*;

/// When a message should come back to the inbox.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("message_flags"))
                    .add_column(
                        ColumnDef::new(Alias::new("snoozed_until"))
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("message_flags"))
                    .drop_column(Alias::new("snoozed_until"))
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}
