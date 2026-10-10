mod api;
mod bootstrap;
mod delivery;
mod smtp;
mod state;

#[cfg(test)]
mod integration_tests;

use anyhow::Context;
use mail_auth::MessageAuthenticator;
use mailcore::config::Config;
use mailcore::queue::Queue;
use mailcore::relay::Relay;
use mailcore::secrets::SecretBox;
use mailcore::store::MailStore;
use state::AppState;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tracing::{info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,sqlx=warn".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let config = Arc::new(Config::load());
    info!(hostname = %config.hostname, "🚀 Starting the mail service");

    let db = sea_orm::Database::connect(&config.database_url)
        .await
        .context("failed to connect to PostgreSQL")?;
    info!("✅ Connected to database");

    let store = MailStore::new(&config.s3).context("failed to build the mail store")?;
    let secrets = Arc::new(SecretBox::new(&config.encryption_key)?);

    let tls = match &config.tls {
        Some(tls) => Some(mailcore::tls::server_config(&tls.cert_path, &tls.key_path)?),
        None => None,
    };

    let authenticator = if config.verify_auth {
        match MessageAuthenticator::new_system_conf() {
            Ok(authenticator) => Some(Arc::new(authenticator)),
            Err(err) => {
                warn!(error = %err, "DNS resolver unavailable; inbound verification disabled");
                None
            }
        }
    } else {
        None
    };

    let state = AppState {
        db,
        config: config.clone(),
        store: store.clone(),
        secrets: secrets.clone(),
        authenticator,
        tls,
    };

    bootstrap::seed(&state).await?;

    let queue = Arc::new(Queue {
        db: state.db.clone(),
        store: store.clone(),
        relay: Relay::new(config.relay.clone()),
        secrets: (*secrets).clone(),
        max_attempts: config.max_attempts,
        helo: config.hostname.clone(),
    });
    queue.spawn(Duration::from_secs(config.queue_interval_secs));

    let inbound = TcpListener::bind(&config.smtp_listen)
        .await
        .with_context(|| {
            format!(
                "failed to bind the inbound listener at {}",
                config.smtp_listen
            )
        })?;
    let submission = TcpListener::bind(&config.submission_listen)
        .await
        .with_context(|| {
            format!(
                "failed to bind the submission listener at {}",
                config.submission_listen
            )
        })?;
    tokio::spawn(smtp::serve(inbound, state.clone(), smtp::Mode::Inbound));
    tokio::spawn(smtp::serve(
        submission,
        state.clone(),
        smtp::Mode::Submission,
    ));

    let app = api::router(state);
    let address = format!("0.0.0.0:{}", config.server_port);
    let listener = TcpListener::bind(&address)
        .await
        .with_context(|| format!("failed to bind the HTTP API at {address}"))?;
    info!("🌐 HTTP API listening on {address}");

    axum::serve(listener, app).await?;
    Ok(())
}
