use crate::smtp::{self, Mode};
use crate::state::AppState;
use chrono::Utc;
use entity::message;
use entity::outbound_job;
use mailcore::config::{Config, RelayConfig, RelayEncryption, S3Config, TlsConfig};
use mailcore::query;
use mailcore::queue::Queue;
use mailcore::relay::Relay;
use mailcore::repo;
use mailcore::store::MailStore;
use migration::{Migrator, MigratorTrait};
use sea_orm::{ActiveModelTrait, ColumnTrait, Database, EntityTrait, QueryFilter, Set};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use uuid::Uuid;

/// Wire-format CRLF, spelled as bytes so the file stays free of escapes.
const CRLF: [u8; 2] = [13, 10];

fn test_config() -> Config {
    Config {
        database_url: std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://cpod:cpod@localhost:5432/cpod".to_string()),
        session_secret: "test-session-secret".to_string(),
        session_cookie_secure: false,
        api_key: None,
        server_port: 0,
        smtp_listen: "127.0.0.1:0".to_string(),
        submission_listen: "127.0.0.1:0".to_string(),
        imap_listen: None,
        pop3_listen: None,
        hostname: "mx.example.test".to_string(),
        bootstrap_domains: Vec::new(),
        bootstrap_password: None,
        max_message_size: 5 * 1024 * 1024,
        max_recipients: 10,
        // Keep the test offline: no DNS lookups.
        verify_auth: false,
        queue_interval_secs: 5,
        max_attempts: 3,
        s3: S3Config {
            endpoint: Some(
                std::env::var("S3_ENDPOINT")
                    .unwrap_or_else(|_| "http://localhost:9090".to_string()),
            ),
            region: "us-east-1".to_string(),
            bucket: std::env::var("S3_BUCKET").unwrap_or_else(|_| "copod-mail".to_string()),
            access_key: std::env::var("S3_ACCESS_KEY").unwrap_or_else(|_| "cpod".to_string()),
            secret_key: std::env::var("S3_SECRET_KEY").unwrap_or_else(|_| "cpod12345".to_string()),
            path_style: true,
            allow_http: true,
        },
        relay: RelayConfig {
            host: "127.0.0.1".to_string(),
            port: 1025,
            username: None,
            password: None,
            encryption: RelayEncryption::None,
            from_override: None,
        },
        tls: None::<TlsConfig>,
    }
}

/// End-to-end check of the inbound path. Requires the compose stack:
///
///     make up && MAIL_TEST_INTEGRATION=1 cargo test -p server -- --nocapture
#[tokio::test]
async fn stores_inbound_mail_to_s3_and_postgres() {
    if std::env::var("MAIL_TEST_INTEGRATION").is_err() {
        eprintln!("skipping: set MAIL_TEST_INTEGRATION=1 with the compose stack running");
        return;
    }

    let config = Arc::new(test_config());
    let db = Database::connect(&config.database_url)
        .await
        .expect("connect");
    Migrator::up(&db, None).await.expect("migrate");

    let store = MailStore::new(&config.s3).expect("object store");

    let domain = format!("test-{}.example", Uuid::now_v7().simple());
    repo::ensure_domain(&db, &domain).await.unwrap();
    let address = format!("inbox@{domain}");
    let mailbox = repo::create_mailbox(&db, &address, "secret").await.unwrap();

    let state = AppState {
        db: db.clone(),
        config: config.clone(),
        store: store.clone(),
        authenticator: None,
        tls: None,
        events: tokio::sync::broadcast::channel(16).0,
    };

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(smtp::serve(listener, state.clone(), Mode::Inbound));

    let stream = TcpStream::connect(addr).await.unwrap();
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);

    assert_eq!(read_reply(&mut reader).await, 220);
    command(&mut write_half, "EHLO client.example.test").await;
    assert_eq!(read_reply(&mut reader).await, 250);
    command(&mut write_half, "MAIL FROM:<sender@remote.test>").await;
    assert_eq!(read_reply(&mut reader).await, 250);

    // Unknown local recipient is refused.
    command(&mut write_half, &format!("RCPT TO:<nobody@{domain}>")).await;
    assert_eq!(read_reply(&mut reader).await, 550);

    command(&mut write_half, &format!("RCPT TO:<{address}>")).await;
    assert_eq!(read_reply(&mut reader).await, 250);

    command(&mut write_half, "DATA").await;
    assert_eq!(read_reply(&mut reader).await, 354);
    write_half
        .write_all(&wire(&[
            "From: Alice <alice@remote.test>",
            "To: inbox",
            "Subject: Integration test",
            "",
            "body line",
            ".",
        ]))
        .await
        .unwrap();
    write_half.flush().await.unwrap();
    assert_eq!(read_reply(&mut reader).await, 250);

    command(&mut write_half, "QUIT").await;
    assert_eq!(read_reply(&mut reader).await, 221);

    let stored = message::Entity::find()
        .filter(message::Column::MailboxId.eq(mailbox.id))
        .all(&db)
        .await
        .unwrap();
    assert_eq!(stored.len(), 1, "one message should be indexed");
    let stored = &stored[0];
    assert_eq!(stored.subject.as_deref(), Some("Integration test"));
    assert_eq!(stored.envelope_to, address);
    assert_eq!(stored.direction, message::Direction::Inbound);

    let raw = store.get(&stored.storage_key).await.expect("read object");
    let text = String::from_utf8_lossy(&raw);
    assert!(text.contains("Integration test"));
    assert!(text.contains("body line"));

    // Clean up the fixtures.
    let _ = store.delete(&stored.storage_key).await;
    let _ = repo::delete_message(&db, stored.id).await;
    let _ = repo::delete_mailbox(&db, &address).await;
    let _ = entity::domain::Entity::delete_many()
        .filter(entity::domain::Column::Name.eq(domain))
        .exec(&db)
        .await;
}

/// Submission path: AUTH is required, the sender must match the authenticated
/// mailbox, and the queued message is relayed to the smarthost (Mailpit).
#[tokio::test]
async fn relays_submission_through_the_smarthost() {
    if std::env::var("MAIL_TEST_INTEGRATION").is_err() {
        eprintln!("skipping: set MAIL_TEST_INTEGRATION=1 with the compose stack running");
        return;
    }

    let config = Arc::new(test_config());
    let db = Database::connect(&config.database_url)
        .await
        .expect("connect");
    Migrator::up(&db, None).await.expect("migrate");

    let store = MailStore::new(&config.s3).expect("object store");

    let domain = format!("submit-{}.example", Uuid::now_v7().simple());
    repo::ensure_domain(&db, &domain).await.unwrap();
    let address = format!("sender@{domain}");
    let sender_mailbox = repo::create_mailbox(&db, &address, "secret").await.unwrap();
    // A second mailbox on the same domain: it gets a local copy of its own, while
    // the sender's copy is the one filed in Sent.
    let local_address = format!("local@{domain}");
    let local_mailbox = repo::create_mailbox(&db, &local_address, "secret")
        .await
        .unwrap();

    let state = AppState {
        db: db.clone(),
        config: config.clone(),
        store: store.clone(),
        authenticator: None,
        tls: None,
        events: tokio::sync::broadcast::channel(16).0,
    };

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(smtp::serve(listener, state.clone(), Mode::Submission));

    let stream = TcpStream::connect(addr).await.unwrap();
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);

    assert_eq!(read_reply(&mut reader).await, 220);
    command(&mut write_half, "EHLO client.example.test").await;
    assert_eq!(read_reply(&mut reader).await, 250);

    // No AUTH yet: MAIL FROM must be refused.
    command(&mut write_half, &format!("MAIL FROM:<{address}>")).await;
    assert_eq!(read_reply(&mut reader).await, 530);

    // Wrong password is rejected.
    let bad = base64_plain(&address, "wrong");
    command(&mut write_half, &format!("AUTH PLAIN {bad}")).await;
    assert_eq!(read_reply(&mut reader).await, 535);

    let good = base64_plain(&address, "secret");
    command(&mut write_half, &format!("AUTH PLAIN {good}")).await;
    assert_eq!(read_reply(&mut reader).await, 235);

    // A different envelope sender is refused (no open relay).
    command(&mut write_half, "MAIL FROM:<someone-else@remote.test>").await;
    assert_eq!(read_reply(&mut reader).await, 550);

    command(&mut write_half, &format!("MAIL FROM:<{address}>")).await;
    assert_eq!(read_reply(&mut reader).await, 250);
    command(&mut write_half, "RCPT TO:<friend@remote.test>").await;
    assert_eq!(read_reply(&mut reader).await, 250);
    command(&mut write_half, &format!("RCPT TO:<{local_address}>")).await;
    assert_eq!(read_reply(&mut reader).await, 250);
    // The blind recipient is enveloped too, so its copy is handed over.
    command(&mut write_half, "RCPT TO:<blind@remote.test>").await;
    assert_eq!(read_reply(&mut reader).await, 250);

    command(&mut write_half, "DATA").await;
    assert_eq!(read_reply(&mut reader).await, 354);
    write_half
        .write_all(&wire(&[
            "From: Sender <sender@remote.test>",
            "To: friend@remote.test",
            "Subject: Relayed",
            "Bcc: blind@remote.test",
            "",
            "relay me",
            ".",
        ]))
        .await
        .unwrap();
    write_half.flush().await.unwrap();
    assert_eq!(read_reply(&mut reader).await, 250);
    command(&mut write_half, "QUIT").await;
    assert_eq!(read_reply(&mut reader).await, 221);

    // Scoped to this run's sender: the database is shared with the other fixtures.
    let jobs = outbound_job::Entity::find()
        .filter(outbound_job::Column::EnvelopeFrom.eq(address.clone()))
        .all(&db)
        .await
        .unwrap();
    // The openly addressed recipient travels on one copy, the blind recipient on
    // another: each job is one set of recipients that may read the same bytes.
    let job = jobs
        .iter()
        .find(|job| job.recipients == serde_json::json!(["friend@remote.test"]))
        .expect("a job for the openly addressed recipient")
        .clone();
    assert_eq!(job.envelope_from, address);
    assert_eq!(job.status, outbound_job::JobStatus::Pending);
    let blind_job = jobs
        .iter()
        .find(|job| job.recipients == serde_json::json!(["blind@remote.test"]))
        .expect("a job for the blind recipient")
        .clone();
    // The blind recipient's copy names them and nobody else; the copy the openly
    // addressed recipient is handed names no blind recipient at all. The sender's
    // own Sent copy is the one that keeps the list.
    let blind_copy = store
        .get(&blind_job.storage_key)
        .await
        .expect("read the blind copy");
    let blind_text = String::from_utf8_lossy(&blind_copy).to_string();
    assert!(
        blind_text.contains("Bcc: <blind@remote.test>"),
        "{blind_text}"
    );
    let bcc_line = blind_text
        .lines()
        .find(|line| line.to_ascii_lowercase().starts_with("bcc:"))
        .expect("a Bcc header on the blind copy")
        .trim()
        .to_string();
    assert_eq!(
        bcc_line, "Bcc: <blind@remote.test>",
        "the blind copy names its own recipient and nobody else"
    );
    let queued = store
        .get(&job.storage_key)
        .await
        .expect("read the queued copy");
    assert!(
        !String::from_utf8_lossy(&queued)
            .to_lowercase()
            .contains("bcc"),
        "a relayed copy must not name blind recipients"
    );
    let sent = message::Entity::find()
        .filter(message::Column::MailboxId.eq(sender_mailbox.id))
        .all(&db)
        .await
        .unwrap();
    assert_eq!(sent.len(), 1, "the sender's own copy is filed once");
    let sent_raw = store
        .get(&sent[0].storage_key)
        .await
        .expect("read the sent copy");
    assert_eq!(
        String::from_utf8_lossy(&sent_raw)
            .matches("Bcc: blind@remote.test")
            .count(),
        1,
        "the Sent copy keeps the header"
    );
    let inbox = message::Entity::find()
        .filter(message::Column::MailboxId.eq(local_mailbox.id))
        .all(&db)
        .await
        .unwrap();
    assert_eq!(inbox.len(), 1, "the local recipient has one copy");
    let inbox_raw = store
        .get(&inbox[0].storage_key)
        .await
        .expect("read the inbox copy");
    assert!(
        !String::from_utf8_lossy(&inbox_raw)
            .to_lowercase()
            .contains("bcc"),
        "a local recipient's copy must not name blind recipients"
    );

    // Process the queue once; Mailpit accepts, so the job must become sent.
    let queue = Queue {
        db: db.clone(),
        store: store.clone(),
        relay: Relay::new(config.relay.clone()),
        max_attempts: 3,
        helo: config.hostname.clone(),
    };
    assert_eq!(queue.run_once().await.unwrap(), 2);

    let job = outbound_job::Entity::find_by_id(job.id)
        .one(&db)
        .await
        .unwrap()
        .expect("job still present");
    assert_eq!(
        job.status,
        outbound_job::JobStatus::Sent,
        "relay should accept"
    );
    assert!(job.attempts <= 1);
    let blind_out = outbound_job::Entity::find_by_id(blind_job.id)
        .one(&db)
        .await
        .unwrap()
        .expect("blind job still present");
    assert_eq!(
        blind_out.status,
        outbound_job::JobStatus::Sent,
        "the relay accepts the blind copy too"
    );

    // Clean up the fixtures.
    let _ = store.delete(&job.storage_key).await;
    let _ = store.delete(&blind_job.storage_key).await;
    let _ = store.delete(&sent[0].storage_key).await;
    let _ = store.delete(&inbox[0].storage_key).await;
    let _ = outbound_job::Entity::delete_by_id(job.id).exec(&db).await;
    let _ = outbound_job::Entity::delete_by_id(blind_job.id)
        .exec(&db)
        .await;
    let _ = repo::delete_mailbox(&db, &address).await;
    let _ = repo::delete_mailbox(&db, &local_address).await;
    let _ = entity::domain::Entity::delete_many()
        .filter(entity::domain::Column::Name.eq(domain))
        .exec(&db)
        .await;
}

