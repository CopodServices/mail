/// Reads a required environment variable, exiting with a clear message when
/// it is missing (matching the panel's configuration style).
fn required(key: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| {
        eprintln!("❌ Missing environment variable: {key}");
        std::process::exit(1);
    })
}

fn optional(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|value| !value.is_empty())
}

fn flag(key: &str, default: bool) -> bool {
    std::env::var(key)
        .map(|value| !matches!(value.as_str(), "" | "0" | "false" | "no"))
        .unwrap_or(default)
}

fn number<T: std::str::FromStr>(key: &str, default: T) -> T {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn csv(key: &str) -> Vec<String> {
    optional(key)
        .map(|value| {
            value
                .split(',')
                .map(|item| item.trim().to_string())
                .filter(|item| !item.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Debug, Clone)]
pub struct S3Config {
    pub endpoint: Option<String>,
    pub region: String,
    pub bucket: String,
    pub access_key: String,
    pub secret_key: String,
    pub path_style: bool,
    pub allow_http: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayEncryption {
    None,
    StartTls,
    Implicit,
}

#[derive(Debug, Clone)]
pub struct RelayConfig {
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
    pub encryption: RelayEncryption,
    /// Envelope sender to rewrite to when the smarthost requires one
    /// (some relays reject arbitrary MAIL FROM values).
    pub from_override: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TlsConfig {
    pub cert_path: String,
    pub key_path: String,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub database_url: String,
    /// HS256 signing key for webmail session cookies (base64, 32 bytes).
    pub session_secret: String,
    /// Marks the session cookie Secure; disable only for plain-HTTP deployments.
    pub session_cookie_secure: bool,
    pub api_key: Option<String>,
    pub server_port: u16,
    pub smtp_listen: String,
    pub submission_listen: String,
    /// IMAP listener; None keeps the service without one.
    pub imap_listen: Option<String>,
    /// POP3 listener; None keeps the service without one.
    pub pop3_listen: Option<String>,
    pub hostname: String,
    pub bootstrap_domains: Vec<String>,
    pub bootstrap_password: Option<String>,
    pub max_message_size: usize,
    pub max_recipients: usize,
    pub verify_auth: bool,
    pub queue_interval_secs: u64,
    pub max_attempts: i32,
    pub s3: S3Config,
    pub relay: RelayConfig,
    pub tls: Option<TlsConfig>,
}

impl Config {
    pub fn load() -> Self {
        let tls = match (optional("TLS_CERT_PATH"), optional("TLS_KEY_PATH")) {
            (Some(cert_path), Some(key_path)) => Some(TlsConfig {
                cert_path,
                key_path,
            }),
            _ => None,
        };

        let encryption = match optional("RELAY_TLS")
            .unwrap_or_else(|| "starttls".to_string())
            .to_ascii_lowercase()
            .as_str()
        {
            "none" | "off" | "false" => RelayEncryption::None,
            "implicit" | "tls" | "ssl" => RelayEncryption::Implicit,
            _ => RelayEncryption::StartTls,
        };

        Config {
            database_url: required("DATABASE_URL"),
            session_secret: required("SESSION_SECRET"),
            session_cookie_secure: flag("SESSION_COOKIE_SECURE", true),
            api_key: optional("API_KEY"),
            server_port: number("SERVER_PORT", 8080u16),
            smtp_listen: optional("SMTP_LISTEN").unwrap_or_else(|| "0.0.0.0:2525".to_string()),
            submission_listen: optional("SUBMISSION_LISTEN")
                .unwrap_or_else(|| "0.0.0.0:2587".to_string()),
            imap_listen: optional("IMAP_LISTEN"),
            pop3_listen: optional("POP3_LISTEN"),
            hostname: optional("MAIL_HOSTNAME").unwrap_or_else(|| "localhost".to_string()),
            bootstrap_domains: csv("MAIL_DOMAINS"),
            bootstrap_password: optional("MAIL_BOOTSTRAP_PASSWORD"),
            max_message_size: number("MAX_MESSAGE_SIZE", 25 * 1024 * 1024usize),
            max_recipients: number("MAX_RECIPIENTS", 50usize),
            verify_auth: flag("MAIL_VERIFY_AUTH", true),
            queue_interval_secs: number("QUEUE_INTERVAL_SECS", 5u64),
            max_attempts: number("RELAY_MAX_ATTEMPTS", 8i32),
            s3: S3Config {
                endpoint: optional("S3_ENDPOINT"),
                region: optional("S3_REGION").unwrap_or_else(|| "us-east-1".to_string()),
                bucket: optional("S3_BUCKET").unwrap_or_else(|| "copod-mail".to_string()),
                access_key: required("S3_ACCESS_KEY"),
                secret_key: required("S3_SECRET_KEY"),
                path_style: flag("S3_PATH_STYLE", true),
                allow_http: flag("S3_ALLOW_HTTP", true),
            },
            relay: RelayConfig {
                host: optional("RELAY_HOST").unwrap_or_else(|| "127.0.0.1".to_string()),
                port: number("RELAY_PORT", 1025u16),
                username: optional("RELAY_USERNAME"),
                password: optional("RELAY_PASSWORD"),
                encryption,
                from_override: optional("RELAY_FROM"),
            },
            tls,
        }
    }

    pub fn starttls_enabled(&self) -> bool {
        self.tls.is_some()
    }

    /// True when the IMAP listener is configured to run.
    pub fn imap_enabled(&self) -> bool {
        self.imap_listen.is_some()
    }

    /// True when the POP3 listener is configured to run.
    pub fn pop3_enabled(&self) -> bool {
        self.pop3_listen.is_some()
    }
}

/// The port out of a listen address like `0.0.0.0:993`, or the fallback when
/// the address does not carry a parseable one.
pub fn listen_port(listen: &str, fallback: u16) -> u16 {
    listen
        .rsplit(':')
        .next()
        .and_then(|port| port.parse().ok())
        .unwrap_or(fallback)
}
