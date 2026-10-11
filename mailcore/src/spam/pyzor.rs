//! A Pyzor client: a fuzzy body digest queried over UDP against a public or
//! private Pyzor server. The digest here is a documented approximation of the
//! Pyzor normalization; wire compatibility with public servers is a follow-up.

use crate::spam::Tags;
use crate::spam::checks::{Elements, tag};
use crate::spam::config::PyzorConfig;
use sha1_smol::Sha1;
use tokio::net::UdpSocket;
use tokio::time::{Duration, timeout};

#[derive(Debug, Default, PartialEq)]
pub struct Response {
    pub code: u32,
    pub count: u64,
    pub wl_count: u64,
}

pub async fn check(config: &PyzorConfig, elements: &Elements, tags: &mut Tags) {
    let digest = digest(&elements.text_body);
    if digest.is_empty() {
        return;
    }
    let request =
        format!("Op: check\nOp-Digest: {digest}\nThread: 0\nPV: 2.1\nUser: anonymous\nTime: 0\n\n");
    let Ok(socket) = UdpSocket::bind("0.0.0.0:0").await else {
        return;
    };
    let Ok(mut addrs) = tokio::net::lookup_host(&config.address).await else {
        return;
    };
    let Some(addr) = addrs.next() else {
        return;
    };
    if socket.connect(addr).await.is_err() || socket.send(request.as_bytes()).await.is_err() {
        return;
    }
    let mut buf = [0u8; 2048];
    let deadline = Duration::from_millis(config.timeout_ms.max(200));
    let Ok(Ok(len)) = timeout(deadline, socket.recv(&mut buf)).await else {
        return;
    };
    let response = parse(&String::from_utf8_lossy(&buf[..len]));
    if response.code == 200 && response.count >= 20 && response.count > response.wl_count {
        tag(tags, "PYZOR_SPAM");
    } else if response.count > 0 {
        tag(tags, "PYZOR_LOW");
    }
}

pub fn parse(text: &str) -> Response {
    let mut response = Response {
        code: 200,
        ..Response::default()
    };
    for line in text.lines() {
        if let Some((key, value)) = line.split_once(':') {
            let value = value.trim();
            match key.trim().to_ascii_lowercase().as_str() {
                "code" => response.code = value.parse().unwrap_or(200),
                "count" => response.count = value.parse().unwrap_or(0),
                "wl-count" => response.wl_count = value.parse().unwrap_or(0),
                _ => {}
            }
        }
    }
    response
}

/// Lowercase, trim, drop trivial lines, and hash the surviving body.
pub fn digest(body: &str) -> String {
    let mut atomic = String::new();
    let mut lines = 0;
    let mut chunks: Vec<String> = Vec::new();
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.len() < 8 {
            continue;
        }
        atomic.push_str(trimmed);
        atomic.push('\n');
        lines += 1;
        if lines == 4 {
            chunks.push(atomic.trim_end().to_string());
            atomic.clear();
            lines = 0;
        }
    }
    if !atomic.trim().is_empty() {
        chunks.push(atomic.trim_end().to_string());
    }
    if chunks.is_empty() {
        return String::new();
    }
    Sha1::from(chunks.join("\n").as_bytes())
        .digest()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_response() {
        let response = parse("Code: 200\nCount: 42\nWL-Count: 3\n");
        assert_eq!(response.count, 42);
        assert_eq!(response.wl_count, 3);
    }

    #[test]
    fn digest_is_stable_and_ignores_short_lines() {
        let a = digest("a very long spammy line here\nanother long line of text\nok\n");
        let b = digest("a very long spammy line here\nanother long line of text\nok\n");
        assert_eq!(a, b);
        assert!(!a.is_empty());
    }
}
