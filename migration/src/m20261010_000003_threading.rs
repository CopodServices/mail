use sea_orm_migration::prelude::*;

/// Threading headers, so a conversation can be reassembled from storage.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("messages"))
                    .add_column(ColumnDef::new(Alias::new("in_reply_to")).text().null())
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("messages"))
                    .add_column(
                        ColumnDef::new(Alias::new("references_header"))
                            .text()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("messages_thread_idx")
                    .table(Alias::new("messages"))
                    .col(Alias::new("mailbox_id"))
                    .col(Alias::new("message_id"))
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .name("messages_thread_idx")
                    .table(Alias::new("messages"))
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("messages"))
                    .drop_column(Alias::new("references_header"))
                    .drop_column(Alias::new("in_reply_to"))
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}
