use sea_orm_migration::prelude::*;

/// Reader state for the mail client: read, starred and folder per message.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("message_flags"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("message_id"))
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("read"))
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(Alias::new("starred"))
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(Alias::new("folder"))
                            .string()
                            .not_null()
                            .default("inbox"),
                    )
                    .col(
                        ColumnDef::new(Alias::new("updated_at"))
                            .timestamp_with_time_zone()
                            .not_null()
                            .extra("DEFAULT now()".to_owned()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(Alias::new("message_flags"), Alias::new("message_id"))
                            .to(Alias::new("messages"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("message_flags_folder_idx")
                    .table(Alias::new("message_flags"))
                    .col(Alias::new("folder"))
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Alias::new("message_flags")).to_owned())
            .await?;
        Ok(())
    }
}
