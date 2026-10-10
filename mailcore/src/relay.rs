use crate::config::{RelayConfig, RelayEncryption};
use crate::stream::{Conn, OutboundStream, dot_stuff};
use crate::tls;
use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use rustls::pki_types::ServerName;
use smtp_proto::Response;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

/// A refusal from the smarthost, carrying whether it is worth retrying.
///
/// A 4xx reply is temporary (greylisting, rate limits) and the queue retries
/// with backoff; a 5xx reply is permanent (bad SPF/DKIM/MX, an unapproved
/// sending domain, bad credentials) and the queue fails the job at once.
#[derive(Debug)]
pub struct SmtpRejection {
    pub permanent: bool,
    pub message: String,
}

impl std::fmt::Display for SmtpRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for SmtpRejection {}

/// Wraps an SMTP reply as a rejection, classifying 5xx as permanent.
fn rejection(response: &Response<String>) -> anyhow::Error {
    anyhow::Error::new(SmtpRejection {
        permanent: response.code >= 500 && response.code < 600,
        message: format!("relay replied {} {}", response.code, response.message),
    })
}

/// Minimal SMTP client used to hand messages to the smarthost. It relays the
/// already-signed bytes unchanged, so no message rebuilding happens here.
pub struct Relay {
    config: RelayConfig,
    tls: Arc<rustls::ClientConfig>,
}

impl Relay {
    pub fn new(config: RelayConfig) -> Self {
        Self {
            config,
            tls: tls::client_config(),
        }
    }

    pub async fn deliver(
        &self,
        helo: &str,
        from: &str,
        recipients: &[String],
        raw: &[u8],
    ) -> Result<()> {
        let socket = TcpStream::connect((self.config.host.as_str(), self.config.port))
            .await
            .with_context(|| {
                format!(
                    "failed to connect to the relay at {}:{}",
                    self.config.host, self.config.port
                )
            })?;
        let _ = socket.set_nodelay(true);

        let mut conn = match self.config.encryption {
            RelayEncryption::Implicit => Conn::new(OutboundStream::Tls(Box::new(
                self.tls_connect(socket).await?,
            ))),
            _ => Conn::new(OutboundStream::Plain(socket)),
        };

        expect(&read_response(&mut conn).await?, 220, "greeting")?;

        let ehlo = run(&mut conn, &format!("EHLO {helo}\r\n")).await?;
        if ehlo.code != 250 {
            expect(
                &run(&mut conn, &format!("HELO {helo}\r\n")).await?,
                250,
                "HELO",
            )?;
        }

        if self.config.encryption == RelayEncryption::StartTls {
            expect(&run(&mut conn, "STARTTLS\r\n").await?, 220, "STARTTLS")?;
            let socket = match conn.into_inner() {
                OutboundStream::Plain(socket) => socket,
                OutboundStream::Tls(_) => bail!("relay connection is already encrypted"),
            };
            conn = Conn::new(OutboundStream::Tls(Box::new(
                self.tls_connect(socket).await?,
            )));
            expect(
                &run(&mut conn, &format!("EHLO {helo}\r\n")).await?,
                250,
                "EHLO after STARTTLS",
            )?;
        }

        if let (Some(username), Some(password)) = (&self.config.username, &self.config.password) {
            let credentials = STANDARD.encode(format!("\0{username}\0{password}"));
            expect(
                &run(&mut conn, &format!("AUTH PLAIN {credentials}\r\n")).await?,
                235,
                "AUTH",
            )?;
        }

        let envelope_from = self
            .config
            .from_override
            .clone()
            .unwrap_or_else(|| from.to_string());
        expect(
            &run(&mut conn, &format!("MAIL FROM:<{envelope_from}>\r\n")).await?,
            250,
            "MAIL FROM",
        )?;

        for recipient in recipients {
            let response = run(&mut conn, &format!("RCPT TO:<{recipient}>\r\n")).await?;
            if response.code != 250 && response.code != 251 {
                return Err(anyhow::Error::new(SmtpRejection {
                    permanent: response.code >= 500 && response.code < 600,
                    message: format!(
                        "relay rejected recipient {recipient}: {} {}",
                        response.code, response.message
                    ),
                }));
            }
        }

        let data = run(&mut conn, "DATA\r\n").await?;
        if data.code != 354 {
            return Err(rejection(&data));
        }

        let mut payload = dot_stuff(raw);
        if !payload.ends_with(b"\r\n") {
            payload.extend_from_slice(b"\r\n");
        }
        payload.extend_from_slice(b".\r\n");
        conn.write_all(&payload).await?;
        conn.flush().await?;

        let accepted = read_response(&mut conn).await?;
        if accepted.code != 250 {
            return Err(rejection(&accepted));
        }

        let _ = run(&mut conn, "QUIT\r\n").await;
        Ok(())
    }

