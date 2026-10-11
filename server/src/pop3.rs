//! The POP3 listener.
//!
//! RFC 1939 with the UIDL and TOP extensions and the CAPA list. A session reads
//! the inbox drop, marks a message read when it is retrieved, and removes what
//! the client deleted when the session ends. Like the IMAP listener it is
//! implicit TLS whenever a certificate is configured.

use crate::state::AppState;
use entity::mailbox;
use mailcore::query::{self, FolderEntry};
use mailcore::stream::{Conn, InboundStream};
use mailcore::{auth, repo};
use std::collections::HashSet;
use std::io;
use std::net::SocketAddr;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tracing::{debug, info, warn};
use uuid::Uuid;

/// The folder keys the drop holds, matching the IMAP inbox.
const DROP_FOLDERS: [&str; 2] = ["inbox", "snoozed"];

/// Mailboxes with a session open: RFC 1939 lets only one client read a drop.
static LOCKS: OnceLock<Mutex<HashSet<Uuid>>> = OnceLock::new();

fn locks() -> &'static Mutex<HashSet<Uuid>> {
    LOCKS.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Holds the maildrop open for as long as the session lives.
struct MaildropLock(Uuid);

impl Drop for MaildropLock {
    fn drop(&mut self) {
        if let Ok(mut held) = locks().lock() {
            held.remove(&self.0);
        }
    }
}