/// A blind recipient is told by their own copy: it names them and nobody else,
/// and no copy another recipient reads names any blind recipient.
#[tokio::test]
async fn a_blind_recipient_is_named_by_their_own_copy() {
    if std::env::var("MAIL_TEST_INTEGRATION").is_err() {
        eprintln!("skipping: set MAIL_TEST_INTEGRATION=1 with the compose stack running");
        return;
    }

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    let config = Arc::new(test_config());
    let db = Database::connect(&config.database_url)
        .await
        .expect("connect");
    Migrator::up(&db, None).await.expect("migrate");
    let store = MailStore::new(&config.s3).expect("object store");

    let domain = format!("blind-{}.example", Uuid::now_v7().simple());
    repo::ensure_domain(&db, &domain).await.unwrap();
    let address = format!("sender@{domain}");
    let sender = repo::create_mailbox(&db, &address, "secret").await.unwrap();
    let friend = format!("friend@{domain}");
    let friend_box = repo::create_mailbox(&db, &friend, "secret").await.unwrap();
    let blind = format!("blind@{domain}");
    let blind_box = repo::create_mailbox(&db, &blind, "secret").await.unwrap();

    let state = AppState {
        db: db.clone(),
        config: config.clone(),
        store: store.clone(),
        authenticator: None,
        tls: None,
        events: tokio::sync::broadcast::channel(16).0,
    };

    let app = crate::api::router(state.clone());
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/send")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "from": address,
                        "to": [friend],
                        "bcc": [blind],
                        "subject": "Blind copy",
                        "text": "hello",
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        status,
        StatusCode::ACCEPTED,
        "{}",
        String::from_utf8_lossy(&body)
    );

    let friend_copy = only_copy(&db, &store, friend_box.id).await;
    assert!(
        !friend_copy.to_lowercase().contains("bcc"),
        "the addressed recipient is told nothing about it: {friend_copy}"
    );
    let blind_copy = only_copy(&db, &store, blind_box.id).await;
    assert!(
        blind_copy.contains(&format!("Bcc: <{blind}>")),
        "the blind recipient is named by their own copy: {blind_copy}"
    );
    let bcc_line = blind_copy
        .lines()
        .find(|line| line.to_ascii_lowercase().starts_with("bcc:"))
        .expect("a Bcc header on the blind copy")
        .trim()
        .to_string();
    assert_eq!(
        bcc_line,
        format!("Bcc: <{blind}>"),
        "the blind copy names its own recipient and nobody else"
    );
    let own_copy = only_copy(&db, &store, sender.id).await;
    assert!(
        own_copy.contains(&format!("Bcc: <{blind}>")),
        "the sender keeps the list: {own_copy}"
    );

    // Clean up the fixtures.
    for id in [sender.id, friend_box.id, blind_box.id] {
        let stored = message::Entity::find()
            .filter(message::Column::MailboxId.eq(id))
            .all(&db)
            .await
            .unwrap();
        for row in &stored {
            let _ = store.delete(&row.storage_key).await;
            let _ = repo::delete_message(&db, row.id).await;
        }
    }
    let _ = repo::delete_mailbox(&db, &address).await;
    let _ = repo::delete_mailbox(&db, &friend).await;
    let _ = repo::delete_mailbox(&db, &blind).await;
    let _ = entity::domain::Entity::delete_many()
        .filter(entity::domain::Column::Name.eq(domain))
        .exec(&db)
        .await;
}

