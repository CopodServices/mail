mod api;
mod bootstrap;
mod delivery;
mod imap;
mod pop3;
mod session;
mod smtp;
mod spam_store;
mod state;

#[cfg(test)]
mod integration_tests;

use anyhow::Context;
use mailcore::config::Config;
use mailcore::queue::Queue;
use mailcore::relay::Relay;
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

    let tls = match &config.tls {
        Some(tls) => Some(mailcore::tls::server_config(&tls.cert_path, &tls.key_path)?),
        None => None,
    };

    let spam_config = Arc::new(mailcore::spam::SpamConfig::load());

    // The authentication resolver and the DNSBL lookups share one resolver;
    // SPAM_DNS_RESOLVER points both at a resolver that permits blocklists.
    let authenticator = if config.verify_auth || !spam_config.mode.is_off() {
        match mailcore::spam::build_authenticator(spam_config.dns_resolver.as_deref()) {
            Some(authenticator) => Some(Arc::new(authenticator)),
            None => {
                warn!("DNS resolver unavailable; inbound verification disabled");
                None
            }
        }
    } else {
        None
    };

    let spam = if spam_config.mode.is_off() {
        None
    } else {
        let resolver = authenticator
            .as_ref()
            .map(|authenticator| Arc::new(authenticator.resolver().clone()));
        info!(mode = ?spam_config.mode, "spam filtering enabled");
        Some(Arc::new(
            mailcore::spam::SpamEngine::new(spam_config.clone(), resolver).await,
        ))
    };

    let (events, _) = tokio::sync::broadcast::channel(256);
    let state = AppState {
        db,
        config: config.clone(),
        store: store.clone(),
        authenticator,
        spam,
        tls,
        events,
    };

    bootstrap::seed(&state).await?;

    if state.spam.is_some() {
        spam_store::load(&state).await;
        spam_store::spawn(state.clone());
    }

    let queue = Arc::new(Queue {
        db: state.db.clone(),
        store: store.clone(),
        relay: Relay::new(config.relay.clone()),
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

    // The readers are opt-in, so an instance that does not set them behaves
    // exactly as it did before they existed.
    if let Some(listen) = config.imap_listen.clone() {
        let listener = TcpListener::bind(&listen)
            .await
            .with_context(|| format!("failed to bind the IMAP listener at {listen}"))?;
        if config.tls.is_none() {
            warn!(listen = %listen, "IMAP is serving plaintext; configure TLS to encrypt it");
        }
        tokio::spawn(imap::serve(listener, state.clone()));
    }
    if let Some(listen) = config.pop3_listen.clone() {
        let listener = TcpListener::bind(&listen)
            .await
            .with_context(|| format!("failed to bind the POP3 listener at {listen}"))?;
        if config.tls.is_none() {
            warn!(listen = %listen, "POP3 is serving plaintext; configure TLS to encrypt it");
        }
        tokio::spawn(pop3::serve(listener, state.clone()));
    }

    let app = api::router(state);
    let address = format!("0.0.0.0:{}", config.server_port);
    let listener = TcpListener::bind(&address)
        .await
        .with_context(|| format!("failed to bind the HTTP API at {address}"))?;
    info!("🌐 HTTP API listening on {address}");

    axum::serve(listener, app).await?;
    Ok(())
}
