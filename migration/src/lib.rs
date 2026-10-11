pub use sea_orm_migration::prelude::*;

mod m20261010_000001_init;
mod m20261010_000002_message_flags;
mod m20261010_000003_threading;
mod m20261010_000004_folders;
mod m20261010_000005_snooze;
mod m20261010_000006_templates_settings;
mod m20261010_000007_contacts;
mod m20261010_000008_scheduled_send;
mod m20261010_000009_mailbox_signature;
mod m20261010_000010_drop_domain_dkim;
mod m20261010_000011_mailbox_display_name;
mod m20261011_000012_outbound_copy_and_keyset;
mod m20261011_000013_mailbox_auto_forward;
mod m20261011_000014_message_search_trgm;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20261010_000001_init::Migration),
            Box::new(m20261010_000002_message_flags::Migration),
            Box::new(m20261010_000003_threading::Migration),
            Box::new(m20261010_000004_folders::Migration),
            Box::new(m20261010_000005_snooze::Migration),
            Box::new(m20261010_000006_templates_settings::Migration),
            Box::new(m20261010_000007_contacts::Migration),
            Box::new(m20261010_000008_scheduled_send::Migration),
            Box::new(m20261010_000009_mailbox_signature::Migration),
            Box::new(m20261010_000010_drop_domain_dkim::Migration),
            Box::new(m20261010_000011_mailbox_display_name::Migration),
            Box::new(m20261011_000012_outbound_copy_and_keyset::Migration),
            Box::new(m20261011_000013_mailbox_auto_forward::Migration),
            Box::new(m20261011_000014_message_search_trgm::Migration),
        ]
    }
}