/// A reply composed here goes out threaded and stamped with our own hostname: the
/// conversation headers the composer stored on the draft ride along with the send,
/// and the Message-ID is built on MAIL_HOSTNAME rather than the pod name.
#[tokio::test]
async fn sends_a_reply_with_its_conversation_headers() {
    if std::env::var("MAIL_TEST_INTEGRATION").is_err() {
        eprintln!("skipping: set MAIL_TEST_INTEGRATION=1 with the compose stack running");
        return;
    }

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    let config = Arc::new(test_config());
    let db = Database::connect(&config.database_url)
        .await
        .expect("connect");
    Migrator::up(&db, None).await.expect("migrate");
    let store = MailStore::new(&config.s3).expect("object store");

    let domain = format!("reply-{}.example", Uuid::now_v7().simple());
    repo::ensure_domain(&db, &domain).await.unwrap();
    let address = format!("sender@{domain}");
    let mailbox = repo::create_mailbox(&db, &address, "secret").await.unwrap();
    let friend = format!("friend@{domain}");
    let friend_mailbox = repo::create_mailbox(&db, &friend, "secret").await.unwrap();

    let state = AppState {
        db: db.clone(),
        config: config.clone(),
        store: store.clone(),
        authenticator: None,
        tls: None,
        events: tokio::sync::broadcast::channel(16).0,
    };

    // Without an API key the caller is the operator, which is how the fixtures run.
    let call = |request: Request<Body>| {
        let app = crate::api::router(state.clone());
        async move {
            let response = app.oneshot(request).await.unwrap();
            let status = response.status();
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            (status, String::from_utf8_lossy(&bytes).to_string())
        }
    };

    // What the composer stores when the user replies.
    let draft = call(
        Request::builder()
            .method("POST")
            .uri("/api/drafts")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "from": address,
                    "to": friend,
                    "subject": "Re: Integration test",
                    "text": "a reply",
                    "in_reply_to": "root@remote.test",
                    "references_header": "root@remote.test",
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(draft.0, StatusCode::CREATED, "{}", draft.1);
    let draft: serde_json::Value = serde_json::from_str(&draft.1).expect("a draft body");
    let draft_id = draft["data"]["id"]
        .as_str()
        .expect("the draft id")
        .to_string();

    // Sending it carries those headers onto the wire, and names itself on the
    // hostname this server is configured with.
    let sent = call(
        Request::builder()
            .method("POST")
            .uri("/api/send")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "from": address,
                    "to": [friend],
                    "subject": "Re: Integration test",
                    "text": "a reply",
                    "draft_id": draft_id,
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(sent.0, StatusCode::ACCEPTED, "{}", sent.1);

    // A reply the composer has not saved as a draft carries the headers on the
    // request itself, which is the path a quick Send takes.
    let again = call(
        Request::builder()
            .method("POST")
            .uri("/api/send")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "from": address,
                    "to": [friend],
                    "subject": "Re: Integration test",
                    "text": "a reply with no draft",
                    "in_reply_to": "root@remote.test",
                    "references_header": "root@remote.test",
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(again.0, StatusCode::ACCEPTED, "{}", again.1);

    let copy = message::Entity::find()
        .filter(message::Column::MailboxId.eq(mailbox.id))
        .all(&db)
        .await
        .unwrap();
    assert_eq!(copy.len(), 2, "the sender keeps both replies");
    let mut ids = Vec::new();
    for row in &copy {
        let raw = store
            .get(&row.storage_key)
            .await
            .expect("read the sent copy");
        let text = String::from_utf8_lossy(&raw).to_string();
        assert!(text.contains("In-Reply-To: <root@remote.test>"), "{text}");
        assert!(text.contains("References: <root@remote.test>"), "{text}");
        let message_id = text
            .lines()
            .find_map(|line| line.strip_prefix("Message-ID: "))
            .map(|value| value.trim().to_string())
            .expect("a Message-ID");
        assert!(
            message_id.ends_with("@mx.example.test>"),
            "the configured hostname, not the pod name: {message_id}"
        );
        ids.push(message_id);
    }
    assert_ne!(ids[0], ids[1], "each message names itself");

    // Clean up the fixtures.
    for id in [mailbox.id, friend_mailbox.id] {
        let stored = message::Entity::find()
            .filter(message::Column::MailboxId.eq(id))
            .all(&db)
            .await
            .unwrap();
        for row in &stored {
            let _ = store.delete(&row.storage_key).await;
            let _ = repo::delete_message(&db, row.id).await;
        }
    }
    let _ = repo::delete_mailbox(&db, &address).await;
    let _ = repo::delete_mailbox(&db, &friend).await;
    let _ = entity::domain::Entity::delete_many()
        .filter(entity::domain::Column::Name.eq(domain))
        .exec(&db)
        .await;
}

/// A full mailbox refuses further inbound mail with a temporary failure and
/// stores nothing, while a message that still fits is accepted and counted.
#[tokio::test]
async fn enforces_the_mailbox_storage_quota() {
    if std::env::var("MAIL_TEST_INTEGRATION").is_err() {
        eprintln!("skipping: set MAIL_TEST_INTEGRATION=1 with the compose stack running");
        return;
    }

    let config = Arc::new(test_config());
    let db = Database::connect(&config.database_url)
        .await
        .expect("connect");
    Migrator::up(&db, None).await.expect("migrate");
    let store = MailStore::new(&config.s3).expect("object store");

    let domain = format!("quota-{}.example", Uuid::now_v7().simple());
    repo::ensure_domain(&db, &domain).await.unwrap();
    let address = format!("full@{domain}");
    let mailbox = repo::create_mailbox(&db, &address, "secret").await.unwrap();
    // A deliberately tiny quota so a normal message overshoots it.
    repo::update_mailbox(
        &db,
        &address,
        repo::MailboxUpdate {
            quota_bytes: Some(400),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let state = AppState {
        db: db.clone(),
        config: config.clone(),
        store: store.clone(),
        authenticator: None,
        tls: None,
        events: tokio::sync::broadcast::channel(16).0,
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(smtp::serve(listener, state.clone(), Mode::Inbound));

    let stream = TcpStream::connect(addr).await.unwrap();
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);

    assert_eq!(read_reply(&mut reader).await, 220);
    command(&mut write_half, "EHLO client.example.test").await;
    assert_eq!(read_reply(&mut reader).await, 250);

    // A message larger than the quota is temp-failed and leaves nothing stored.
    command(&mut write_half, "MAIL FROM:<sender@remote.test>").await;
    assert_eq!(read_reply(&mut reader).await, 250);
    command(&mut write_half, &format!("RCPT TO:<{address}>")).await;
    assert_eq!(read_reply(&mut reader).await, 250);
    command(&mut write_half, "DATA").await;
    assert_eq!(read_reply(&mut reader).await, 354);
    let filler = "x".repeat(600);
    write_half
        .write_all(&wire(&[
            "From: Alice <alice@remote.test>",
            "To: full",
            "Subject: Too big",
            "",
            filler.as_str(),
            ".",
        ]))
        .await
        .unwrap();
    write_half.flush().await.unwrap();
    assert_eq!(read_reply(&mut reader).await, 451);
    assert_eq!(repo::mailbox_usage_bytes(&db, mailbox.id).await.unwrap(), 0);

    // A message that fits is accepted and adds to the usage.
    command(&mut write_half, "MAIL FROM:<sender@remote.test>").await;
    assert_eq!(read_reply(&mut reader).await, 250);
    command(&mut write_half, &format!("RCPT TO:<{address}>")).await;
    assert_eq!(read_reply(&mut reader).await, 250);
    command(&mut write_half, "DATA").await;
    assert_eq!(read_reply(&mut reader).await, 354);
    write_half
        .write_all(&wire(&[
            "From: Alice <alice@remote.test>",
            "To: full",
            "Subject: Fits",
            "",
            "hi",
            ".",
        ]))
        .await
        .unwrap();
    write_half.flush().await.unwrap();
    assert_eq!(read_reply(&mut reader).await, 250);
    assert!(repo::mailbox_usage_bytes(&db, mailbox.id).await.unwrap() > 0);

    command(&mut write_half, "QUIT").await;
    assert_eq!(read_reply(&mut reader).await, 221);

    // Clean up the fixtures.
    let stored = message::Entity::find()
        .filter(message::Column::MailboxId.eq(mailbox.id))
        .all(&db)
        .await
        .unwrap();
    for message in &stored {
        let _ = store.delete(&message.storage_key).await;
        let _ = repo::delete_message(&db, message.id).await;
    }
    let _ = repo::delete_mailbox(&db, &address).await;
    let _ = entity::domain::Entity::delete_many()
        .filter(entity::domain::Column::Name.eq(domain))
        .exec(&db)
        .await;
}

fn base64_plain(address: &str, password: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(format!("\0{address}\0{password}"))
}

/// Builds a CRLF-terminated wire block from logical lines.
fn wire(lines: &[&str]) -> Vec<u8> {
    let mut out = Vec::new();
    for line in lines {
        out.extend_from_slice(line.as_bytes());
        out.extend_from_slice(&CRLF);
    }
    out
}

async fn command(writer: &mut (impl AsyncWriteExt + Unpin), line: &str) {
    writer.write_all(line.as_bytes()).await.unwrap();
    writer.write_all(&CRLF).await.unwrap();
    writer.flush().await.unwrap();
}

/// Reads a reply, folding continuation lines, and returns the status code.
async fn read_reply(reader: &mut (impl AsyncBufReadExt + Unpin)) -> u16 {
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line).await.unwrap();
        assert!(read > 0, "connection closed before a reply arrived");
        let bytes = line.as_bytes();
        assert!(bytes.len() >= 3, "malformed reply: {line:?}");
        let code: u16 = line[..3].parse().expect("reply code");
        if bytes.len() < 4 || bytes[3] == b' ' {
            return code;
        }
    }
}

/// Session auth end to end: the operator key manages every tenant, a mailbox
/// session reaches only its own mailbox, and cookie writes need the CSRF token.
/// Requires the compose stack and MAIL_TEST_INTEGRATION=1.
#[tokio::test]
async fn sessions_scope_a_mailbox_and_the_operator_key_manages_tenants() {
    if std::env::var("MAIL_TEST_INTEGRATION").is_err() {
        eprintln!("skipping: set MAIL_TEST_INTEGRATION=1 with the compose stack running");
        return;
    }

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    let mut config = test_config();
    config.api_key = Some("test-key-123".to_string());
    let config = Arc::new(config);

    let db = Database::connect(&config.database_url).await.unwrap();
    let store = MailStore::new(&config.s3).unwrap();
    let state = AppState {
        db: db.clone(),
        config: config.clone(),
        store,
        authenticator: None,
        tls: None,
        events: tokio::sync::broadcast::channel(16).0,
    };

    let domain = format!("it-{}.test", Uuid::now_v7().simple());
    repo::ensure_domain(&db, &domain).await.unwrap();
    let alpha = format!("alpha@{domain}");
    let beta = format!("beta@{domain}");
    repo::create_mailbox(&db, &alpha, "secret").await.unwrap();
    repo::create_mailbox(&db, &beta, "secret").await.unwrap();

    let send = |request: Request<Body>| {
        let app = crate::api::router(state.clone());
        async move {
            let response = app.oneshot(request).await.unwrap();
            let status = response.status();
            let headers = response.headers().clone();
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            (status, headers, String::from_utf8_lossy(&bytes).to_string())
        }
    };

    // Without a key, the tenant endpoints are closed.
    let anonymous = Request::builder()
        .method("GET")
        .uri("/api/domains")
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(anonymous).await.0, StatusCode::UNAUTHORIZED);

    // CORS is off: a browser on another origin gets no access-control headers.
    let cross_origin = Request::builder()
        .method("GET")
        .uri("/api/session")
        .header("origin", "https://client.example")
        .body(Body::empty())
        .unwrap();
    let (_, headers, _) = send(cross_origin).await;
    assert!(
        headers.get("access-control-allow-origin").is_none(),
        "the API must not answer cross-origin requests"
    );

    let login_body =
        |password: &str| serde_json::json!({ "address": alpha, "password": password }).to_string();

    // A wrong password is refused.
    let wrong = Request::builder()
        .method("POST")
        .uri("/api/session")
        .header("content-type", "application/json")
        .body(Body::from(login_body("nope")))
        .unwrap();
    assert_eq!(send(wrong).await.0, StatusCode::UNAUTHORIZED);

    // The right password sets the session and CSRF cookies.
    let login = Request::builder()
        .method("POST")
        .uri("/api/session")
        .header("content-type", "application/json")
        .body(Body::from(login_body("secret")))
        .unwrap();
    let (status, headers, _) = send(login).await;
    assert_eq!(status, StatusCode::OK);
    let cookies = headers
        .get_all("set-cookie")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .collect::<Vec<_>>()
        .join("; ");
    let session = crate::session::cookie_value(&cookies, crate::session::SESSION_COOKIE)
        .expect("session cookie");
    let csrf =
        crate::session::cookie_value(&cookies, crate::session::CSRF_COOKIE).expect("csrf cookie");
    let cookie_header = format!(
        "{}={}; {}={}",
        crate::session::SESSION_COOKIE,
        session,
        crate::session::CSRF_COOKIE,
        csrf
    );

    let with_session = |method: &'static str, path: String| {
        Request::builder()
            .method(method)
            .uri(path)
            .header("cookie", cookie_header.clone())
            .body(Body::empty())
            .unwrap()
    };

    // The session sees only its own mailbox.
    let (status, _, body) = send(with_session("GET", "/api/mailboxes".to_string())).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains(&alpha), "{body}");
    assert!(!body.contains(&beta), "{body}");

    // A session is not an operator.
    assert_eq!(
        send(with_session("GET", "/api/domains".to_string()))
            .await
            .0,
        StatusCode::FORBIDDEN
    );

    // Cookie writes need the CSRF token, and may only touch the own mailbox.
    let patch_mailbox = |address: &str, with_csrf: bool| {
        let mut builder = Request::builder()
            .method("PATCH")
            .uri(format!("/api/mailboxes/{address}"))
            .header("content-type", "application/json")
            .header("cookie", cookie_header.clone());
        if with_csrf {
            builder = builder.header("x-csrf-token", csrf.clone());
        }
        builder
            .body(Body::from(
                serde_json::json!({ "signature": "bye" }).to_string(),
            ))
            .unwrap()
    };
    assert_eq!(
        send(patch_mailbox(&alpha, false)).await.0,
        StatusCode::FORBIDDEN,
        "a write without the CSRF token is rejected"
    );
    assert_eq!(send(patch_mailbox(&alpha, true)).await.0, StatusCode::OK);
    assert_eq!(
        send(patch_mailbox(&beta, true)).await.0,
        StatusCode::NOT_FOUND,
        "another tenant mailbox is invisible"
    );

    // The operator key still manages the tenant, including deactivation.
    let deactivate = Request::builder()
        .method("PATCH")
        .uri(format!("/api/domains/{domain}"))
        .header("content-type", "application/json")
        .header("authorization", "Bearer test-key-123")
        .body(Body::from(
            serde_json::json!({ "active": false }).to_string(),
        ))
        .unwrap();
    let (status, _, body) = send(deactivate).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("\"active\":false"), "{body}");

    // Clean up the fixtures.
    let _ = repo::delete_mailbox(&db, &alpha).await;
    let _ = repo::delete_mailbox(&db, &beta).await;
    let _ = entity::domain::Entity::delete_many()
        .filter(entity::domain::Column::Name.eq(domain))
        .exec(&db)
        .await;
}

