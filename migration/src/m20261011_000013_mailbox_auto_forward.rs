use sea_orm_migration::prelude::*;

/// An address every arriving message for a mailbox is copied to.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("mailboxes"))
                    .add_column(ColumnDef::new(Alias::new("forward_to")).text().null())
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
                    .drop_column(Alias::new("forward_to"))
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}
