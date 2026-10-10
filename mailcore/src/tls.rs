use anyhow::{Context, Result, anyhow};
use rustls::pki_types::CertificateDer;
use std::fs::File;
use std::io::BufReader;
use std::sync::Arc;

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Loads a PEM certificate chain and private key into a rustls server config
/// used for inbound STARTTLS.
pub fn server_config(cert_path: &str, key_path: &str) -> Result<Arc<rustls::ServerConfig>> {
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut BufReader::new(
        File::open(cert_path).context("open TLS cert")?,
    ))
    .collect::<Result<_, _>>()
    .context("parse TLS certificate chain")?;
    if certs.is_empty() {
        return Err(anyhow!("TLS certificate file contains no certificates"));
    }

    let key = rustls_pemfile::private_key(&mut BufReader::new(
        File::open(key_path).context("open TLS key")?,
    ))
    .context("parse TLS private key")?
    .ok_or_else(|| anyhow!("TLS key file contains no private key"))?;

    let config = rustls::ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .context("build TLS server config")?;

    Ok(Arc::new(config))
}

/// Client config used when talking to the smarthost over STARTTLS or implicit TLS.
pub fn client_config() -> Arc<rustls::ClientConfig> {
    let roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let config = rustls::ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .expect("default protocol versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
    Arc::new(config)
}
