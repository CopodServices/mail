use sea_orm_migration::prelude::*;

/// A per-mailbox signature the composer appends to new messages.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("mailboxes"))
                    .add_column(ColumnDef::new(Alias::new("signature")).text().null())
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
                    .drop_column(Alias::new("signature"))
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}