/// The neighbour walk follows the reader's order inside one folder: the unread
/// messages newest first, then the read ones newest first, and a message in
/// another folder is never walked. Postgres alone is enough:
///
///     MAIL_TEST_INTEGRATION=1 cargo test --all neighbors_walk
#[tokio::test]
async fn neighbors_walk_the_reading_order_within_a_folder() {
    if std::env::var("MAIL_TEST_INTEGRATION").is_err() {
        eprintln!("skipping: set MAIL_TEST_INTEGRATION=1 with the compose stack running");
        return;
    }

    let config = test_config();
    let db = Database::connect(&config.database_url)
        .await
        .expect("connect");
    Migrator::up(&db, None).await.expect("migrate");

    let domain = format!("neighbors-{}.example", Uuid::now_v7().simple());
    repo::ensure_domain(&db, &domain).await.unwrap();
    let mailbox = repo::create_mailbox(&db, &format!("inbox@{domain}"), "secret")
        .await
        .unwrap();

    // Two unread arrivals (a newer than b) and two read ones (d newer than c),
    // plus a sent copy and a quarantined message that live in other folders.
    let a = insert_message(
        &db,
        &mailbox,
        message::Direction::Inbound,
        message::Status::Stored,
        5,
    )
    .await;
    let b = insert_message(
        &db,
        &mailbox,
        message::Direction::Inbound,
        message::Status::Stored,
        4,
    )
    .await;
    let c = insert_message(
        &db,
        &mailbox,
        message::Direction::Inbound,
        message::Status::Stored,
        3,
    )
    .await;
    let d = insert_message(
        &db,
        &mailbox,
        message::Direction::Inbound,
        message::Status::Stored,
        2,
    )
    .await;
    for id in [c, d] {
        set_flag(&db, id, true, false, "inbox", None).await;
    }
    let sent = insert_message(
        &db,
        &mailbox,
        message::Direction::Outbound,
        message::Status::Stored,
        0,
    )
    .await;
    let spam = insert_message(
        &db,
        &mailbox,
        message::Direction::Inbound,
        message::Status::Quarantined,
        1,
    )
    .await;
    let fixtures = [a, b, c, d, sent, spam];

    let filters = query::MessageFilters {
        mailbox_id: Some(mailbox.id),
        folder: Some("inbox".to_string()),
        ..query::MessageFilters::default()
    };
    async fn walk(
        db: &sea_orm::DatabaseConnection,
        filters: &query::MessageFilters,
        id: Uuid,
        unread: bool,
    ) -> query::Neighbors {
        let row = repo::get_message(db, id).await.unwrap().unwrap();
        query::neighbors(
            db,
            filters,
            query::MessageCursor {
                received_at: row.received_at,
                id: row.id,
            },
            unread,
        )
        .await
        .unwrap()
    }

    let a_n = walk(&db, &filters, a, true).await;
    assert_eq!(a_n.total, 4, "only the four inbox messages are walked");
    assert_eq!(a_n.unread_total, 2);
    assert_eq!(a_n.position, 1);
    assert_eq!(a_n.unread_position, 1);
    assert_eq!(a_n.previous_id, None, "the newest unread leads the order");
    assert_eq!(a_n.next_id, Some(b));

    let b_n = walk(&db, &filters, b, true).await;
    assert_eq!(b_n.position, 2);
    assert_eq!(b_n.previous_id, Some(a));
    assert_eq!(
        b_n.next_id,
        Some(d),
        "the last unread hands over to the newest read"
    );

    let d_n = walk(&db, &filters, d, false).await;
    assert_eq!(d_n.position, 3);
    assert_eq!(
        d_n.unread_position, 0,
        "a read message has no unread position"
    );
    assert_eq!(
        d_n.previous_id,
        Some(b),
        "the newest read follows the oldest unread"
    );
    assert_eq!(d_n.next_id, Some(c));

    let c_n = walk(&db, &filters, c, false).await;
    assert_eq!(c_n.position, 4);
    assert_eq!(c_n.previous_id, Some(d));
    assert_eq!(c_n.next_id, None, "the oldest read ends the order");

    for id in fixtures {
        repo::delete_message(&db, id).await.unwrap();
    }
    let _ = repo::delete_mailbox(&db, &mailbox.address).await;
    let _ = entity::domain::Entity::delete_many()
        .filter(entity::domain::Column::Name.eq(domain))
        .exec(&db)
        .await;
}

/// The folder classification, the counts and the keyset pages all read the same
/// reader state, and a copy in the sender's own Sent folder is never unread.
/// Postgres alone is enough:
///
///     MAIL_TEST_INTEGRATION=1 cargo test --all keyset_and_counts
#[tokio::test]
async fn keyset_and_counts_follow_the_reader_state() {
    if std::env::var("MAIL_TEST_INTEGRATION").is_err() {
        eprintln!("skipping: set MAIL_TEST_INTEGRATION=1 with the compose stack running");
        return;
    }

    let config = test_config();
    let db = Database::connect(&config.database_url)
        .await
        .expect("connect");
    Migrator::up(&db, None).await.expect("migrate");

    let domain = format!("keyset-{}.example", Uuid::now_v7().simple());
    repo::ensure_domain(&db, &domain).await.unwrap();
    let mailbox = repo::create_mailbox(&db, &format!("inbox@{domain}"), "secret")
        .await
        .unwrap();

    // Three arrivals, a quarantined one, the sender's own copy, a draft, a
    // snoozed one and a starred one, oldest last.
    // Every fixture row is tracked, because deleting the mailbox leaves the
    // messages behind with a null mailbox_id.
    let mut fixtures = Vec::new();
    for minutes_ago in [5, 4, 3] {
        fixtures.push(
            insert_message(
                &db,
                &mailbox,
                message::Direction::Inbound,
                message::Status::Stored,
                minutes_ago,
            )
            .await,
        );
    }
    let quarantined = insert_message(
        &db,
        &mailbox,
        message::Direction::Inbound,
        message::Status::Quarantined,
        2,
    )
    .await;
    fixtures.push(quarantined);
    let sent = insert_message(
        &db,
        &mailbox,
        message::Direction::Outbound,
        message::Status::Stored,
        0,
    )
    .await;
    fixtures.push(sent);
    let draft = insert_message(
        &db,
        &mailbox,
        message::Direction::Inbound,
        message::Status::Stored,
        6,
    )
    .await;
    fixtures.push(draft);
    set_flag(&db, draft, true, false, "drafts", None).await;
    let snoozed = insert_message(
        &db,
        &mailbox,
        message::Direction::Inbound,
        message::Status::Stored,
        7,
    )
    .await;
    fixtures.push(snoozed);
    set_flag(
        &db,
        snoozed,
        false,
        false,
        "inbox",
        Some(Utc::now() + chrono::Duration::hours(2)),
    )
    .await;
    let starred = insert_message(
        &db,
        &mailbox,
        message::Direction::Inbound,
        message::Status::Stored,
        8,
    )
    .await;
    fixtures.push(starred);
    set_flag(&db, starred, false, true, "inbox", None).await;

    let counts = query::folder_counts(&db, Some(mailbox.id)).await.unwrap();
    let folder = |name: &str| counts.folders.get(name).copied().unwrap_or((0, 0));
    assert_eq!(
        folder("inbox"),
        (4, 4),
        "arrivals, the starred one included"
    );
    assert_eq!(folder("spam"), (1, 1));
    assert_eq!(folder("drafts"), (1, 0), "a draft is never unread");
    assert_eq!(folder("snoozed"), (1, 1));
    assert_eq!(
        folder("sent"),
        (1, 0),
        "the sender's own copy is not unread"
    );
    assert_eq!(counts.starred, (1, 1));
    assert_eq!(counts.mailboxes.get(&mailbox.id), Some(&(8, 6)));

    // The pages walk the mailbox newest first and stop at the end.
    let filters = query::MessageFilters {
        mailbox_id: Some(mailbox.id),
        ..query::MessageFilters::default()
    };
    let mut seen: Vec<Uuid> = Vec::new();
    let mut cursor = None;
    for _ in 0..4 {
        let page = query::message_page(&db, &filters, cursor, 3).await.unwrap();
        assert_eq!(page.total, 8);
        seen.extend(page.ids.iter().copied());
        match page.next {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(seen.len(), 8, "every message is on exactly one page");
    let mut unique = seen.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), 8, "no page repeats a message");
    assert_eq!(seen[0], sent, "the newest message leads the first page");
    assert_eq!(seen[7], starred, "the oldest message ends the last page");

    // The unread filter and the navigation order agree with the counts.
    let unread = query::navigation(
        &db,
        &query::MessageFilters {
            read: Some("unread".to_string()),
            ..filters.clone()
        },
        query::NAVIGATION_CAP,
    )
    .await
    .unwrap();
    assert!(
        !unread.iter().any(|(id, _)| *id == sent || *id == draft),
        "neither the sent copy nor the draft is unread"
    );
    assert_eq!(unread.iter().filter(|(_, unread)| *unread).count(), 6);
    assert!(unread.iter().any(|(id, _)| *id == quarantined));

    for id in fixtures {
        repo::delete_message(&db, id).await.unwrap();
    }
    let _ = repo::delete_mailbox(&db, &mailbox.address).await;
    let _ = entity::domain::Entity::delete_many()
        .filter(entity::domain::Column::Name.eq(domain))
        .exec(&db)
        .await;
}

