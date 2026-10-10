use sea_orm_migration::prelude::*;

/// Links a queued message to the copy shown in the Scheduled folder, so the
/// worker can move it to Sent when it actually goes out.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("outbound_jobs"))
                    .add_column(
                        ColumnDef::new(Alias::new("scheduled_message_id"))
                            .uuid()
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
                    .table(Alias::new("outbound_jobs"))
                    .drop_column(Alias::new("scheduled_message_id"))
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}
