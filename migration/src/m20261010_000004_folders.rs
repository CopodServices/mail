use sea_orm_migration::prelude::*;

/// Custom mailbox folders, the client's folder sidebar.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("folders"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Alias::new("mailbox_id")).uuid().not_null())
                    .col(ColumnDef::new(Alias::new("name")).string().not_null())
                    .col(
                        ColumnDef::new(Alias::new("color"))
                            .string()
                            .not_null()
                            .default("#2563eb"),
                    )
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .not_null()
                            .extra("DEFAULT now()".to_owned()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(Alias::new("folders"), Alias::new("mailbox_id"))
                            .to(Alias::new("mailboxes"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("folders_mailbox_name_idx")
                    .table(Alias::new("folders"))
                    .col(Alias::new("mailbox_id"))
                    .col(Alias::new("name"))
                    .unique()
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Alias::new("folders")).to_owned())
            .await?;
        Ok(())
    }
}