/// Accepts connections and runs one POP3 session per connection.
pub async fn serve(listener: TcpListener, state: AppState) {
    let local = listener
        .local_addr()
        .map(|address| address.to_string())
        .unwrap_or_default();
    info!(listen = %local, "POP3 listener ready");

    loop {
        match listener.accept().await {
            Ok((socket, peer)) => {
                let state = state.clone();
                tokio::spawn(async move {
                    if let Err(err) = Session::new(socket, peer, state).run().await {
                        debug!(%peer, error = %err, "POP3 session ended");
                    }
                });
            }
            Err(err) => {
                warn!(error = %err, "failed to accept a POP3 connection");
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }
}

fn io_err(err: impl std::fmt::Display) -> io::Error {
    io::Error::other(err.to_string())
}

struct Session {
    conn: Option<Conn<InboundStream>>,
    peer: SocketAddr,
    state: AppState,
    /// The user named by USER, before PASS proves it.
    pending_user: Option<String>,
    account: Option<mailbox::Model>,
    /// The drop as it was when the session opened.
    entries: Vec<FolderEntry>,
    deleted: HashSet<Uuid>,
    _lock: Option<MaildropLock>,
}

impl Session {
    fn new(socket: TcpStream, peer: SocketAddr, state: AppState) -> Self {
        Self {
            conn: Some(Conn::new(InboundStream::Plain(socket))),
            peer,
            state,
            pending_user: None,
            account: None,
            entries: Vec::new(),
            deleted: HashSet::new(),
            _lock: None,
        }
    }

    async fn run(mut self) -> io::Result<()> {
        debug!(peer = %self.peer, "POP3 session started");
        // The listener is implicit TLS whenever a certificate is configured.
        if let Some(config) = self.state.tls.clone() {
            let conn = self.conn.take().expect("connection is present");
            let socket = match conn.into_inner() {
                InboundStream::Plain(socket) => socket,
                InboundStream::Tls(_) => unreachable!("a fresh connection is plain"),
            };
            let acceptor = tokio_rustls::TlsAcceptor::from(config);
            let stream = acceptor
                .accept(socket)
                .await
                .map_err(|err| io::Error::other(format!("TLS handshake failed: {err}")))?;
            self.conn = Some(Conn::new(InboundStream::Tls(Box::new(stream))));
        }

        self.ok("cPOD Mail POP3 ready").await?;

        loop {
            let line = {
                let conn = self.conn.as_mut().expect("connection is present");
                match conn.read_line(1024).await? {
                    Some(line) => line,
                    None => break,
                }
            };
            let text = String::from_utf8_lossy(&line).trim().to_string();
            let mut parts = text.split_whitespace();
            let command = parts.next().unwrap_or("").to_ascii_uppercase();
            let argument = parts.next().unwrap_or("").to_string();
            if self.dispatch(&command, &argument).await? {
                break;
            }
        }

        self.apply_deletions().await
    }

    // -- responses ---------------------------------------------------------

    async fn send(&mut self, text: &str) -> io::Result<()> {
        let conn = self.conn.as_mut().expect("connection is present");
        conn.write_all(text.as_bytes()).await?;
        conn.flush().await
    }

    async fn ok(&mut self, text: &str) -> io::Result<()> {
        self.send(&format!("+OK {text}\r\n")).await
    }

    async fn err(&mut self, text: &str) -> io::Result<()> {
        self.send(&format!("-ERR {text}\r\n")).await
    }

    /// A multi-line response: the lines, dot-stuffed, ended by a lone dot.
    async fn multi(&mut self, lines: &[String]) -> io::Result<()> {
        let mut out = String::new();
        for line in lines {
            if line.starts_with('.') {
                out.push('.');
            }
            out.push_str(line);
            out.push_str("\r\n");
        }
        out.push_str(".\r\n");
        self.send(&out).await
    }

    // -- dispatch ----------------------------------------------------------

    /// Returns true when the session should end.
    async fn dispatch(&mut self, command: &str, argument: &str) -> io::Result<bool> {
        if self.account.is_none() {
            match command {
                "USER" => {
                    self.pending_user = Some(argument.to_string());
                    self.ok("Send PASS").await?;
                }
                "PASS" => self.login(argument).await?,
                "AUTH" => self.authenticate(argument).await?,
                "APOP" => {
                    self.err("APOP is not supported; use USER and PASS").await?;
                }
                "CAPA" => self.capa().await?,
                "STLS" => {
                    self.err("TLS is already active").await?;
                }
                "QUIT" => {
                    self.ok("cPOD Mail signing off").await?;
                    return Ok(true);
                }
                "NOOP" => self.ok("").await?,
                _ => self.err("Authenticate first").await?,
            }
            return Ok(false);
        }

        match command {
            "CAPA" => self.capa().await?,
            "STAT" => {
                let (count, octets) = self.totals();
                self.ok(&format!("{count} {octets}")).await?;
            }
            "LIST" => self.list(argument).await?,
            "UIDL" => self.uidl(argument).await?,
            "RETR" => self.retrieve(argument, None).await?,
            "TOP" => self.top(argument).await?,
            "DELE" => self.delete(argument).await?,
            "RSET" => {
                self.deleted.clear();
                self.ok("Maildrop reset").await?;
            }
            "NOOP" => self.ok("").await?,
            "USER" | "PASS" => self.err("Already authenticated").await?,
            "STLS" => self.err("TLS is already active").await?,
            "QUIT" => {
                self.ok("cPOD Mail signing off").await?;
                return Ok(true);
            }
            _ => self.err("Unknown command").await?,
        }
        Ok(false)
    }

    async fn capa(&mut self) -> io::Result<()> {
        self.send("+OK Capability list follows\r\n").await?;
        self.multi(&[
            "TOP".to_string(),
            "USER".to_string(),
            "UIDL".to_string(),
            "SASL PLAIN LOGIN".to_string(),
            "RESP-CODES".to_string(),
        ])
        .await
    }

    // -- authentication ----------------------------------------------------

    async fn login(&mut self, password: &str) -> io::Result<()> {
        let Some(user) = self.pending_user.clone() else {
            return self.err("Send USER first").await;
        };
        let found = repo::find_active_mailbox(&self.state.db, &user)
            .await
            .map_err(io_err)?;
        let Some(account) = found else {
            return self.err("[AUTH] Invalid credentials").await;
        };
        if !auth::verify_password(&account.password_hash, password) {
            return self.err("[AUTH] Invalid credentials").await;
        }
        self.open(account).await
    }

    async fn authenticate(&mut self, argument: &str) -> io::Result<()> {
        let mut parts = argument.split_whitespace();
        let mechanism = parts.next().unwrap_or("").to_ascii_uppercase();
        if !matches!(mechanism.as_str(), "PLAIN" | "LOGIN") {
            return self.err("Unsupported authentication mechanism").await;
        }
        let encoded = match parts.next() {
            Some(value) => value.to_string(),
            None => {
                self.send("+ \r\n").await?;
                let conn = self.conn.as_mut().expect("connection is present");
                match conn.read_line(64 * 1024).await? {
                    Some(line) => String::from_utf8_lossy(&line).trim().to_string(),
                    None => return Ok(()),
                }
            }
        };
        let decoded = decode_base64(&encoded);
        let (user, password) = split_plain(&decoded);
        self.pending_user = Some(user);
        self.login(&password).await
    }

    /// Opens the drop for a proven account, refusing a second reader.
    async fn open(&mut self, account: mailbox::Model) -> io::Result<()> {
        let already_open = {
            let mut held = locks().lock().expect("the lock is never poisoned");
            !held.insert(account.id)
        };
        if already_open {
            return self
                .err("[IN-USE] This mailbox already has a POP3 session")
                .await;
        }
        self._lock = Some(MaildropLock(account.id));

        let entries = query::folder_entries(&self.state.db, account.id, &DROP_FOLDERS)
            .await
            .map_err(io_err)?;
        self.entries = entries;
        self.account = Some(account);
        let count = self.entries.len();
        self.ok(&format!("Maildrop ready, {count} messages")).await
    }

    /// The count and total size of the messages still in the drop.
    fn totals(&self) -> (usize, u64) {
        let live = self
            .entries
            .iter()
            .filter(|entry| !self.deleted.contains(&entry.id));
        let mut count = 0usize;
        let mut octets = 0u64;
        for entry in live {
            count += 1;
            octets += entry.size_bytes.max(0) as u64;
        }
        (count, octets)
    }

    /// The index of the message the argument names, when it is still in the drop.
    fn index_of(&self, argument: &str) -> Option<usize> {
        let number: usize = argument.trim().parse().ok()?;
        if number == 0 || number > self.entries.len() {
            return None;
        }
        let entry = &self.entries[number - 1];
        (!self.deleted.contains(&entry.id)).then_some(number - 1)
    }

    async fn send_bytes(&mut self, bytes: &[u8]) -> io::Result<()> {
        let conn = self.conn.as_mut().expect("connection is present");
        conn.write_all(bytes).await?;
        conn.flush().await
    }

    async fn list(&mut self, argument: &str) -> io::Result<()> {
        if argument.is_empty() {
            let lines: Vec<String> = self
                .entries
                .iter()
                .enumerate()
                .filter(|(_, entry)| !self.deleted.contains(&entry.id))
                .map(|(index, entry)| format!("{} {}", index + 1, entry.size_bytes))
                .collect();
            let (count, octets) = self.totals();
            self.send(&format!("+OK {count} messages ({octets} octets)\r\n"))
                .await?;
            return self.multi(&lines).await;
        }
        match self.index_of(argument) {
            Some(index) => {
                let line = format!("{} {}", index + 1, self.entries[index].size_bytes);
                self.ok(&line).await
            }
            None => self.err("No such message").await,
        }
    }

    async fn uidl(&mut self, argument: &str) -> io::Result<()> {
        if argument.is_empty() {
            let lines: Vec<String> = self
                .entries
                .iter()
                .enumerate()
                .filter(|(_, entry)| !self.deleted.contains(&entry.id))
                .map(|(index, entry)| format!("{} {}", index + 1, entry.uid))
                .collect();
            self.send("+OK Unique-id listing follows\r\n").await?;
            return self.multi(&lines).await;
        }
        match self.index_of(argument) {
            Some(index) => {
                let line = format!("{} {}", index + 1, self.entries[index].uid);
                self.ok(&line).await
            }
            None => self.err("No such message").await,
        }
    }

    async fn top(&mut self, argument: &str) -> io::Result<()> {
        let mut parts = argument.split_whitespace();
        let number = parts.next().unwrap_or("").to_string();
        let lines: usize = parts.next().unwrap_or("0").parse().unwrap_or(0);
        self.retrieve(&number, Some(lines)).await
    }

    async fn retrieve(&mut self, argument: &str, lines: Option<usize>) -> io::Result<()> {
        let Some(index) = self.index_of(argument) else {
            return self.err("No such message").await;
        };
        let entry = self.entries[index].clone();
        let raw = self
            .state
            .store
            .get(&entry.storage_key)
            .await
            .map_err(io_err)?;
        // Reading a message is what marks it read, as POP3 has no flag command.
        if let Err(err) = repo::update_flags(
            &self.state.db,
            entry.id,
            repo::FlagUpdate {
                read: Some(true),
                ..Default::default()
            },
        )
        .await
        {
            warn!(error = %err, message = %entry.id, "failed to mark a retrieved message read");
        }

        let body = match lines {
            Some(lines) => top_bytes(&raw, lines),
            None => normalize_crlf(&raw),
        };
        self.send(&format!("+OK {} octets\r\n", body.len())).await?;
        self.send_dot_block(&body).await
    }

    /// Writes a message body, dot-stuffed, ended by a lone dot.
    async fn send_dot_block(&mut self, body: &[u8]) -> io::Result<()> {
        let mut out = Vec::with_capacity(body.len() + 8);
        for line in body.split_inclusive(|byte| *byte == b'\n') {
            if line.first() == Some(&b'.') {
                out.push(b'.');
            }
            out.extend_from_slice(line);
        }
        if !out.ends_with(b"\n") {
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(b".\r\n");
        self.send_bytes(&out).await
    }

    async fn delete(&mut self, argument: &str) -> io::Result<()> {
        match self.index_of(argument) {
            Some(index) => {
                self.deleted.insert(self.entries[index].id);
                self.ok("Message marked for deletion").await
            }
            None => self.err("No such message").await,
        }
    }

    /// Removes what the client deleted, once the session is over.
    async fn apply_deletions(&mut self) -> io::Result<()> {
        let doomed: Vec<FolderEntry> = self
            .entries
            .iter()
            .filter(|entry| self.deleted.contains(&entry.id))
            .cloned()
            .collect();
        for entry in doomed {
            if let Err(err) = self.state.store.delete(&entry.storage_key).await {
                warn!(error = %err, key = %entry.storage_key, "failed to remove a deleted message");
            }
            if let Err(err) = repo::delete_message(&self.state.db, entry.id).await {
                warn!(error = %err, message = %entry.id, "failed to remove a deleted row");
            }
        }
        Ok(())
    }
}

// -- helpers ---------------------------------------------------------------

fn decode_base64(value: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(value.trim())
        .map(|bytes| String::from_utf8_lossy(&bytes).to_string())
        .unwrap_or_default()
}

/// The user and password a decoded SASL PLAIN payload carries. The payload is an
/// authorization id, an authentication id and a password, NUL separated, and the
/// authorization id is empty in practice.
fn split_plain(decoded: &str) -> (String, String) {
    let mut fields = decoded.split(char::from(0));
    let _authzid = fields.next();
    let user = fields.next().unwrap_or("").to_string();
    let password = fields.next().unwrap_or("").to_string();
    (user, password)
}

/// Rewrites lone line feeds as CRLF, which is what the protocol transmits.
fn normalize_crlf(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len() + raw.len() / 16);
    let mut previous = 0u8;
    for byte in raw {
        if *byte == b'\n' && previous != b'\r' {
            out.push(b'\r');
        }
        out.push(*byte);
        previous = *byte;
    }
    out
}

/// The offset just past the blank line that ends the headers, when there is one.
fn header_end(raw: &[u8]) -> Option<usize> {
    for (index, window) in raw.windows(4).enumerate() {
        if window == b"\r\n\r\n" {
            return Some(index + 4);
        }
    }
    for (index, window) in raw.windows(2).enumerate() {
        if window == b"\n\n" {
            return Some(index + 2);
        }
    }
    None
}

/// The headers plus a prefix of the body, which is what TOP asks for.
fn top_bytes(raw: &[u8], lines: usize) -> Vec<u8> {
    let normalized = normalize_crlf(raw);
    let Some(end) = header_end(&normalized) else {
        return normalized;
    };
    let (header, body) = normalized.split_at(end);
    let mut out = header.to_vec();
    for line in body.split_inclusive(|byte| *byte == b'\n').take(lines) {
        out.extend_from_slice(line);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_lone_line_feeds() {
        assert_eq!(normalize_crlf(b"a\nb\r\nc"), b"a\r\nb\r\nc");
        assert_eq!(normalize_crlf(b""), b"");
    }

    #[test]
    fn top_keeps_the_headers_and_a_prefix_of_the_body() {
        let raw = b"Subject: Hi\n\none\ntwo\nthree\n";
        assert_eq!(
            top_bytes(raw, 2),
            b"Subject: Hi\r\n\r\none\r\ntwo\r\n".to_vec(),
            "two body lines follow the header block"
        );
        assert_eq!(
            top_bytes(raw, 0),
            b"Subject: Hi\r\n\r\n".to_vec(),
            "zero lines asks for the headers alone"
        );
        assert_eq!(
            top_bytes(b"Subject: Hi\r\n\r\nbody\r\n", 9),
            b"Subject: Hi\r\n\r\nbody\r\n".to_vec()
        );
    }

    #[test]
    fn finds_the_header_block() {
        assert_eq!(header_end(b"a\r\n\r\nbody"), Some(5));
        assert_eq!(header_end(b"a\n\nbody"), Some(3));
        assert_eq!(header_end(b"no body here"), None);
    }

    #[test]
    fn reads_a_sasl_plain_initial_response() {
        let decoded = decode_base64("AGFsaWNlQGV4YW1wbGUudGVzdABzZWNyZXQ=");
        assert_eq!(decoded, "\u{0}alice@example.test\u{0}secret");
        let (user, password) = split_plain(&decoded);
        assert_eq!(user, "alice@example.test");
        assert_eq!(password, "secret");
    }
}
