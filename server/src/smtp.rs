use crate::delivery;
use crate::state::AppState;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use entity::message;
use mailcore::address;
use mailcore::policy::{self, AuthAction, AuthOutcome};
use mailcore::repo;
use mailcore::stream::{Conn, InboundStream};
use smtp_proto::response::generate::BitToString;
use smtp_proto::{Request, Response};
use std::io;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tracing::{debug, info, warn};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Inbound,
    Submission,
}

/// Accepts connections and runs one SMTP session per connection.
pub async fn serve(listener: TcpListener, state: AppState, mode: Mode) {
    let local = listener
        .local_addr()
        .map(|address| address.to_string())
        .unwrap_or_default();
    info!(listen = %local, mode = ?mode, "SMTP listener ready");

    loop {
        match listener.accept().await {
            Ok((socket, peer)) => {
                let state = state.clone();
                tokio::spawn(async move {
                    if let Err(err) = Session::new(socket, peer, state, mode).run().await {
                        debug!(%peer, error = %err, "SMTP session ended");
                    }
                });
            }
            Err(err) => {
                warn!(error = %err, "failed to accept an SMTP connection");
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }
}

struct Recipient {
    address: String,
    mailbox: Option<entity::mailbox::Model>,
}

struct Session {
    conn: Option<Conn<InboundStream>>,
    peer: SocketAddr,
    state: AppState,
    mode: Mode,
    tls: bool,
    helo: Option<String>,
    authenticated: Option<entity::mailbox::Model>,
    mail_from: Option<String>,
    recipients: Vec<Recipient>,
}

impl Session {
    fn new(socket: TcpStream, peer: SocketAddr, state: AppState, mode: Mode) -> Self {
        Self {
            conn: Some(Conn::new(InboundStream::Plain(socket))),
            peer,
            state,
            mode,
            tls: false,
            helo: None,
            authenticated: None,
            mail_from: None,
            recipients: Vec::new(),
        }
    }

    async fn run(mut self) -> io::Result<()> {
        let hostname = self.state.config.hostname.clone();
        self.reply(220, 2, 0, 0, &format!("{hostname} ESMTP ready"))
            .await?;

        loop {
            let Some(line) = self.conn().read_line(4096).await? else {
                return Ok(());
            };
            if line.is_empty() {
                self.reply(500, 5, 5, 2, "Empty command").await?;
                continue;
            }

            // The parser expects the CRLF terminator to be present.
            let mut terminated = line.clone();
            terminated.extend_from_slice(b"\r\n");
            let request = match Request::parse(&mut terminated.iter()) {
                Ok(request) => request.into_owned(),
                Err(_) => {
                    self.reply(500, 5, 5, 2, "Syntax error, command unrecognized")
                        .await?;
                    continue;
                }
            };

            match request {
                Request::Ehlo { host } | Request::Lhlo { host } => {
                    self.reset_envelope();
                    self.authenticated = None;
                    self.helo = Some(host.clone());
                    self.write_ehlo(&host).await?;
                }
                Request::Helo { host } => {
                    self.reset_envelope();
                    self.helo = Some(host);
                    self.reply(250, 2, 0, 0, &hostname).await?;
                }
                Request::StartTls => self.handle_starttls().await?,
                Request::Auth {
                    mechanism,
                    initial_response,
                } => {
                    let mechanism = mechanism.to_mechanism().to_string();
                    self.handle_auth(&mechanism, &initial_response).await?;
                }
                Request::Mail { from } => self.handle_mail(&from.address).await?,
                Request::Rcpt { to } => self.handle_rcpt(&to.address).await?,
                Request::Data => self.handle_data().await?,
                Request::Rset => {
                    self.reset_envelope();
                    self.reply(250, 2, 0, 0, "OK").await?;
                }
                Request::Noop { .. } => self.reply(250, 2, 0, 0, "OK").await?,
                Request::Vrfy { .. } => {
                    self.reply(252, 2, 5, 2, "Cannot VRFY user, but will accept message")
                        .await?;
                }
                Request::Quit => {
                    self.reply(221, 2, 0, 0, "Bye").await?;
                    return Ok(());
                }
                _ => {
                    self.reply(502, 5, 5, 1, "Command not implemented").await?;
                }
            }
        }
    }

    fn conn(&mut self) -> &mut Conn<InboundStream> {
        self.conn.as_mut().expect("connection is present")
    }

    fn reset_envelope(&mut self) {
        self.mail_from = None;
        self.recipients.clear();
    }

    async fn reply(&mut self, code: u16, e0: u8, e1: u8, e2: u8, message: &str) -> io::Result<()> {
        let response = Response::new(code, e0, e1, e2, message.to_string());
        let mut buffer = Vec::new();
        response.write(&mut buffer)?;
        self.conn().write_all(&buffer).await?;
        self.conn().flush().await
    }

    async fn write_ehlo(&mut self, host: &str) -> io::Result<()> {
        let hostname = self.state.config.hostname.clone();
        let mut lines = vec![
            format!("{hostname} greets {host}"),
            format!("SIZE {}", self.state.config.max_message_size),
            "8BITMIME".to_string(),
            "PIPELINING".to_string(),
            "ENHANCEDSTATUSCODES".to_string(),
        ];
        if self.state.config.starttls_enabled() && !self.tls {
            lines.push("STARTTLS".to_string());
        }
        if self.mode == Mode::Submission {
            lines.push("AUTH PLAIN LOGIN".to_string());
        }

        let mut buffer = Vec::new();
        let last = lines.len() - 1;
        for (index, line) in lines.iter().enumerate() {
            let separator = if index == last { ' ' } else { '-' };
            buffer.extend_from_slice(format!("250{separator}{line}\r\n").as_bytes());
        }
        self.conn().write_all(&buffer).await?;
        self.conn().flush().await
    }

    async fn handle_starttls(&mut self) -> io::Result<()> {
        let Some(config) = self.state.tls.clone() else {
            return self.reply(502, 5, 5, 1, "TLS not available").await;
        };
        if self.tls {
            return self.reply(503, 5, 5, 1, "TLS already active").await;
        }

        // A buffered read cannot survive the switch to TLS, so refuse while the
        // connection is still in the session and self.reply() can reach it.
        if self
            .conn
            .as_ref()
            .expect("connection is present")
            .has_buffered()
        {
            return self.reply(503, 5, 5, 1, "STARTTLS not permitted now").await;
        }

        // The 220 must go out before the handshake, so it is written while the
        // connection is still here: self.reply() reads self.conn and would panic
        // once the connection has been taken for the TLS upgrade.
        self.reply(220, 2, 0, 0, "Ready to start TLS").await?;

        let conn = self.conn.take().expect("connection is present");
        let socket = match conn.into_inner() {
            InboundStream::Plain(socket) => socket,
            other => {
                self.conn = Some(Conn::new(other));
                return self.reply(503, 5, 5, 1, "TLS already active").await;
            }
        };

        let acceptor = tokio_rustls::TlsAcceptor::from(config);
        match acceptor.accept(socket).await {
            Ok(stream) => {
                self.conn = Some(Conn::new(InboundStream::Tls(Box::new(stream))));
                self.tls = true;
                self.helo = None;
                self.mail_from = None;
                self.authenticated = None;
                self.recipients.clear();
                Ok(())
            }
            Err(err) => Err(io::Error::other(format!("TLS handshake failed: {err}"))),
        }
    }

    async fn handle_auth(&mut self, mechanism: &str, initial_response: &str) -> io::Result<()> {
        if self.mode != Mode::Submission {
            return self
                .reply(503, 5, 5, 1, "AUTH not enabled on this listener")
                .await;
        }
        if self.authenticated.is_some() {
            return self.reply(503, 5, 5, 1, "Already authenticated").await;
        }
        if self.helo.is_none() {
            return self.reply(503, 5, 5, 1, "Send EHLO first").await;
        }

        let credentials = match mechanism.to_ascii_uppercase().as_str() {
            "PLAIN" => {
                let encoded = if initial_response.is_empty() {
                    self.challenge("").await?;
                    self.read_auth_line().await?
                } else {
                    initial_response.to_string()
                };
                decode_plain(&encoded)
            }
            "LOGIN" => {
                let username = if initial_response.is_empty() {
                    self.challenge("VXNlcm5hbWU6").await?;
                    self.read_auth_line().await?
                } else {
                    initial_response.to_string()
                };
                let Some(username) = decode_base64(&username) else {
                    return self.reply(501, 5, 5, 2, "Invalid credentials").await;
                };
                self.challenge("UGFzc3dvcmQ6").await?;
                let Some(password) = decode_base64(&self.read_auth_line().await?) else {
                    return self.reply(501, 5, 5, 2, "Invalid credentials").await;
                };
                Some((username, password))
            }
            _ => {
                return self
                    .reply(504, 5, 5, 4, "Unsupported authentication mechanism")
                    .await;
            }
        };

        let Some((username, password)) = credentials else {
            return self.reply(501, 5, 5, 2, "Invalid credentials").await;
        };

        if !self.tls {
            warn!(peer = %self.peer, "AUTH attempted without TLS");
        }

        let mailbox = match repo::find_active_mailbox(&self.state.db, &username).await {
            Ok(Some(mailbox)) => mailbox,
            Ok(None) => return self.reply(535, 5, 7, 8, "Authentication failed").await,
            Err(err) => {
                warn!(error = %err, "mailbox lookup failed during AUTH");
                return self.reply(451, 4, 3, 0, "Temporary failure").await;
            }
        };

        if !mailcore::auth::verify_password(&mailbox.password_hash, &password) {
            return self.reply(535, 5, 7, 8, "Authentication failed").await;
        }

        self.authenticated = Some(mailbox);
        self.reply(235, 2, 7, 0, "Authentication successful").await
    }

    /// Writes a bare 334 challenge line (smtp-proto's generator would prepend an
    /// enhanced status code, which is not valid here).
    async fn challenge(&mut self, payload: &str) -> io::Result<()> {
        let buffer = format!("334 {payload}\r\n");
        self.conn().write_all(buffer.as_bytes()).await?;
        self.conn().flush().await
    }

    async fn read_auth_line(&mut self) -> io::Result<String> {
        let line = self.conn().read_line(4096).await?.unwrap_or_default();
        Ok(String::from_utf8_lossy(&line).trim().to_string())
    }

    async fn handle_mail(&mut self, raw: &str) -> io::Result<()> {
        if self.helo.is_none() {
            return self.reply(503, 5, 5, 1, "Send HELO/EHLO first").await;
        }
        self.reset_envelope();
        let parsed = address::parse(raw);

        if self.mode == Mode::Submission {
            let Some(authenticated) = self.authenticated.clone() else {
                return self.reply(530, 5, 7, 0, "Authentication required").await;
            };
            let Some(parsed) = parsed else {
                return self.reply(553, 5, 1, 7, "Sender address rejected").await;
            };
            if parsed.normalized() != authenticated.address {
                return self
                    .reply(
                        550,
                        5,
                        7,
                        1,
                        "Sender address must match the authenticated mailbox",
                    )
                    .await;
            }
            self.mail_from = Some(parsed.normalized());
        } else {
            // The null sender is allowed inbound (bounces); anything else is normalized.
            self.mail_from = Some(parsed.map(|parsed| parsed.normalized()).unwrap_or_default());
        }

        self.reply(250, 2, 1, 0, "Sender OK").await
    }

    async fn handle_rcpt(&mut self, raw: &str) -> io::Result<()> {
        if self.mail_from.is_none() {
            return self.reply(503, 5, 5, 1, "Need MAIL command").await;
        }
        if self.recipients.len() >= self.state.config.max_recipients {
            return self.reply(452, 4, 5, 3, "Too many recipients").await;
        }

        let Some(parsed) = address::parse(raw) else {
            return self.reply(501, 5, 1, 3, "Invalid recipient address").await;
        };
        let normalized = parsed.normalized();

        let mailbox = match repo::find_active_mailbox(&self.state.db, &normalized).await {
            Ok(mailbox) => mailbox,
            Err(err) => {
                warn!(error = %err, "mailbox lookup failed during RCPT");
                return self.reply(451, 4, 3, 0, "Temporary failure").await;
            }
        };

        if mailbox.is_none() && self.mode == Mode::Inbound {
            return self.reply(550, 5, 1, 1, "No such user here").await;
        }

        self.recipients.push(Recipient {
            address: normalized,
            mailbox,
        });
        self.reply(250, 2, 1, 5, "Recipient OK").await
    }

    async fn handle_data(&mut self) -> io::Result<()> {
        if self.mail_from.is_none() {
            return self.reply(503, 5, 5, 1, "Need MAIL command").await;
        }
        if self.recipients.is_empty() {
            return self.reply(554, 5, 5, 1, "No valid recipients").await;
        }

        self.reply(354, 3, 0, 0, "End data with <CR><LF>.<CR><LF>")
            .await?;

        let max_size = self.state.config.max_message_size;
        let raw = match self.conn().read_data(max_size).await {
            Ok(raw) => raw,
            Err(err) if err.kind() == io::ErrorKind::InvalidData => {
                self.reset_envelope();
                return self.reply(552, 5, 3, 4, "Message size exceeds limit").await;
            }
            Err(err) => return Err(err),
        };

        let outcome = self.authenticate(&raw).await;
        if outcome.action == AuthAction::Reject {
            self.reset_envelope();
            return self
                .reply(550, 5, 7, 1, "Message rejected by DMARC policy")
                .await;
        }

        let envelope_from = self.mail_from.clone().unwrap_or_default();
        let recipients = std::mem::take(&mut self.recipients);
        // The envelope recipient list, kept for the sender's own copy: a copy
        // in Sent shows who the message went to, not the sender's address.
        let envelope_to = recipients
            .iter()
            .map(|recipient| recipient.address.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let mut local = Vec::new();
        let mut remote = Vec::new();
        for recipient in recipients {
            match recipient.mailbox {
                Some(mailbox) => local.push(mailbox),
                None => remote.push(recipient.address),
            }
        }

        let mut failed = false;
        // A submission may name blind recipients: the copy the Sent folder keeps
        // has the Bcc header, each blind recipient's copy names them and nobody
        // else, and the copies that reach anybody else name no blind recipient.
        // Inbound mail is handed on as it arrived, which stays the record of what
        // was sent.
        let blind = match self.mode {
            Mode::Submission => mailcore::body::blind_recipients(&raw),
            Mode::Inbound => Vec::new(),
        };
        let is_blind = |address: &str| {
            blind
                .iter()
                .any(|blind| blind.eq_ignore_ascii_case(address))
        };
        // The copy this submission owns, so its row can report the job state.
        let mut copy_message_id = None;

        // A submission keeps the sender's own copy: that is what its Sent folder
        // shows. Inbound mail has no local sender to file a copy for.
        if let (Mode::Submission, Some(sender)) = (self.mode, self.authenticated.as_ref()) {
            match delivery::store_local(
                &self.state,
                sender,
                message::Direction::Outbound,
                &envelope_from,
                &envelope_to,
                &raw,
                &outcome,
            )
            .await
            {
                Ok(id) => copy_message_id = Some(id),
                Err(err) => {
                    warn!(error = %err, "failed to store the sender's copy");
                    failed = true;
                }
            }
        }

        // Either way a local recipient gets it in its inbox.
        for mailbox in &local {
            let copy = delivery::recipient_copy(&raw, &mailbox.address, is_blind(&mailbox.address));
            let payload: &[u8] = match self.mode {
                Mode::Submission => &copy,
                Mode::Inbound => &raw,
            };
            if let Err(err) = delivery::store_local(
                &self.state,
                mailbox,
                message::Direction::Inbound,
                &envelope_from,
                &mailbox.address,
                payload,
                &outcome,
            )
            .await
            {
                warn!(error = %err, "failed to store a message");
                failed = true;
            }
        }

        if !remote.is_empty() {
            let mailbox_id = self.authenticated.as_ref().map(|mailbox| mailbox.id);
            let queued = match self.mode {
                // A blind recipient is handed its own copy, which names it.
                Mode::Submission => {
                    let (blind_remote, visible_remote): (Vec<String>, Vec<String>) =
                        remote.into_iter().partition(|address| is_blind(address));
                    delivery::enqueue_send(
                        &self.state,
                        mailbox_id,
                        delivery::Send {
                            envelope_from: &envelope_from,
                            raw: &raw,
                            visible: &visible_remote,
                            blind: &blind_remote,
                            not_before: None,
                            copies: delivery::SendCopies {
                                copy_message_id,
                                scheduled_message_id: None,
                            },
                        },
                    )
                    .await
                    .map(|_| ())
                }
                // Inbound mail goes out exactly as it arrived.
                Mode::Inbound => delivery::enqueue_remote(
                    &self.state,
                    mailbox_id,
                    &envelope_from,
                    &remote,
                    &raw,
                    copy_message_id,
                )
                .await
                .map(|_| ()),
            };
            if let Err(err) = queued {
                warn!(error = %err, "failed to queue an outbound message");
                failed = true;
            }
        }

        self.reset_envelope();
        if failed {
            self.reply(451, 4, 3, 0, "Temporary failure storing the message")
                .await
        } else {
            self.reply(250, 2, 0, 0, "Message accepted").await
        }
    }

    /// Runs SPF/DKIM/DMARC on inbound mail; submission (authenticated) mail is
    /// trusted and skips verification.
    async fn authenticate(&self, raw: &[u8]) -> AuthOutcome {
        if self.mode != Mode::Inbound || !self.state.config.verify_auth {
            return AuthOutcome::skipped();
        }
        let Some(authenticator) = self.state.authenticator.clone() else {
            return AuthOutcome::skipped();
        };
        let helo = self.helo.clone().unwrap_or_else(|| "unknown".to_string());
        let mail_from = self.mail_from.clone().unwrap_or_default();
        policy::evaluate(
            &authenticator,
            raw,
            self.peer.ip(),
            &helo,
            &mail_from,
            &self.state.config.hostname,
        )
        .await
    }
}

fn decode_base64(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    let decoded = STANDARD.decode(trimmed).ok()?;
    Some(String::from_utf8_lossy(&decoded).to_string())
}

/// Decodes the base64 AUTH PLAIN payload: [authzid] NUL authcid NUL passwd.
fn decode_plain(value: &str) -> Option<(String, String)> {
    let decoded = decode_base64(value)?;
    let mut parts = decoded.split('\0');
    let _authzid = parts.next();
    let username = parts.next()?.to_string();
    let password = parts.next()?.to_string();
    if username.is_empty() {
        return None;
    }
    Some((username, password))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_plain_credentials() {
        let encoded = STANDARD.encode("\0alice@example.test\0hunter2");
        assert_eq!(
            decode_plain(&encoded).unwrap(),
            ("alice@example.test".to_string(), "hunter2".to_string())
        );
    }

    #[test]
    fn rejects_garbage_credentials() {
        assert!(decode_plain("not-base64!!").is_none());
        assert!(decode_plain(&STANDARD.encode("nul-less")).is_none());
    }
}
