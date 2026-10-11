//! Attachment and content scanners: ClamAV over its INSTREAM protocol, an
//! olefy (oletools) HTTP service for Office macros, and Tika for text
//! extraction. Every call is best-effort and fails open.

use crate::spam::Tags;
use crate::spam::checks::tag;
use crate::spam::config::ScannersConfig;
use base64::Engine;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::{Duration, timeout};

#[derive(Debug, Default)]
pub struct ScanOutcome {
    pub malware: bool,
}

pub async fn scan(
    http: Option<&reqwest::Client>,
    config: &ScannersConfig,
    raw: &[u8],
    tags: &mut Tags,
) -> ScanOutcome {
    let deadline = Duration::from_millis(config.timeout_ms.max(500));
    let mut outcome = ScanOutcome::default();

    if let Some(addr) = &config.clamav {
        match timeout(deadline, clamav(addr, raw)).await {
            Ok(Ok(Some(signature))) => {
                outcome.malware = true;
                tag(tags, format!("MALWARE_{}", sanitize(&signature)));
            }
            Ok(Ok(None)) => {}
            Ok(Err(err)) => tracing::debug!(error = %err, "clamav scan failed"),
            Err(_) => tracing::debug!("clamav scan timed out"),
        }
    }

    if let (Some(client), Some(url)) = (http, config.olefy.as_deref()) {
        match timeout(deadline, olefy(client, url, raw)).await {
            Ok(Ok(true)) => {
                tag(tags, "MACRO_SUSPICIOUS");
            }
            Ok(Ok(false)) => {}
            Ok(Err(err)) => tracing::debug!(error = %err, "olefy scan failed"),
            Err(_) => tracing::debug!("olefy scan timed out"),
        }
    }

    outcome
}

/// Tika text extraction, used to feed attachment content into the classifier.
pub async fn extract_text(client: &reqwest::Client, url: &str, raw: &[u8]) -> Option<String> {
    let response = client
        .put(url)
        .header("Accept", "text/plain")
        .body(raw.to_vec())
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    response.text().await.ok()
}

async fn clamav(addr: &str, raw: &[u8]) -> anyhow::Result<Option<String>> {
    let mut stream = TcpStream::connect(addr).await?;
    stream.write_all(b"zINSTREAM\0").await?;
    for chunk in raw.chunks(4096) {
        stream
            .write_all(&(chunk.len() as u32).to_be_bytes())
            .await?;
        stream.write_all(chunk).await?;
    }
    stream.write_all(&0u32.to_be_bytes()).await?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await?;
    let text = String::from_utf8_lossy(&buf).to_string();
    Ok(parse_clamd_reply(&text))
}

/// The signature in a clamd reply, or None when the stream is clean.
fn parse_clamd_reply(text: &str) -> Option<String> {
    if !text.contains("FOUND") {
        return None;
    }
    Some(
        text.split_once("stream:")
            .map(|(_, rest)| {
                rest.split("FOUND")
                    .next()
                    .unwrap_or("virus")
                    .trim()
                    .to_string()
            })
            .unwrap_or_else(|| "virus".to_string()),
    )
}

async fn olefy(client: &reqwest::Client, url: &str, raw: &[u8]) -> anyhow::Result<bool> {
    let body = serde_json::json!({
        "data": base64::engine::general_purpose::STANDARD.encode(raw)
    });
    let response = client.post(url).json(&body).send().await?;
    let text = response.text().await?;
    Ok(text.contains("macro") || text.contains("OLE") || text.contains("VBA"))
}

fn sanitize(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(40)
        .collect::<String>()
        .to_ascii_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_clamd_found_reply() {
        assert_eq!(
            parse_clamd_reply("stream: Eicar-Test-Signature FOUND\0").as_deref(),
            Some("Eicar-Test-Signature")
        );
        assert_eq!(parse_clamd_reply("stream: OK\0"), None);
        assert_eq!(
            sanitize(&parse_clamd_reply("stream: Eicar-Test-Signature FOUND").unwrap()),
            "EICARTESTSIGNATURE"
        );
    }

    #[tokio::test]
    async fn an_unreachable_scanner_fails_open() {
        let config = ScannersConfig {
            clamav: Some("127.0.0.1:1".to_string()),
            timeout_ms: 500,
            ..ScannersConfig::default()
        };
        let mut tags = Tags::new();
        let outcome = scan(None, &config, b"hello", &mut tags).await;
        assert!(!outcome.malware);
        assert!(tags.is_empty(), "tags: {tags:?}");
    }
}