/// A queued copy reports the state of the job carrying it, and a scheduled one
/// also reports when it goes out. Postgres alone is enough:
///
///     MAIL_TEST_INTEGRATION=1 cargo test --all queued_copy
#[tokio::test]
async fn a_queued_copy_reports_its_send_state() {
    if std::env::var("MAIL_TEST_INTEGRATION").is_err() {
        eprintln!("skipping: set MAIL_TEST_INTEGRATION=1 with the compose stack running");
        return;
    }

    let config = test_config();
    let db = Database::connect(&config.database_url)
        .await
        .expect("connect");
    Migrator::up(&db, None).await.expect("migrate");

    let domain = format!("queue-{}.example", Uuid::now_v7().simple());
    repo::ensure_domain(&db, &domain).await.unwrap();
    let mailbox = repo::create_mailbox(&db, &format!("inbox@{domain}"), "secret")
        .await
        .unwrap();

    let waiting = insert_message(
        &db,
        &mailbox,
        message::Direction::Outbound,
        message::Status::Stored,
        0,
    )
    .await;
    let refused = insert_message(
        &db,
        &mailbox,
        message::Direction::Outbound,
        message::Status::Stored,
        1,
    )
    .await;
    let later = insert_message(
        &db,
        &mailbox,
        message::Direction::Outbound,
        message::Status::Stored,
        2,
    )
    .await;
    let untouched = insert_message(
        &db,
        &mailbox,
        message::Direction::Inbound,
        message::Status::Stored,
        3,
    )
    .await;

    let due = chrono::DateTime::from_timestamp(1_800_000_000, 0).unwrap();
    let waiting_job = queue_job(
        &db,
        &mailbox,
        waiting,
        outbound_job::JobStatus::Pending,
        None,
        None,
    )
    .await;
    let refused_job = queue_job(
        &db,
        &mailbox,
        refused,
        outbound_job::JobStatus::Failed,
        Some("550 rejected by the relay"),
        None,
    )
    .await;
    let later_job = queue_job(
        &db,
        &mailbox,
        later,
        outbound_job::JobStatus::Pending,
        None,
        Some(due),
    )
    .await;

    let ids = vec![waiting, refused, later, untouched];
    let states = query::send_states(&db, &ids).await.unwrap();
    assert_eq!(
        states.get(&waiting).map(|state| state.state),
        Some("queued")
    );
    let failed = states.get(&refused).expect("the refused copy has a job");
    assert_eq!(failed.state, "failed");
    assert_eq!(failed.error.as_deref(), Some("550 rejected by the relay"));
    assert_eq!(states.get(&later).map(|state| state.state), Some("queued"));
    assert!(
        !states.contains_key(&untouched),
        "an arrival nothing queued carries no send state"
    );

    let times = query::scheduled_times(&db, &ids).await.unwrap();
    assert_eq!(times.get(&later), Some(&due));
    assert_eq!(times.len(), 1, "only the scheduled copy has a time");
    assert!(
        !times.contains_key(&refused),
        "a failed job is not a schedule"
    );

    for id in ids {
        repo::delete_message(&db, id).await.unwrap();
    }
    for job in [waiting_job, refused_job, later_job] {
        let _ = outbound_job::Entity::delete_by_id(job).exec(&db).await;
    }
    let _ = repo::delete_mailbox(&db, &mailbox.address).await;
    let _ = entity::domain::Entity::delete_many()
        .filter(entity::domain::Column::Name.eq(domain))
        .exec(&db)
        .await;
}

