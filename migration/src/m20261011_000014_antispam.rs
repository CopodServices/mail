use sea_orm_migration::prelude::*;

/// Antispam: per-message verdicts, a per-mailbox junk threshold, a sender
/// allow-list, and the classifier's durable model table.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("messages"))
                    .add_column(ColumnDef::new(Alias::new("spam_score")).double().null())
                    .add_column(ColumnDef::new(Alias::new("spam_tags")).text().null())
                    .add_column(ColumnDef::new(Alias::new("spam_action")).text().null())
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("settings"))
                    .add_column(ColumnDef::new(Alias::new("spam_threshold")).double().null())
                    .to_owned(),
            )
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("contacts"))
                    .add_column(
                        ColumnDef::new(Alias::new("allowed"))
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(Alias::new("spam_models"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("scope"))
                            .string()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Alias::new("payload")).binary().not_null())
                    .col(
                        ColumnDef::new(Alias::new("trained"))
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(Alias::new("updated_at"))
                            .timestamp_with_time_zone()
                            .not_null()
                            .extra("DEFAULT now()".to_owned()),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(Alias::new("spam_samples"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Alias::new("mailbox_id")).uuid().null())
                    .col(ColumnDef::new(Alias::new("tokens")).text().not_null())
                    .col(ColumnDef::new(Alias::new("is_spam")).boolean().not_null())
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .not_null()
                            .extra("DEFAULT now()".to_owned()),
                    )
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Alias::new("spam_samples")).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Alias::new("spam_models")).to_owned())
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("contacts"))
                    .drop_column(Alias::new("allowed"))
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("settings"))
                    .drop_column(Alias::new("spam_threshold"))
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("messages"))
                    .drop_column(Alias::new("spam_action"))
                    .drop_column(Alias::new("spam_tags"))
                    .drop_column(Alias::new("spam_score"))
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}
