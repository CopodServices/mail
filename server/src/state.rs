use mail_auth::MessageAuthenticator;
use mailcore::config::Config;
use mailcore::spam::SpamEngine;
use mailcore::store::MailStore;
use sea_orm::DatabaseConnection;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub db: DatabaseConnection,
    pub config: Arc<Config>,
    pub store: MailStore,
    /// None when DNS verification is disabled or the resolver could not start.
    pub authenticator: Option<Arc<MessageAuthenticator>>,
    /// None when spam filtering is off.
    pub spam: Option<Arc<SpamEngine>>,
    pub tls: Option<Arc<rustls::ServerConfig>>,
    /// Live updates for connected clients; each message is a JSON frame.
    pub events: tokio::sync::broadcast::Sender<String>,
}

impl AppState {
    /// Nudges every connected client that mail changed.
    pub fn publish(&self, frame: String) {
        // No subscribers is the normal case outside the web client.
        let _ = self.events.send(frame);
    }
}