    async fn tls_connect(
        &self,
        socket: TcpStream,
    ) -> Result<tokio_rustls::client::TlsStream<TcpStream>> {
        let server_name = ServerName::try_from(self.config.host.clone())
            .map_err(|_| anyhow!("relay host is not a valid TLS name"))?;
        TlsConnector::from(self.tls.clone())
            .connect(server_name, socket)
            .await
            .context("TLS handshake with the relay failed")
    }
}

async fn run(conn: &mut Conn<OutboundStream>, command: &str) -> Result<Response<String>> {
    conn.write_all(command.as_bytes()).await?;
    conn.flush().await?;
    read_response(conn).await
}

fn expect(response: &Response<String>, code: u16, step: &str) -> Result<()> {
    if response.code == code {
        Ok(())
    } else if response.code >= 500 && response.code < 600 {
        Err(anyhow::Error::new(SmtpRejection {
            permanent: true,
            message: format!(
                "relay refused {step}: {} {}",
                response.code, response.message
            ),
        }))
    } else {
        Err(anyhow::Error::new(SmtpRejection {
            permanent: false,
            message: format!(
                "relay deferred {step}: {} {}",
                response.code, response.message
            ),
        }))
    }
}

/// Reads a (possibly multi-line) SMTP reply and parses it with smtp-proto.
async fn read_response(conn: &mut Conn<OutboundStream>) -> Result<Response<String>> {
    let mut raw = Vec::new();
    loop {
        let line = conn
            .read_line(4096)
            .await?
            .ok_or_else(|| anyhow!("the relay closed the connection"))?;
        let is_final = line.len() >= 4 && line[3] == b' ';
        let is_continuation = line.len() >= 4 && line[3] == b'-';
        raw.extend_from_slice(&line);
        raw.extend_from_slice(b"\r\n");
        if is_final || !is_continuation {
            break;
        }
    }

    build_response(&raw)
}

/// Turns the accumulated reply lines into an smtp-proto Response. Continuation
/// lines (a hyphen after the code) are folded into the message.
fn build_response(raw: &[u8]) -> Result<Response<String>> {
    let text = String::from_utf8_lossy(raw);
    let mut code: Option<u16> = None;
    let mut parts: Vec<&str> = Vec::new();
    for line in text.split("\r\n") {
        if line.len() < 3 {
            continue;
        }
        if code.is_none() {
            code = Some(
                line[..3]
                    .parse()
                    .map_err(|_| anyhow!("malformed SMTP reply code from the relay"))?,
            );
        }
        parts.push(line.get(4..).unwrap_or_default());
        if line.as_bytes().get(3).copied() != Some(b'-') {
            break;
        }
    }
    let code = code.ok_or_else(|| anyhow!("empty SMTP reply from the relay"))?;
    Ok(Response::new(
        code,
        (code / 100) as u8,
        ((code / 10) % 10) as u8,
        (code % 10) as u8,
        parts.join(" "),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_line_replies() {
        let response = build_response(b"220 mx.example.test ESMTP\r\n").unwrap();
        assert_eq!(response.code, 220);
        assert_eq!(response.message, "mx.example.test ESMTP");
    }

    #[test]
    fn folds_multiline_replies() {
        let raw = b"250-mx.example.test\r\n250-SIZE 10\r\n250 OK\r\n";
        let response = build_response(raw).unwrap();
        assert_eq!(response.code, 250);
        assert!(response.message.contains("SIZE 10"));
        assert!(response.message.ends_with("OK"));
    }

    #[test]
    fn expect_rejects_other_codes() {
        let response = Response::new(421, 4, 4, 1, "try later".to_string());
        assert!(expect(&response, 250, "MAIL FROM").is_err());
        let ok = Response::new(250, 2, 0, 0, "ok".to_string());
        assert!(expect(&ok, 250, "MAIL FROM").is_ok());
    }
}
