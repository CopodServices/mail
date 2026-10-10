use crate::smtp::{self, Mode};
use crate::state::AppState;
use entity::message;
use entity::outbound_job;
use mailcore::config::{Config, RelayConfig, RelayEncryption, S3Config, TlsConfig};
use mailcore::queue::Queue;
use mailcore::relay::Relay;
use mailcore::repo;
use mailcore::store::MailStore;
use migration::{Migrator, MigratorTrait};
use sea_orm::{ColumnTrait, Database, EntityTrait, QueryFilter};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
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
    repo::create_mailbox(&db, &address, "secret").await.unwrap();

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

    command(&mut write_half, "DATA").await;
    assert_eq!(read_reply(&mut reader).await, 354);
    write_half
        .write_all(&wire(&[
            "From: Sender <sender@remote.test>",
            "To: friend@remote.test",
            "Subject: Relayed",
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

    let jobs = outbound_job::Entity::find().all(&db).await.unwrap();
    let job = jobs
        .into_iter()
        .find(|job| job.envelope_from == address)
        .expect("a queued job for this sender");
    assert_eq!(job.status, outbound_job::JobStatus::Pending);
    assert_eq!(
        job.recipients,
        serde_json::json!(["friend@remote.test"]),
        "recipients should be captured from RCPT TO"
    );

    // Process the queue once; Mailpit accepts, so the job must become sent.
    let queue = Queue {
        db: db.clone(),
        store: store.clone(),
        relay: Relay::new(config.relay.clone()),
        max_attempts: 3,
        helo: config.hostname.clone(),
    };
    assert_eq!(queue.run_once().await.unwrap(), 1);

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

    // Clean up the fixtures.
    let _ = store.delete(&job.storage_key).await;
    let _ = outbound_job::Entity::delete_by_id(job.id).exec(&db).await;
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
