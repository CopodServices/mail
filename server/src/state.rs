use mail_auth::MessageAuthenticator;
use mailcore::config::Config;
use mailcore::secrets::SecretBox;
use mailcore::store::MailStore;
use sea_orm::DatabaseConnection;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub db: DatabaseConnection,
    pub config: Arc<Config>,
    pub store: MailStore,
    pub secrets: Arc<SecretBox>,
    /// None when DNS verification is disabled or the resolver could not start.
    pub authenticator: Option<Arc<MessageAuthenticator>>,
    pub tls: Option<Arc<rustls::ServerConfig>>,
}
