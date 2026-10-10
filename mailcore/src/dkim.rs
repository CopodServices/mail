use anyhow::{Context, Result};
use mail_auth::common::crypto::{RsaKey, Sha256};
use mail_auth::dkim::DkimSigner;
use mail_auth::dkim::generate::DkimKeyPair;
use rustls_pki_types::{PrivateKeyDer, PrivatePkcs1KeyDer};

/// Headers covered by the outbound signature.
const SIGNED_HEADERS: [&str; 8] = [
    "From",
    "To",
    "Cc",
    "Subject",
    "Date",
    "Message-ID",
    "MIME-Version",
    "Content-Type",
];

pub struct DkimKey {
    /// PKCS#1 DER private key, stored encrypted in the database.
    pub private_key: Vec<u8>,
    /// Base64 of the PKCS#1 DER public key, the DNS p= tag value.
    pub public_key_b64: String,
}

/// Generates a 2048-bit RSA key pair for a domain.
pub fn generate_key() -> Result<DkimKey> {
    let pair = DkimKeyPair::generate_rsa(2048).context("failed to generate a DKIM key pair")?;
    Ok(DkimKey {
        private_key: pair.private_key().to_vec(),
        public_key_b64: pair.encoded_public_key(),
    })
}

/// Prepends a DKIM-Signature header to the raw message, leaving the body
/// byte-for-byte intact.
pub fn sign(private_key_der: &[u8], domain: &str, selector: &str, raw: &[u8]) -> Result<Vec<u8>> {
    let der = PrivatePkcs1KeyDer::from(private_key_der.to_vec());
    let key = RsaKey::<Sha256>::from_key_der(PrivateKeyDer::Pkcs1(der))
        .context("failed to load the DKIM private key")?;

    let signer = DkimSigner::from_key(key)
        .domain(domain)
        .selector(selector)
        .headers(SIGNED_HEADERS);

    let signature = signer.sign(raw).context("failed to sign the message")?;

    let mut header = Vec::new();
    signature.write(&mut header, true);
    if !header.ends_with(b"\r\n") {
        header.extend_from_slice(b"\r\n");
    }

    let mut signed = header;
    signed.extend_from_slice(raw);
    Ok(signed)
}

pub fn txt_record_name(selector: &str, domain: &str) -> String {
    format!("{selector}._domainkey.{domain}")
}

pub fn txt_record_value(public_key_b64: &str) -> String {
    format!("v=DKIM1; k=rsa; p={public_key_b64}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_key_signs_a_message() {
        let key = generate_key().expect("key generation");
        assert!(!key.public_key_b64.is_empty());

        let raw = b"From: Alice <alice@example.test>\r\nTo: bob@example.test\r\nSubject: hi\r\n\r\nhello\r\n";
        let signed = sign(&key.private_key, "example.test", "default", raw).expect("sign");

        let text = String::from_utf8_lossy(&signed);
        assert!(text.starts_with("DKIM-Signature:"));
        assert!(text.contains("d=example.test"));
        assert!(text.contains("s=default"));
        assert!(text.contains("a=rsa-sha256"));
        assert!(signed.ends_with(raw));
    }

    #[test]
    fn dns_record_shape() {
        assert_eq!(
            txt_record_name("default", "example.test"),
            "default._domainkey.example.test"
        );
        assert_eq!(txt_record_value("AAAA"), "v=DKIM1; k=rsa; p=AAAA");
    }
}