/// Inbound mail for a mailbox with an auto-forward address is queued once to
/// that address, and a copy that already carries the forward marker is not
/// forwarded again. Needs the compose stack:
///
///     MAIL_TEST_INTEGRATION=1 cargo test --all auto_forwards
#[tokio::test]
async fn auto_forwards_an_arriving_message_once() {
    if std::env::var("MAIL_TEST_INTEGRATION").is_err() {
        eprintln!("skipping: set MAIL_TEST_INTEGRATION=1 with the compose stack running");
        return;
    }

    let config = Arc::new(test_config());
    let db = Database::connect(&config.database_url)
        .await
        .expect("connect");
    Migrator::up(&db, None).await.expect("migrate");
    let store = MailStore::new(&config.s3).expect("object store");

    let domain = format!("forward-{}.example", Uuid::now_v7().simple());
    repo::ensure_domain(&db, &domain).await.unwrap();
    let address = format!("inbox@{domain}");
    let mailbox = repo::create_mailbox(&db, &address, "secret").await.unwrap();
    repo::update_mailbox(
        &db,
        &address,
        repo::MailboxUpdate {
            forward_to: Some(Some("dest@remote.test".to_string())),
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .expect("the mailbox exists");

    let state = AppState {
        db: db.clone(),
        config: config.clone(),
        store: store.clone(),
        authenticator: None,
        tls: None,
        events: tokio::sync::broadcast::channel(16).0,
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(smtp::serve(listener, state.clone(), Mode::Inbound));

    deliver_inbound(
        addr,
        "sender@remote.test",
        &address,
        &[
            "From: sender@remote.test",
            "Subject: hello",
            "Bcc: blind@remote.test",
            "",
            "body line",
        ],
    )
    .await;
    // A message that already came through the forward is delivered but not sent
    // on again, so a loop between two mailboxes cannot run forever.
    let marker = format!("X-Copod-Auto-Forwarded: {address}");
    deliver_inbound(
        addr,
        "sender@remote.test",
        &address,
        &[&marker, "Subject: looped", "", "body line"],
    )
    .await;

    let jobs = outbound_job::Entity::find()
        .filter(outbound_job::Column::MailboxId.eq(mailbox.id))
        .all(&db)
        .await
        .unwrap();
    assert_eq!(jobs.len(), 1, "only the unmarked arrival is forwarded");
    let job = &jobs[0];
    let recipients: Vec<String> = serde_json::from_value(job.recipients.clone()).unwrap();
    assert_eq!(recipients, vec!["dest@remote.test".to_string()]);

    // The envelope sender is rewritten with SRS, which is what the relay asks of
    // forwarded mail, and it decodes back to the sender a bounce belongs to.
    let (local_part, srs_domain) = job
        .envelope_from
        .split_once('@')
        .expect("a rewritten envelope sender");
    assert_eq!(srs_domain, domain);
    let key = mailcore::srs::key(&config.session_secret);
    assert_eq!(
        mailcore::srs::decode(local_part, srs_domain, &key, chrono::Utc::now().timestamp())
            .as_deref(),
        Some("sender@remote.test")
    );

    // The message itself goes out with its own headers, so its own DKIM signature
    // survives; the marker is what stops a copy that loops back from being sent on
    // again, and a Bcc header the arrival leaked is dropped on the way out.
    let payload = store
        .get(&job.storage_key)
        .await
        .expect("forwarded payload");
    let text = String::from_utf8_lossy(&payload);
    assert!(
        text.starts_with("X-Copod-Auto-Forwarded: "),
        "the copy is marked so it cannot loop: {text}"
    );
    assert!(
        text.contains("From: sender@remote.test"),
        "the forwarded message keeps its own headers: {text}"
    );
    assert!(text.contains("body line"));
    assert!(
        !text.to_lowercase().contains("bcc"),
        "a forwarded copy must not name blind recipients: {text}"
    );

    // Clean up the fixtures, including the stored arrivals and the forwarded copy.
    let arrivals = message::Entity::find()
        .filter(message::Column::MailboxId.eq(mailbox.id))
        .all(&db)
        .await
        .unwrap();
    for row in &arrivals {
        let _ = store.delete(&row.storage_key).await;
    }
    let _ = store.delete(&job.storage_key).await;
    let _ = outbound_job::Entity::delete_by_id(job.id).exec(&db).await;
    for row in arrivals {
        let _ = repo::delete_message(&db, row.id).await;
    }
    let _ = repo::delete_mailbox(&db, &address).await;
    let _ = entity::domain::Entity::delete_many()
        .filter(entity::domain::Column::Name.eq(domain))
        .exec(&db)
        .await;
}

/// Saving a job keeps the envelope it was called with: the queue rewrites a
/// refused forward's sender and has to be able to record the new one. Needs the
/// compose stack and MAIL_TEST_INTEGRATION=1.
#[tokio::test]
async fn saving_a_job_records_the_envelope_it_was_given() {
    if std::env::var("MAIL_TEST_INTEGRATION").is_err() {
        eprintln!("skipping: set MAIL_TEST_INTEGRATION=1 with the compose stack running");
        return;
    }

    let config = test_config();
    let db = Database::connect(&config.database_url)
        .await
        .expect("connect");
    Migrator::up(&db, None).await.expect("migrate");

    let domain = format!("job-{}.example", Uuid::now_v7().simple());
    repo::ensure_domain(&db, &domain).await.unwrap();
    let mailbox = repo::create_mailbox(&db, &format!("inbox@{domain}"), "secret")
        .await
        .unwrap();

    let id = Uuid::now_v7();
    let now = Utc::now();
    repo::enqueue_job(
        &db,
        outbound_job::ActiveModel {
            id: Set(id),
            mailbox_id: Set(Some(mailbox.id)),
            envelope_from: Set(format!("SRS0=AAAA=11=remote.test=someone@{domain}")),
            recipients: Set(serde_json::json!(["dest@remote.test"])),
            storage_key: Set(format!("fixtures/job-{id}")),
            message_id: Set(None),
            scheduled_message_id: Set(None),
            copy_message_id: Set(None),
            attempts: Set(0),
            next_attempt_at: Set(now),
            status: Set(outbound_job::JobStatus::Pending),
            last_error: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        },
    )
    .await
    .unwrap();

    repo::save_job(
        &db,
        id,
        &mailbox.address,
        outbound_job::JobStatus::Pending,
        0,
        now,
        None,
    )
    .await
    .unwrap();

    let saved = outbound_job::Entity::find_by_id(id)
        .one(&db)
        .await
        .unwrap()
        .expect("the job");
    assert_eq!(saved.envelope_from, mailbox.address);

    let _ = outbound_job::Entity::delete_by_id(id).exec(&db).await;
    let _ = repo::delete_mailbox(&db, &mailbox.address).await;
    let _ = entity::domain::Entity::delete_many()
        .filter(entity::domain::Column::Name.eq(domain))
        .exec(&db)
        .await;
}

/// Delivers one message to `recipient` over a fresh inbound session.
async fn deliver_inbound(addr: std::net::SocketAddr, from: &str, recipient: &str, lines: &[&str]) {
    let stream = TcpStream::connect(addr).await.unwrap();
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);
    assert_eq!(read_reply(&mut reader).await, 220);
    command(&mut write_half, "EHLO client.example.test").await;
    assert_eq!(read_reply(&mut reader).await, 250);
    command(&mut write_half, &format!("MAIL FROM:<{from}>")).await;
    assert_eq!(read_reply(&mut reader).await, 250);
    command(&mut write_half, &format!("RCPT TO:<{recipient}>")).await;
    assert_eq!(read_reply(&mut reader).await, 250);
    command(&mut write_half, "DATA").await;
    assert_eq!(read_reply(&mut reader).await, 354);
    let mut body = lines.to_vec();
    body.push(".");
    write_half.write_all(&wire(&body)).await.unwrap();
    write_half.flush().await.unwrap();
    assert_eq!(read_reply(&mut reader).await, 250);
    command(&mut write_half, "QUIT").await;
    assert_eq!(read_reply(&mut reader).await, 221);
}

/// The attachment endpoint serves a renderable part inline on a preview request
/// and nothing else: HTML stays a download so a preview URL cannot run script.
/// Needs the compose stack and MAIL_TEST_INTEGRATION=1.
#[tokio::test]
async fn preview_serves_only_renderable_attachments_inline() {
    if std::env::var("MAIL_TEST_INTEGRATION").is_err() {
        eprintln!("skipping: set MAIL_TEST_INTEGRATION=1 with the compose stack running");
        return;
    }

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    let config = Arc::new(test_config());
    let db = Database::connect(&config.database_url)
        .await
        .expect("connect");
    Migrator::up(&db, None).await.expect("migrate");
    let store = MailStore::new(&config.s3).expect("object store");

    let state = AppState {
        db: db.clone(),
        config: config.clone(),
        store: store.clone(),
        authenticator: None,
        tls: None,
        events: tokio::sync::broadcast::channel(16).0,
    };

    let domain = format!("preview-{}.example", Uuid::now_v7().simple());
    repo::ensure_domain(&db, &domain).await.unwrap();
    let mailbox = repo::create_mailbox(&db, &format!("inbox@{domain}"), "secret")
        .await
        .unwrap();

    let raw = concat!(
        "From: sender@remote.test\r\n",
        "To: inbox\r\n",
        "Subject: preview\r\n",
        "MIME-Version: 1.0\r\n",
        "Content-Type: multipart/mixed; boundary=xyz\r\n",
        "\r\n",
        "--xyz\r\n",
        "Content-Type: text/html; name=page.html\r\n",
        "Content-Disposition: attachment; filename=page.html\r\n",
        "\r\n",
        "<script>alert(1)</script>\r\n",
        "--xyz\r\n",
        "Content-Type: application/pdf; name=doc.pdf\r\n",
        "Content-Disposition: attachment; filename=doc.pdf\r\n",
        "\r\n",
        "%PDF-1.4\r\n",
        "--xyz\r\n",
        "Content-Type: image/png; name=logo.png\r\n",
        "Content-Disposition: inline; filename=logo.png\r\n",
        "Content-Transfer-Encoding: base64\r\n",
        "Content-ID: <logo@remote.test>\r\n",
        "\r\n",
        "aGk=\r\n",
        "--xyz--\r\n",
    )
    .as_bytes()
    .to_vec();

    let id = Uuid::now_v7();
    let key = format!("fixtures/preview-{id}.eml");
    store.put(&key, raw.clone()).await.unwrap();
    repo::insert_message(
        &db,
        message::ActiveModel {
            id: Set(id),
            mailbox_id: Set(Some(mailbox.id)),
            direction: Set(message::Direction::Inbound),
            status: Set(message::Status::Stored),
            envelope_from: Set("sender@remote.test".to_string()),
            envelope_to: Set(mailbox.address.clone()),
            message_id: Set(Some(format!("<{id}@remote.test>"))),
            subject: Set(Some("preview".to_string())),
            size_bytes: Set(raw.len() as i64),
            storage_key: Set(key.clone()),
            received_at: Set(Utc::now()),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let fetch = |uri: String| {
        let app = crate::api::router(state.clone());
        async move {
            let request = Request::builder()
                .method("GET")
                .uri(uri)
                .body(Body::empty())
                .unwrap();
            let response = app.oneshot(request).await.unwrap();
            let header = |name: &str| {
                response
                    .headers()
                    .get(name)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default()
                    .to_string()
            };
            (
                response.status(),
                header("content-disposition"),
                header("content-type"),
            )
        }
    };

    let detail = |uri: String| {
        let app = crate::api::router(state.clone());
        async move {
            let request = Request::builder()
                .method("GET")
                .uri(uri)
                .body(Body::empty())
                .unwrap();
            let response = app.oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            serde_json::from_slice::<serde_json::Value>(
                &axum::body::to_bytes(response.into_body(), usize::MAX)
                    .await
                    .unwrap(),
            )
            .unwrap()
        }
    };

    // The PDF previews in place; the same part downloads without the query.
    let (status, disposition, content_type) =
        fetch(format!("/api/messages/{id}/attachments/1?preview=1")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(disposition.starts_with("inline"), "{disposition}");
    assert_eq!(content_type, "application/pdf");

    // The inline image previews too, and the message detail names the part its
    // cid: reference points at, so the reader can draw it in place.
    let (status, disposition, content_type) =
        fetch(format!("/api/messages/{id}/attachments/2?preview=1")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(disposition.starts_with("inline"), "{disposition}");
    assert_eq!(content_type, "image/png");

    let body = detail(format!("/api/messages/{id}")).await;
    let parts = body["data"]["attachments"].as_array().unwrap();
    assert_eq!(parts.len(), 3);
    assert_eq!(parts[2]["content_id"], "logo@remote.test");
    assert!(parts[0]["content_id"].is_null(), "{parts:?}");

    let (_, disposition, _) = fetch(format!("/api/messages/{id}/attachments/1")).await;
    assert!(disposition.starts_with("attachment"), "{disposition}");

    // HTML carries script, so even a preview request downloads it.
    let (_, disposition, content_type) =
        fetch(format!("/api/messages/{id}/attachments/0?preview=1")).await;
    assert!(disposition.starts_with("attachment"), "{disposition}");
    assert!(
        content_type.starts_with("text/html"),
        "index 0 is the HTML part: {content_type}"
    );

    // Clean up the fixtures.
    let _ = store.delete(&key).await;
    let _ = repo::delete_message(&db, id).await;
    let _ = repo::delete_mailbox(&db, &mailbox.address).await;
    let _ = entity::domain::Entity::delete_many()
        .filter(entity::domain::Column::Name.eq(domain))
        .exec(&db)
        .await;
}

/// Queues a job for a stored copy: the fixture does not need object storage.
/// Returns the job id so the test can take its fixture back out of the queue.
async fn queue_job(
    db: &sea_orm::DatabaseConnection,
    mailbox: &entity::mailbox::Model,
    copy_message_id: Uuid,
    status: outbound_job::JobStatus,
    last_error: Option<&str>,
    scheduled_at: Option<chrono::DateTime<Utc>>,
) -> Uuid {
    let now = Utc::now();
    let id = Uuid::now_v7();
    repo::enqueue_job(
        db,
        outbound_job::ActiveModel {
            id: Set(id),
            mailbox_id: Set(Some(mailbox.id)),
            envelope_from: Set(mailbox.address.clone()),
            recipients: Set(serde_json::json!(["remote@elsewhere.test"])),
            storage_key: Set(format!("fixtures/job-{}", Uuid::now_v7().simple())),
            message_id: Set(None),
            scheduled_message_id: Set(scheduled_at.map(|_| copy_message_id)),
            copy_message_id: Set(Some(copy_message_id)),
            attempts: Set(0),
            next_attempt_at: Set(scheduled_at.unwrap_or(now)),
            status: Set(status),
            last_error: Set(last_error.map(str::to_string)),
            created_at: Set(now),
            updated_at: Set(now),
        },
    )
    .await
    .unwrap();
    id
}

/// Stores a message row directly: the fixture does not need object storage.
async fn insert_message(
    db: &sea_orm::DatabaseConnection,
    mailbox: &entity::mailbox::Model,
    direction: message::Direction,
    status: message::Status,
    minutes_ago: i64,
) -> Uuid {
    let id = Uuid::now_v7();
    repo::insert_message(
        db,
        message::ActiveModel {
            id: Set(id),
            mailbox_id: Set(Some(mailbox.id)),
            direction: Set(direction),
            status: Set(status),
            envelope_from: Set("sender@remote.test".to_string()),
            envelope_to: Set(mailbox.address.clone()),
            message_id: Set(Some(format!("<{id}@remote.test>"))),
            subject: Set(Some(format!("message {minutes_ago}"))),
            size_bytes: Set(64),
            storage_key: Set(format!("fixtures/{id}.eml")),
            received_at: Set(Utc::now() - chrono::Duration::minutes(minutes_ago)),
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .id
}

async fn set_flag(
    db: &sea_orm::DatabaseConnection,
    message_id: Uuid,
    read: bool,
    starred: bool,
    folder: &str,
    snoozed_until: Option<chrono::DateTime<Utc>>,
) {
    entity::message_flag::ActiveModel {
        message_id: Set(message_id),
        read: Set(read),
        starred: Set(starred),
        answered: Set(false),
        deleted: Set(false),
        folder: Set(folder.to_string()),
        snoozed_until: Set(snoozed_until),
        updated_at: Set(Utc::now()),
    }
    .insert(db)
    .await
    .unwrap();
}

/// Deleting a mailbox takes its messages and their stored copies with it: a
/// message row's mailbox is nullable, so leaving them behind would hide them
/// from every reader while their objects stayed in the bucket.
#[tokio::test]
async fn deleting_a_mailbox_takes_its_messages_and_objects() {
    if std::env::var("MAIL_TEST_INTEGRATION").is_err() {
        eprintln!("skipping: set MAIL_TEST_INTEGRATION=1 with the compose stack running");
        return;
    }

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    let config = Arc::new(test_config());
    let db = Database::connect(&config.database_url)
        .await
        .expect("connect");
    Migrator::up(&db, None).await.expect("migrate");
    let store = MailStore::new(&config.s3).expect("object store");

    let domain = format!("mailbox-{}.example", Uuid::now_v7().simple());
    repo::ensure_domain(&db, &domain).await.unwrap();
    let address = format!("gone@{domain}");
    let mailbox = repo::create_mailbox(&db, &address, "secret").await.unwrap();

    let id = insert_message(
        &db,
        &mailbox,
        message::Direction::Inbound,
        message::Status::Stored,
        5,
    )
    .await;
    let key = format!("fixtures/{id}.eml");
    store.put(&key, b"message bytes".to_vec()).await.unwrap();

    let state = AppState {
        db: db.clone(),
        config: config.clone(),
        store: store.clone(),
        authenticator: None,
        tls: None,
        events: tokio::sync::broadcast::channel(16).0,
    };
    let response = crate::api::router(state.clone())
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/mailboxes/{address}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let rows = message::Entity::find()
        .filter(message::Column::MailboxId.eq(mailbox.id))
        .all(&db)
        .await
        .unwrap();
    assert!(rows.is_empty(), "the messages went with the mailbox");
    assert!(
        store.get(&key).await.is_err(),
        "and so did their stored copies"
    );

    let _ = repo::delete_mailbox(&db, &address).await;
    let _ = entity::domain::Entity::delete_many()
        .filter(entity::domain::Column::Name.eq(domain))
        .exec(&db)
        .await;
}

/// A message whose stored copy is gone answers 410, not a 500 from object
/// storage, so the reader can call the mail unavailable rather than broken.
#[tokio::test]
async fn a_message_without_its_object_reads_as_gone() {
    if std::env::var("MAIL_TEST_INTEGRATION").is_err() {
        eprintln!("skipping: set MAIL_TEST_INTEGRATION=1 with the compose stack running");
        return;
    }

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    let config = Arc::new(test_config());
    let db = Database::connect(&config.database_url)
        .await
        .expect("connect");
    Migrator::up(&db, None).await.expect("migrate");
    let store = MailStore::new(&config.s3).expect("object store");

    let domain = format!("gone-{}.example", Uuid::now_v7().simple());
    repo::ensure_domain(&db, &domain).await.unwrap();
    let address = format!("inbox@{domain}");
    let mailbox = repo::create_mailbox(&db, &address, "secret").await.unwrap();

    // A row whose object was never written, as an interrupted delivery leaves.
    let id = insert_message(
        &db,
        &mailbox,
        message::Direction::Inbound,
        message::Status::Stored,
        3,
    )
    .await;

    let state = AppState {
        db: db.clone(),
        config: config.clone(),
        store: store.clone(),
        authenticator: None,
        tls: None,
        events: tokio::sync::broadcast::channel(16).0,
    };
    let response = crate::api::router(state.clone())
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/messages/{id}/raw"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::GONE,
        "a copy that is gone is not an outage"
    );

    let _ = repo::delete_message(&db, id).await;
    let _ = repo::delete_mailbox(&db, &address).await;
    let _ = entity::domain::Entity::delete_many()
        .filter(entity::domain::Column::Name.eq(domain))
        .exec(&db)
        .await;
}

/// The raw bytes of the one message a mailbox owns, for asserting on headers.
async fn only_copy(
    db: &sea_orm::DatabaseConnection,
    store: &MailStore,
    mailbox_id: Uuid,
) -> String {
    let stored = message::Entity::find()
        .filter(message::Column::MailboxId.eq(mailbox_id))
        .all(db)
        .await
        .unwrap();
    assert_eq!(stored.len(), 1, "one stored copy");
    let raw = store
        .get(&stored[0].storage_key)
        .await
        .expect("read the copy");
    String::from_utf8_lossy(&raw).to_string()
}
/// The IMAP reader end to end: the folders it exposes, the reads a client makes,
/// the flags it persists and the delete path. Needs the compose stack:
///
///     make up && MAIL_TEST_INTEGRATION=1 cargo test -p server -- --nocapture
#[tokio::test]
async fn imap_reads_the_inbox_and_applies_flags() {
    if std::env::var("MAIL_TEST_INTEGRATION").is_err() {
        eprintln!("skipping: set MAIL_TEST_INTEGRATION=1 with the compose stack running");
        return;
    }

    let config = Arc::new(test_config());
    let db = Database::connect(&config.database_url)
        .await
        .expect("connect");
    Migrator::up(&db, None).await.expect("migrate");
    let store = MailStore::new(&config.s3).expect("object store");

    let domain = format!("imap-{}.example", Uuid::now_v7().simple());
    repo::ensure_domain(&db, &domain).await.unwrap();
    let address = format!("reader@{domain}");
    let mailbox = repo::create_mailbox(&db, &address, "secret").await.unwrap();

    let state = AppState {
        db: db.clone(),
        config: config.clone(),
        store: store.clone(),
        authenticator: None,
        tls: None,
        events: tokio::sync::broadcast::channel(16).0,
    };

    let raw = b"From: Alice <alice@remote.test>\r\nTo: reader\r\nSubject: imap fixture\r\nMessage-ID: <imap-fixture@remote.test>\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nhello from imap\r\n";
    let id = crate::delivery::store_local(
        &state,
        &mailbox,
        message::Direction::Inbound,
        "alice@remote.test",
        &address,
        raw,
        &mailcore::policy::AuthOutcome::skipped(),
    )
    .await
    .unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(crate::imap::serve(listener, state.clone()));

    let stream = TcpStream::connect(addr).await.unwrap();
    let (read_half, mut writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);
    let greeting = imap_line(&mut reader).await;
    assert!(greeting.starts_with("* OK"), "{greeting}");

    // A wrong password is refused before anything is listed.
    let reply = imap_command(
        &mut reader,
        &mut writer,
        "a1",
        &format!("LOGIN {address} wrong"),
    )
    .await;
    assert!(reply.contains("NO"), "{reply}");

    let reply = imap_command(
        &mut reader,
        &mut writer,
        "a2",
        &format!("LOGIN {address} secret"),
    )
    .await;
    assert!(reply.contains("a2 OK"), "{reply}");

    let reply = imap_command(&mut reader, &mut writer, "a3", "LIST \"\" \"*\"").await;
    assert!(reply.contains("\"INBOX\""), "{reply}");
    assert!(reply.contains("\"Sent\""), "{reply}");
    assert!(reply.contains("\\Trash"), "{reply}");

    let reply = imap_command(&mut reader, &mut writer, "a4", "SELECT INBOX").await;
    assert!(reply.contains("1 EXISTS"), "{reply}");
    assert!(reply.contains("UIDVALIDITY"), "{reply}");
    assert!(reply.contains("a4 OK"), "{reply}");

    let reply = imap_command(
        &mut reader,
        &mut writer,
        "a5",
        "FETCH 1 (UID FLAGS RFC822.SIZE ENVELOPE BODYSTRUCTURE)",
    )
    .await;
    assert!(reply.contains("ENVELOPE"), "{reply}");
    assert!(reply.contains("\"imap fixture\""), "{reply}");
    assert!(reply.contains("BODYSTRUCTURE"), "{reply}");

    // A literal section: the body itself comes back inside BODY[TEXT].
    let reply = imap_command(&mut reader, &mut writer, "a6", "FETCH 1 (BODY.PEEK[TEXT])").await;
    assert!(reply.contains("hello from imap"), "{reply}");
    assert!(reply.contains("a6 OK"), "{reply}");

    let reply = imap_command(&mut reader, &mut writer, "a7", "STORE 1 +FLAGS (\\Seen)").await;
    assert!(reply.contains("\\Seen"), "{reply}");

    let reply = imap_command(&mut reader, &mut writer, "a8", "UID SEARCH UNSEEN").await;
    assert!(reply.contains("* SEARCH"), "{reply}");
    assert!(
        !reply.contains("* SEARCH 1"),
        "the message is read now: {reply}"
    );

    // An APPEND of a message that is already here is answered with the copy that
    // exists rather than stored a second time, which is what a client saving its
    // own sent mail would otherwise produce.
    let reply = imap_append(
        &mut reader,
        &mut writer,
        "a8b",
        "INBOX",
        "From: Alice <alice@remote.test>\r\nTo: reader\r\nSubject: imap fixture\r\nMessage-ID: <imap-fixture@remote.test>\r\n\r\nhello from imap\r\n",
    )
    .await;
    assert!(reply.contains("Message already stored"), "{reply}");
    assert_eq!(
        message::Entity::find()
            .filter(message::Column::MailboxId.eq(mailbox.id))
            .all(&db)
            .await
            .unwrap()
            .len(),
        1,
        "a duplicate APPEND stores nothing"
    );

    let reply = imap_command(
        &mut reader,
        &mut writer,
        "a9",
        &format!(
            "UID STORE {} +FLAGS (\\Deleted)",
            repo::get_message(&db, id)
                .await
                .unwrap()
                .expect("the fixture")
                .uid
        ),
    )
    .await;
    assert!(reply.contains("a9 OK"), "{reply}");
    let reply = imap_command(&mut reader, &mut writer, "a10", "EXPUNGE").await;
    assert!(reply.contains("1 EXPUNGE"), "{reply}");

    let reply = imap_command(&mut reader, &mut writer, "a11", "LOGOUT").await;
    assert!(reply.contains("a11 OK"), "{reply}");

    let remaining = message::Entity::find()
        .filter(message::Column::Id.eq(id))
        .all(&db)
        .await
        .unwrap();
    assert!(remaining.is_empty(), "an expunged message leaves no row");

    let _ = repo::delete_mailbox(&db, &address).await;
    let _ = entity::domain::Entity::delete_many()
        .filter(entity::domain::Column::Name.eq(domain))
        .exec(&db)
        .await;
}

/// The POP3 drop end to end: the single reader lock, the listing, a download
/// that marks the message read, and the delete at QUIT.
#[tokio::test]
async fn pop3_downloads_and_deletes_the_drop() {
    if std::env::var("MAIL_TEST_INTEGRATION").is_err() {
        eprintln!("skipping: set MAIL_TEST_INTEGRATION=1 with the compose stack running");
        return;
    }

    let config = Arc::new(test_config());
    let db = Database::connect(&config.database_url)
        .await
        .expect("connect");
    Migrator::up(&db, None).await.expect("migrate");
    let store = MailStore::new(&config.s3).expect("object store");

    let domain = format!("pop3-{}.example", Uuid::now_v7().simple());
    repo::ensure_domain(&db, &domain).await.unwrap();
    let address = format!("dropper@{domain}");
    let mailbox = repo::create_mailbox(&db, &address, "secret").await.unwrap();

    let state = AppState {
        db: db.clone(),
        config: config.clone(),
        store: store.clone(),
        authenticator: None,
        tls: None,
        events: tokio::sync::broadcast::channel(16).0,
    };

    let raw = b"From: Alice <alice@remote.test>\r\nTo: dropper\r\nSubject: pop3 fixture\r\n\r\nhello from pop3\r\n";
    let id = crate::delivery::store_local(
        &state,
        &mailbox,
        message::Direction::Inbound,
        "alice@remote.test",
        &address,
        raw,
        &mailcore::policy::AuthOutcome::skipped(),
    )
    .await
    .unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(crate::pop3::serve(listener, state.clone()));

    let stream = TcpStream::connect(addr).await.unwrap();
    let (read_half, mut writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);
    assert!(pop3_line(&mut reader).await.starts_with("+OK"));

    let reply = pop3_command(&mut reader, &mut writer, &format!("USER {address}")).await;
    assert!(reply.starts_with("+OK"), "{reply}");
    let reply = pop3_command(&mut reader, &mut writer, "PASS secret").await;
    assert!(reply.starts_with("+OK"), "{reply}");

    // A second reader is refused while the drop is open.
    let second = TcpStream::connect(addr).await.unwrap();
    let (second_read, mut second_write) = tokio::io::split(second);
    let mut second_reader = BufReader::new(second_read);
    assert!(pop3_line(&mut second_reader).await.starts_with("+OK"));
    let _ = pop3_command(
        &mut second_reader,
        &mut second_write,
        &format!("USER {address}"),
    )
    .await;
    let reply = pop3_command(&mut second_reader, &mut second_write, "PASS secret").await;
    assert!(reply.contains("IN-USE"), "{reply}");
    drop(second_reader);
    drop(second_write);

    let reply = pop3_block(&mut reader, &mut writer, "CAPA").await;
    assert!(reply.contains("UIDL"), "{reply}");
    assert!(reply.contains("TOP"), "{reply}");

    let reply = pop3_command(&mut reader, &mut writer, "STAT").await;
    assert!(reply.starts_with("+OK 1 "), "{reply}");

    let reply = pop3_block(&mut reader, &mut writer, "LIST").await;
    assert!(reply.contains("1 "), "{reply}");

    let reply = pop3_block(&mut reader, &mut writer, "RETR 1").await;
    assert!(reply.contains("hello from pop3"), "{reply}");
    assert!(reply.trim_end().ends_with('.'), "{reply}");

    let reply = pop3_block(&mut reader, &mut writer, "TOP 1 0").await;
    assert!(reply.contains("Subject: pop3 fixture"), "{reply}");

    let stored = message::Entity::find()
        .filter(message::Column::Id.eq(id))
        .all(&db)
        .await
        .unwrap();
    assert_eq!(stored.len(), 1);
    let flag = entity::message_flag::Entity::find_by_id(id)
        .one(&db)
        .await
        .unwrap()
        .expect("a flag row");
    assert!(flag.read, "RETR marks the message read");

    let reply = pop3_command(&mut reader, &mut writer, "DELE 1").await;
    assert!(reply.starts_with("+OK"), "{reply}");
    let reply = pop3_command(&mut reader, &mut writer, "QUIT").await;
    assert!(reply.starts_with("+OK"), "{reply}");
    // The drop is cleared once the reply is out, so wait for the close before
    // looking at what is left behind.
    let mut rest = Vec::new();
    let _ = reader.read_to_end(&mut rest).await;

    let remaining = message::Entity::find()
        .filter(message::Column::Id.eq(id))
        .all(&db)
        .await
        .unwrap();
    assert!(remaining.is_empty(), "QUIT removes what was deleted");

    let _ = repo::delete_mailbox(&db, &address).await;
    let _ = entity::domain::Entity::delete_many()
        .filter(entity::domain::Column::Name.eq(domain))
        .exec(&db)
        .await;
}

/// A reader over an IMAP session that keeps literals intact.
type ImapReader = BufReader<tokio::io::ReadHalf<TcpStream>>;
type ImapWriter = tokio::io::WriteHalf<TcpStream>;

/// Sends one tagged command and returns every line it produced, literals
/// included, up to and including the tagged reply.
async fn imap_command(
    reader: &mut ImapReader,
    writer: &mut ImapWriter,
    tag: &str,
    line: &str,
) -> String {
    command(writer, &format!("{tag} {line}")).await;
    imap_block(reader, tag).await
}

/// Sends an APPEND with its literal and reads the reply.
async fn imap_append(
    reader: &mut ImapReader,
    writer: &mut ImapWriter,
    tag: &str,
    mailbox: &str,
    body: &str,
) -> String {
    let head = format!("{tag} APPEND {mailbox} {{{}}}\r\n", body.len());
    writer.write_all(head.as_bytes()).await.unwrap();
    writer.write_all(body.as_bytes()).await.unwrap();
    writer.write_all(&CRLF).await.unwrap();
    writer.flush().await.unwrap();
    imap_block(reader, tag).await
}

async fn imap_block(reader: &mut ImapReader, tag: &str) -> String {
    let mut out = String::new();
    loop {
        let text = imap_line(reader).await;
        out.push_str(&text);
        if let Some(size) = literal_size(&text) {
            let mut bytes = vec![0u8; size];
            reader.read_exact(&mut bytes).await.unwrap();
            out.push_str(&String::from_utf8_lossy(&bytes));
        }
        if text.starts_with(&format!("{tag} ")) {
            return out;
        }
    }
}

/// Reads one IMAP line, terminator included, as text.
async fn imap_line(reader: &mut ImapReader) -> String {
    let mut line = Vec::new();
    let read = reader.read_until(b'\n', &mut line).await.unwrap();
    assert!(read > 0, "connection closed before an IMAP reply");
    String::from_utf8_lossy(&line).to_string()
}

/// The size a line announces with a trailing literal marker.
fn literal_size(line: &str) -> Option<usize> {
    let trimmed = line.trim_end_matches(['\r', '\n']);
    let open = trimmed.rfind('{')?;
    let close = trimmed.rfind('}')?;
    if close <= open {
        return None;
    }
    trimmed[open + 1..close].parse().ok()
}

/// A reader over a POP3 session.
type Pop3Reader = BufReader<tokio::io::ReadHalf<TcpStream>>;
type Pop3Writer = tokio::io::WriteHalf<TcpStream>;

async fn pop3_line(reader: &mut Pop3Reader) -> String {
    let mut line = String::new();
    let read = reader.read_line(&mut line).await.unwrap();
    assert!(read > 0, "connection closed before a POP3 reply");
    line
}

async fn pop3_command(reader: &mut Pop3Reader, writer: &mut Pop3Writer, line: &str) -> String {
    command(writer, line).await;
    pop3_line(reader).await
}

/// Sends a command whose reply is a multi-line block and reads it whole.
async fn pop3_block(reader: &mut Pop3Reader, writer: &mut Pop3Writer, line: &str) -> String {
    let mut out = pop3_command(reader, writer, line).await;
    if !out.starts_with("+OK") {
        return out;
    }
    loop {
        let line = pop3_line(reader).await;
        out.push_str(&line);
        if line.trim_end_matches(['\r', '\n']) == "." {
            return out;
        }
    }
}
