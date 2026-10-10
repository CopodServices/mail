use sea_orm_migration::prelude::*;

/// Records which stored copy a queued message belongs to, so the Sent list can
/// report whether that copy is still queued, is going out, or failed, and
/// indexes the key the message list pages with.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("outbound_jobs"))
                    .add_column(ColumnDef::new(Alias::new("copy_message_id")).uuid().null())
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_outbound_jobs_scheduled_message")
                    .table(Alias::new("outbound_jobs"))
                    .col(Alias::new("scheduled_message_id"))
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_outbound_jobs_copy_message")
                    .table(Alias::new("outbound_jobs"))
                    .col(Alias::new("copy_message_id"))
                    .to_owned(),
            )
            .await?;

        // Lists page with (received_at, id) descending inside one mailbox, so
        // the index carries the id tiebreaker as well.
        manager
            .drop_index(
                Index::drop()
                    .name("idx_messages_mailbox_received")
                    .table(Alias::new("messages"))
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_messages_mailbox_received_id")
                    .table(Alias::new("messages"))
                    .col(Alias::new("mailbox_id"))
                    .col(Alias::new("received_at"))
                    .col(Alias::new("id"))
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .name("idx_messages_mailbox_received_id")
                    .table(Alias::new("messages"))
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_messages_mailbox_received")
                    .table(Alias::new("messages"))
                    .col(Alias::new("mailbox_id"))
                    .col(Alias::new("received_at"))
                    .to_owned(),
            )
            .await?;
        manager
            .drop_index(
                Index::drop()
                    .name("idx_outbound_jobs_copy_message")
                    .table(Alias::new("outbound_jobs"))
                    .to_owned(),
            )
            .await?;
        manager
            .drop_index(
                Index::drop()
                    .name("idx_outbound_jobs_scheduled_message")
                    .table(Alias::new("outbound_jobs"))
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("outbound_jobs"))
                    .drop_column(Alias::new("copy_message_id"))
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}
