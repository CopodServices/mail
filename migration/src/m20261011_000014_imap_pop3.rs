use sea_orm_migration::prelude::*;

/// Everything the IMAP and POP3 readers need that the web reader did not:
/// a stable per-message UID, a UIDVALIDITY per mailbox, and the two flags an
/// external client can set that the reader state did not carry.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // One global sequence: unique by construction, so it is also strictly
        // increasing inside any single mailbox, which is what IMAP UIDs and
        // POP3 UIDLs need.
        db.execute_unprepared("CREATE SEQUENCE IF NOT EXISTS messages_uid_seq")
            .await?;
        db.execute_unprepared("ALTER TABLE messages ADD COLUMN uid BIGINT")
            .await?;
        // Backfill in arrival order so the sequence numbers a client sees match
        // the order the mail arrived in.
        db.execute_unprepared(
            "WITH ordered AS (\
                 SELECT id, row_number() OVER (ORDER BY received_at, id) AS rn FROM messages\
             )\
             UPDATE messages m SET uid = o.rn FROM ordered o WHERE m.id = o.id",
        )
        .await?;
        db.execute_unprepared(
            "SELECT setval('messages_uid_seq', \
             COALESCE((SELECT max(uid) FROM messages), 0) + 1, false)",
        )
        .await?;
        db.execute_unprepared(
            "ALTER TABLE messages ALTER COLUMN uid SET DEFAULT nextval('messages_uid_seq')",
        )
        .await?;
        db.execute_unprepared("ALTER TABLE messages ALTER COLUMN uid SET NOT NULL")
            .await?;
        db.execute_unprepared("ALTER SEQUENCE messages_uid_seq OWNED BY messages.uid")
            .await?;
        db.execute_unprepared("CREATE UNIQUE INDEX messages_uid_key ON messages (uid)")
            .await?;

        // UIDVALIDITY: a random non-zero value per mailbox, so a UID space that
        // was rebuilt is never mistaken for the old one. New mailboxes get a
        // fresh value from the column default.
        db.execute_unprepared(
            "ALTER TABLE mailboxes ADD COLUMN uid_validity INTEGER NOT NULL \
             DEFAULT (floor(random() * 2147483646) + 1)::int",
        )
        .await?;
        db.execute_unprepared(
            "UPDATE mailboxes SET uid_validity = (floor(random() * 2147483646) + 1)::int",
        )
        .await?;

        db.execute_unprepared(
            "ALTER TABLE message_flags ADD COLUMN answered BOOLEAN NOT NULL DEFAULT false",
        )
        .await?;
        db.execute_unprepared(
            "ALTER TABLE message_flags ADD COLUMN deleted BOOLEAN NOT NULL DEFAULT false",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared("ALTER TABLE message_flags DROP COLUMN deleted")
            .await?;
        db.execute_unprepared("ALTER TABLE message_flags DROP COLUMN answered")
            .await?;
        db.execute_unprepared("ALTER TABLE mailboxes DROP COLUMN uid_validity")
            .await?;
        db.execute_unprepared("DROP INDEX IF EXISTS messages_uid_key")
            .await?;
        db.execute_unprepared("ALTER TABLE messages DROP COLUMN uid")
            .await?;
        db.execute_unprepared("DROP SEQUENCE IF EXISTS messages_uid_seq")
            .await?;
        Ok(())
    }
}
