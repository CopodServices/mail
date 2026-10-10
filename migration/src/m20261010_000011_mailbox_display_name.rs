use sea_orm_migration::prelude::*;

/// The name recipients see beside a mailbox's address in the From header.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("mailboxes"))
                    .add_column(ColumnDef::new(Alias::new("display_name")).text().null())
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("mailboxes"))
                    .drop_column(Alias::new("display_name"))
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}
