use sea_orm_migration::prelude::*;

/// Address book entries per mailbox.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("contacts"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Alias::new("mailbox_id")).uuid().not_null())
                    .col(ColumnDef::new(Alias::new("address")).string().not_null())
                    .col(ColumnDef::new(Alias::new("display_name")).string().null())
                    .col(ColumnDef::new(Alias::new("avatar_key")).string().null())
                    .col(ColumnDef::new(Alias::new("preview_key")).string().null())
                    .col(
                        ColumnDef::new(Alias::new("avatar_content_type"))
                            .string()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("blocked"))
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(Alias::new("source"))
                            .string()
                            .not_null()
                            .default("manual"),
                    )
                    .col(
                        ColumnDef::new(Alias::new("last_seen_at"))
                            .timestamp_with_time_zone()
                            .not_null()
                            .extra("DEFAULT now()".to_owned()),
                    )
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .not_null()
                            .extra("DEFAULT now()".to_owned()),
                    )
                    .col(
                        ColumnDef::new(Alias::new("updated_at"))
                            .timestamp_with_time_zone()
                            .not_null()
                            .extra("DEFAULT now()".to_owned()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(Alias::new("contacts"), Alias::new("mailbox_id"))
                            .to(Alias::new("mailboxes"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("contacts_mailbox_address_idx")
                    .table(Alias::new("contacts"))
                    .col(Alias::new("mailbox_id"))
                    .col(Alias::new("address"))
                    .unique()
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Alias::new("contacts")).to_owned())
            .await?;
        Ok(())
    }
}
