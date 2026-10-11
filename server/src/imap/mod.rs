//! The IMAP4rev1 listener.
//!
//! It is a thin reader over what is already stored: the messages table gives a
//! folder its rows in uid order, the bytes live in S3, and every flag a client
//! sets lands in message_flags. No new storage is introduced here.

mod fetch;
mod search;
mod wire;

use crate::delivery;
use crate::state::AppState;
use chrono::{DateTime, Datelike, Timelike, Utc};
use entity::{folder, mailbox, message, message_flag};
use mailcore::policy::AuthOutcome;
use mailcore::query::{self, FolderEntry};
use mailcore::stream::{Conn, InboundStream};
use mailcore::{auth, meta, repo};
use sea_orm::sea_query::Expr as SeaExpr;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use std::io;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tracing::{debug, info, warn};
use uuid::Uuid;
use wire::{Arg, Command, Incoming};

/// The flags every mailbox reports, and the permanent flags a client may set.
const FLAGS: &str = "\\Answered \\Flagged \\Deleted \\Seen \\Draft";

/// How long an IDLE connection may sit in silence before a keepalive.
const IDLE_KEEPALIVE: Duration = Duration::from_secs(25 * 60);

/// The folders the reader exposes, in the order a client should show them.
fn system_mailboxes() -> Vec<ImapMailbox> {
    vec![
        ImapMailbox {
            name: "INBOX".to_string(),
            folders: vec!["inbox".to_string(), "snoozed".to_string()],
            attributes: vec![],
        },
        ImapMailbox {
            name: "Sent".to_string(),
            folders: vec!["sent".to_string()],
            attributes: vec!["\\Sent".to_string()],
        },
        ImapMailbox {
            name: "Drafts".to_string(),
            folders: vec!["drafts".to_string()],
            attributes: vec!["\\Drafts".to_string()],
        },
        ImapMailbox {
            name: "Trash".to_string(),
            folders: vec!["trash".to_string()],
            attributes: vec!["\\Trash".to_string()],
        },
        ImapMailbox {
            name: "Junk".to_string(),
            folders: vec!["spam".to_string()],
            attributes: vec!["\\Junk".to_string()],
        },
        ImapMailbox {
            name: "Archive".to_string(),
            folders: vec!["archived".to_string()],
            attributes: vec!["\\Archive".to_string()],
        },
    ]
}

/// The names that belong to the system, which a client may not create or drop.
fn system_names() -> Vec<String> {
    system_mailboxes()
        .into_iter()
        .map(|entry| entry.name)
        .collect()
}

/// One mailbox as a client sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ImapMailbox {
    name: String,
    /// The reader folder keys whose messages the mailbox holds.
    folders: Vec<String>,
    attributes: Vec<String>,
}

impl ImapMailbox {
    /// The folder key a message filed here carries.
    fn key(&self) -> String {
        self.folders
            .first()
            .cloned()
            .unwrap_or_else(|| "inbox".to_string())
    }
}

/// Accepts connections and runs one IMAP session per connection.
pub async fn serve(listener: TcpListener, state: AppState) {
    let local = listener
        .local_addr()
        .map(|address| address.to_string())
        .unwrap_or_default();
    info!(listen = %local, "IMAP listener ready");

    loop {
        match listener.accept().await {
            Ok((socket, peer)) => {
                let state = state.clone();
                tokio::spawn(async move {
                    if let Err(err) = Session::new(socket, peer, state).run().await {
                        debug!(%peer, error = %err, "IMAP session ended");
                    }
                });
            }
            Err(err) => {
                warn!(error = %err, "failed to accept an IMAP connection");
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }
}

fn io_err(err: impl std::fmt::Display) -> io::Error {
    io::Error::other(err.to_string())
}

/// The mailbox a session selected, and whether it may be written to.
struct Selected {
    name: String,
    folders: Vec<String>,
    read_only: bool,
}

struct Session {
    conn: Option<Conn<InboundStream>>,
    peer: SocketAddr,
    state: AppState,
    account: Option<mailbox::Model>,
    selected: Option<Selected>,
}

enum Control {
    Continue,
    Close,
}

impl Session {
    fn new(socket: TcpStream, peer: SocketAddr, state: AppState) -> Self {
        Self {
            conn: Some(Conn::new(InboundStream::Plain(socket))),
            peer,
            state,
            account: None,
            selected: None,
        }
    }

    async fn run(mut self) -> io::Result<()> {
        debug!(peer = %self.peer, "IMAP session started");
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

        self.untagged(&format!(
            "OK [CAPABILITY {}] cPOD Mail IMAP ready",
            capabilities()
        ))
        .await?;

        loop {
            let conn = self.conn.as_mut().expect("connection is present");
            match wire::read_command(conn).await? {
                Incoming::Eof => return Ok(()),
                Incoming::Bad { tag, reason } => {
                    self.bad(&tag, reason).await?;
                }
                Incoming::Command(command) => {
                    if let Control::Close = self.dispatch(command).await? {
                        return Ok(());
                    }
                }
            }
        }
    }

    // -- responses ---------------------------------------------------------

    async fn send(&mut self, bytes: &[u8]) -> io::Result<()> {
        let conn = self.conn.as_mut().expect("connection is present");
        conn.write_all(bytes).await?;
        conn.flush().await
    }

    async fn line(&mut self, text: &str) -> io::Result<()> {
        self.send(text.as_bytes()).await
    }

    async fn untagged(&mut self, text: &str) -> io::Result<()> {
        self.line(&format!("* {text}\r\n")).await
    }

    async fn ok(&mut self, tag: &str, text: &str) -> io::Result<()> {
        self.line(&format!("{tag} OK {text}\r\n")).await
    }

    async fn no(&mut self, tag: &str, text: &str) -> io::Result<()> {
        self.line(&format!("{tag} NO {text}\r\n")).await
    }

    async fn bad(&mut self, tag: &str, text: &str) -> io::Result<()> {
        self.line(&format!("{tag} BAD {text}\r\n")).await
    }

    // -- account and mailboxes --------------------------------------------

    fn mailbox_id(&self) -> Option<Uuid> {
        self.account.as_ref().map(|account| account.id)
    }

    /// The system mailboxes plus this mailbox's own folders.
    async fn mailboxes(&self) -> io::Result<Vec<ImapMailbox>> {
        let mut list = system_mailboxes();
        let Some(id) = self.mailbox_id() else {
            return Ok(list);
        };
        let custom = folder::Entity::find()
            .filter(folder::Column::MailboxId.eq(id))
            .all(&self.state.db)
            .await
            .map_err(io_err)?;
        list.extend(custom.into_iter().map(|row| ImapMailbox {
            name: row.name,
            folders: vec![row.id.to_string()],
            attributes: vec![],
        }));
        Ok(list)
    }

    async fn resolve_mailbox(&self, name: &str) -> io::Result<Option<ImapMailbox>> {
        Ok(self
            .mailboxes()
            .await?
            .into_iter()
            .find(|entry| entry.name.eq_ignore_ascii_case(name)))
    }

    /// The rows of the selected mailbox, oldest uid first.
    async fn entries(&self) -> io::Result<Vec<FolderEntry>> {
        let Some(selected) = self.selected.as_ref() else {
            return Ok(Vec::new());
        };
        let folders: Vec<&str> = selected.folders.iter().map(String::as_str).collect();
        self.entries_for(&folders).await
    }

    async fn entries_for(&self, folders: &[&str]) -> io::Result<Vec<FolderEntry>> {
        let Some(id) = self.mailbox_id() else {
            return Ok(Vec::new());
        };
        query::folder_entries(&self.state.db, id, folders)
            .await
            .map_err(io_err)
    }

    fn uid_validity(&self) -> i32 {
        self.account
            .as_ref()
            .map(|account| account.uid_validity)
            .unwrap_or(1)
    }

    /// True when the selected mailbox files its messages as drafts.
    fn selected_is_drafts(&self) -> bool {
        self.selected
            .as_ref()
            .is_some_and(|selected| selected.folders.iter().any(|folder| folder == "drafts"))
    }

    /// True when the mailbox was opened with EXAMINE, which forbids writes.
    fn read_only(&self) -> bool {
        self.selected
            .as_ref()
            .is_some_and(|selected| selected.read_only)
    }

    // -- dispatch ----------------------------------------------------------

    async fn dispatch(&mut self, command: Command) -> io::Result<Control> {
        let Command { tag, name, args } = command;
        let authenticated = self.account.is_some();

        match name.as_str() {
            "CAPABILITY" => {
                self.untagged(&format!("CAPABILITY {}", capabilities()))
                    .await?;
                self.ok(&tag, "CAPABILITY completed").await?;
            }
            "NOOP" => self.ok(&tag, "NOOP completed").await?,
            "LOGOUT" => {
                self.untagged("BYE cPOD Mail signing off").await?;
                self.ok(&tag, "LOGOUT completed").await?;
                return Ok(Control::Close);
            }
            "ID" => {
                self.untagged("ID (\"name\" \"cPOD Mail\" \"version\" \"0.1.0\")")
                    .await?;
                self.ok(&tag, "ID completed").await?;
            }
            "LOGIN" => {
                let user = args.first().map(Arg::text).unwrap_or("").to_string();
                let password = args.get(1).map(Arg::text).unwrap_or("").to_string();
                self.login(&tag, &user, &password).await?;
            }
            "AUTHENTICATE" => {
                let mechanism = args.first().map(Arg::upper).unwrap_or_default();
                let initial = args.get(1).map(Arg::text).map(str::to_string);
                self.authenticate(&tag, &mechanism, initial).await?;
            }
            _ if !authenticated => {
                self.bad(&tag, "Please authenticate first").await?;
            }
            "NAMESPACE" => {
                self.untagged("NAMESPACE ((\"\" \"/\")) NIL NIL").await?;
                self.ok(&tag, "NAMESPACE completed").await?;
            }
            "SELECT" | "EXAMINE" => {
                let mailbox = args.first().map(Arg::text).unwrap_or("").to_string();
                self.select(&tag, &mailbox, name == "EXAMINE").await?;
            }
            "LIST" | "LSUB" => {
                let reference = args.first().map(Arg::text).unwrap_or("").to_string();
                let pattern = args.get(1).map(Arg::text).unwrap_or("*").to_string();
                self.list(&tag, &reference, &pattern).await?;
            }
            "STATUS" => {
                let mailbox = args.first().map(Arg::text).unwrap_or("").to_string();
                let items: Vec<String> = args
                    .get(1)
                    .and_then(Arg::list)
                    .map(|list| list.iter().map(Arg::upper).collect())
                    .unwrap_or_default();
                self.status(&tag, &mailbox, &items).await?;
            }
            "CREATE" => {
                let mailbox = args.first().map(Arg::text).unwrap_or("").to_string();
                self.create_folder(&tag, &mailbox).await?;
            }
            "DELETE" => {
                let mailbox = args.first().map(Arg::text).unwrap_or("").to_string();
                self.delete_folder(&tag, &mailbox).await?;
            }
            "RENAME" => {
                let from = args.first().map(Arg::text).unwrap_or("").to_string();
                let to = args.get(1).map(Arg::text).unwrap_or("").to_string();
                self.rename_folder(&tag, &from, &to).await?;
            }
            "SUBSCRIBE" | "UNSUBSCRIBE" => self.ok(&tag, "Completed").await?,
            "APPEND" => self.append(tag.clone(), args).await?,
            "CHECK" => self.ok(&tag, "CHECK completed").await?,
            "CLOSE" => {
                self.expunge(&tag, None, true).await?;
                self.selected = None;
            }
            "UNSELECT" => {
                self.selected = None;
                self.ok(&tag, "UNSELECT completed").await?;
            }
            "EXPUNGE" => self.expunge(&tag, None, false).await?,
            "SEARCH" => self.search(&tag, &args, false).await?,
            "FETCH" => self.fetch(&tag, &args, false).await?,
            "STORE" => self.store(&tag, &args, false).await?,
            "COPY" => self.copy(&tag, &args, false).await?,
            "MOVE" => self.copy(&tag, &args, true).await?,
            "UID" => self.uid_command(&tag, &args).await?,
            "IDLE" => self.idle(&tag).await?,
            "STARTTLS" => self.no(&tag, "TLS is already active").await?,
            _ => self.bad(&tag, "Unknown command").await?,
        }
        Ok(Control::Continue)
    }

    // -- authentication ----------------------------------------------------

    async fn login(&mut self, tag: &str, user: &str, password: &str) -> io::Result<()> {
        if self.account.is_some() {
            return self.bad(tag, "Already authenticated").await;
        }
        let found = repo::find_active_mailbox(&self.state.db, user)
            .await
            .map_err(io_err)?;
        let Some(account) = found else {
            return self
                .no(tag, "[AUTHENTICATIONFAILED] Invalid credentials")
                .await;
        };
        if !auth::verify_password(&account.password_hash, password) {
            return self
                .no(tag, "[AUTHENTICATIONFAILED] Invalid credentials")
                .await;
        }
        self.account = Some(account);
        self.ok(tag, "LOGIN completed").await
    }

    async fn authenticate(
        &mut self,
        tag: &str,
        mechanism: &str,
        initial: Option<String>,
    ) -> io::Result<()> {
        if !matches!(mechanism, "PLAIN" | "LOGIN") {
            return self.no(tag, "Unsupported authentication mechanism").await;
        }
        let encoded = match initial {
            Some(value) if !value.is_empty() => value,
            _ => {
                self.line("+ \r\n").await?;
                let conn = self.conn.as_mut().expect("connection is present");
                match conn.read_line(64 * 1024).await? {
                    Some(line) => String::from_utf8_lossy(&line).trim().to_string(),
                    None => return Ok(()),
                }
            }
        };
        let decoded = decode_base64(&encoded);
        let (user, password) = split_plain(&decoded);
        self.login(tag, &user, &password).await
    }

    // -- mailbox commands --------------------------------------------------

    async fn select(&mut self, tag: &str, name: &str, read_only: bool) -> io::Result<()> {
        let Some(mailbox) = self.resolve_mailbox(name).await? else {
            return self.no(tag, "No such mailbox").await;
        };
        let folders: Vec<&str> = mailbox.folders.iter().map(String::as_str).collect();
        let entries = self.entries_for(&folders).await?;
        let uid_next = self.uid_next().await?;
        let uid_validity = self.uid_validity();
        let unseen = entries.iter().position(|entry| !entry.read);

        self.selected = Some(Selected {
            name: mailbox.name.clone(),
            folders: mailbox.folders.clone(),
            read_only,
        });

        self.untagged(&format!("FLAGS ({FLAGS})")).await?;
        self.untagged(&format!("{} EXISTS", entries.len())).await?;
        self.untagged("0 RECENT").await?;
        if let Some(position) = unseen {
            self.untagged(&format!("OK [UNSEEN {}] First unseen", position + 1))
                .await?;
        }
        self.untagged(&format!("OK [UIDVALIDITY {uid_validity}] UIDs are valid"))
            .await?;
        self.untagged(&format!("OK [UIDNEXT {uid_next}] Predicted next UID"))
            .await?;
        self.untagged(&format!(
            "OK [PERMANENTFLAGS ({FLAGS})] Flags are permanent"
        ))
        .await?;
        let text = if read_only {
            "[READ-ONLY] EXAMINE completed"
        } else {
            "[READ-WRITE] SELECT completed"
        };
        self.ok(tag, text).await
    }

    /// One past the largest uid in the mailbox, which is the UIDNEXT.
    async fn uid_next(&self) -> io::Result<i64> {
        let Some(id) = self.mailbox_id() else {
            return Ok(1);
        };
        Ok(query::max_uid(&self.state.db, id).await.map_err(io_err)? + 1)
    }

    async fn list(&mut self, tag: &str, reference: &str, pattern: &str) -> io::Result<()> {
        // An empty pattern asks for the hierarchy delimiter alone.
        if pattern.is_empty() {
            self.untagged("LIST (\\Noselect) \"/\" \"\"").await?;
            return self.ok(tag, "LIST completed").await;
        }
        let combined = format!("{reference}{pattern}");
        for mailbox in self.mailboxes().await? {
            let matched = combined == "*"
                || pattern_matches(&combined, &mailbox.name)
                || mailbox.name.eq_ignore_ascii_case("INBOX");
            if !matched {
                continue;
            }
            let attributes = if mailbox.attributes.is_empty() {
                "\\HasNoChildren".to_string()
            } else {
                format!("{} \\HasNoChildren", mailbox.attributes.join(" "))
            };
            self.untagged(&format!(
                "LIST ({attributes}) \"/\" {}",
                wire::quote(&mailbox.name)
            ))
            .await?;
        }
        self.ok(tag, "LIST completed").await
    }

    async fn status(&mut self, tag: &str, name: &str, items: &[String]) -> io::Result<()> {
        let Some(mailbox) = self.resolve_mailbox(name).await? else {
            return self.no(tag, "No such mailbox").await;
        };
        let folders: Vec<&str> = mailbox.folders.iter().map(String::as_str).collect();
        let entries = self.entries_for(&folders).await?;
        let unread = entries.iter().filter(|entry| !entry.read).count();
        let uid_next = self.uid_next().await?;
        let uid_validity = self.uid_validity();
        let mut values = Vec::new();
        for item in items {
            match item.as_str() {
                "MESSAGES" => values.push(format!("MESSAGES {}", entries.len())),
                "UNSEEN" => values.push(format!("UNSEEN {unread}")),
                "UIDNEXT" => values.push(format!("UIDNEXT {uid_next}")),
                "UIDVALIDITY" => values.push(format!("UIDVALIDITY {uid_validity}")),
                "RECENT" => values.push("RECENT 0".to_string()),
                _ => {}
            }
        }
        self.untagged(&format!(
            "STATUS {} ({})",
            wire::quote(&mailbox.name),
            values.join(" ")
        ))
        .await?;
        self.ok(tag, "STATUS completed").await
    }

    async fn create_folder(&mut self, tag: &str, name: &str) -> io::Result<()> {
        let Some(id) = self.mailbox_id() else {
            return self.bad(tag, "No mailbox selected").await;
        };
        if name.is_empty() {
            return self.no(tag, "A folder name is required").await;
        }
        if self.resolve_mailbox(name).await?.is_some() {
            return self.no(tag, "Mailbox already exists").await;
        }
        let row = folder::ActiveModel {
            id: Set(Uuid::now_v7()),
            mailbox_id: Set(id),
            name: Set(name.to_string()),
            color: Set("#64748b".to_string()),
            created_at: Set(Utc::now()),
        };
        row.insert(&self.state.db).await.map_err(io_err)?;
        self.ok(tag, "CREATE completed").await
    }

    async fn delete_folder(&mut self, tag: &str, name: &str) -> io::Result<()> {
        let Some(mailbox) = self.resolve_mailbox(name).await? else {
            return self.no(tag, "No such mailbox").await;
        };
        if system_names()
            .iter()
            .any(|system| system.eq_ignore_ascii_case(&mailbox.name))
        {
            return self.no(tag, "The system folders cannot be removed").await;
        }
        let key = mailbox.key();
        // What the folder held goes back to the inbox rather than disappearing.
        message_flag::Entity::update_many()
            .col_expr(message_flag::Column::Folder, SeaExpr::value("inbox"))
            .col_expr(message_flag::Column::UpdatedAt, SeaExpr::value(Utc::now()))
            .filter(message_flag::Column::Folder.eq(key.clone()))
            .exec(&self.state.db)
            .await
            .map_err(io_err)?;
        if let Ok(id) = Uuid::parse_str(&key) {
            folder::Entity::delete_by_id(id)
                .exec(&self.state.db)
                .await
                .map_err(io_err)?;
        }
        if self
            .selected
            .as_ref()
            .is_some_and(|selected| selected.name.eq_ignore_ascii_case(&mailbox.name))
        {
            self.selected = None;
        }
        self.ok(tag, "DELETE completed").await
    }

    async fn rename_folder(&mut self, tag: &str, from: &str, to: &str) -> io::Result<()> {
        let Some(mailbox) = self.resolve_mailbox(from).await? else {
            return self.no(tag, "No such mailbox").await;
        };
        if system_names()
            .iter()
            .any(|system| system.eq_ignore_ascii_case(&mailbox.name))
        {
            return self.no(tag, "The system folders cannot be renamed").await;
        }
        let Ok(id) = Uuid::parse_str(&mailbox.key()) else {
            return self.no(tag, "No such mailbox").await;
        };
        let row = folder::Entity::find_by_id(id)
            .one(&self.state.db)
            .await
            .map_err(io_err)?
            .ok_or_else(|| io::Error::other("folder vanished"))?;
        let mut active: folder::ActiveModel = row.into();
        active.name = Set(to.to_string());
        active.update(&self.state.db).await.map_err(io_err)?;
        if self
            .selected
            .as_ref()
            .is_some_and(|selected| selected.name.eq_ignore_ascii_case(from))
            && let Some(selected) = self.selected.as_mut()
        {
            selected.name = to.to_string();
        }
        self.ok(tag, "RENAME completed").await
    }

    // -- message commands --------------------------------------------------

    async fn append(&mut self, tag: String, args: Vec<Arg>) -> io::Result<()> {
        if self.read_only() {
            return self
                .no(&tag, "[READ-ONLY] The mailbox is open read-only")
                .await;
        }
        let name = args.first().map(Arg::text).unwrap_or("").to_string();
        let literal = args.iter().rev().find_map(|arg| match arg {
            Arg::Literal(bytes) => Some(bytes.clone()),
            _ => None,
        });
        let flag_args: Vec<String> = args
            .iter()
            .find_map(Arg::list)
            .map(|list| list.iter().map(Arg::upper).collect())
            .unwrap_or_default();

        let Some(bytes) = literal else {
            return self.bad(&tag, "APPEND needs a literal").await;
        };
        let Some(target) = self.resolve_mailbox(&name).await? else {
            return self.no(&tag, "[TRYCREATE] No such mailbox").await;
        };
        let Some(account) = self.account.clone() else {
            return self.bad(&tag, "Please authenticate first").await;
        };

        // The send path already files the sender's own copy, so an APPEND of the
        // same message would only duplicate it: answer with the copy already there.
        let folders: Vec<&str> = target.folders.iter().map(String::as_str).collect();
        let existing = self.entries_for(&folders).await?;
        let metadata = meta::extract(&bytes);
        if let Some(message_id) = metadata.message_id.as_deref()
            && let Some(entry) = existing
                .iter()
                .find(|entry| entry.message_id.as_deref() == Some(message_id))
        {
            let uid = entry.uid;
            let uid_validity = self.uid_validity();
            return self
                .ok(
                    &tag,
                    &format!("[APPENDUID {uid_validity} {uid}] Message already stored"),
                )
                .await;
        }

        let direction = if matches!(target.key().as_str(), "drafts" | "sent") {
            message::Direction::Outbound
        } else {
            message::Direction::Inbound
        };
        let id = match delivery::store_local(
            &self.state,
            &account,
            direction,
            &account.address,
            &account.address,
            &bytes,
            &AuthOutcome::skipped(),
        )
        .await
        {
            Ok(id) => id,
            Err(err) => return self.no(&tag, &format!("APPEND failed: {err}")).await,
        };

        let has = |flag: &str| flag_args.iter().any(|candidate| candidate == flag);
        repo::update_flags(
            &self.state.db,
            id,
            repo::FlagUpdate {
                read: Some(has("\\SEEN")),
                starred: Some(has("\\FLAGGED")),
                answered: Some(has("\\ANSWERED")),
                deleted: Some(false),
            },
        )
        .await
        .map_err(io_err)?;
        repo::set_message_folder(&self.state.db, id, &target.key())
            .await
            .map_err(io_err)?;

        let uid = repo::get_message(&self.state.db, id)
            .await
            .map_err(io_err)?
            .map(|row| row.uid)
            .unwrap_or_default();
        let uid_validity = self.uid_validity();
        self.ok(
            &tag,
            &format!("[APPENDUID {uid_validity} {uid}] Message appended"),
        )
        .await
    }

    async fn expunge(
        &mut self,
        tag: &str,
        set: Option<&[(u32, u32)]>,
        silent: bool,
    ) -> io::Result<()> {
        if self.read_only() {
            return self
                .no(tag, "[READ-ONLY] The mailbox is open read-only")
                .await;
        }
        let entries = self.entries().await?;
        let mut removed = 0usize;
        for (index, entry) in entries.iter().enumerate() {
            if !entry.deleted {
                continue;
            }
            if let Some(set) = set
                && !wire::sequence_set_matches(set, entry.uid as u32, u32::MAX)
            {
                continue;
            }
            self.purge(entry).await?;
            if !silent {
                // Sequence numbers shift as rows go, so the removals already
                // made come off the index.
                let sequence = index + 1 - removed;
                self.untagged(&format!("{sequence} EXPUNGE")).await?;
            }
            removed += 1;
        }
        self.ok(tag, "EXPUNGE completed").await
    }

    /// Removes a message row and its stored bytes.
    async fn purge(&self, entry: &FolderEntry) -> io::Result<()> {
        if let Err(err) = self.state.store.delete(&entry.storage_key).await {
            warn!(error = %err, key = %entry.storage_key, "failed to remove a stored message");
        }
        repo::delete_message(&self.state.db, entry.id)
            .await
            .map_err(io_err)?;
        Ok(())
    }

    async fn search(&mut self, tag: &str, args: &[Arg], uid_mode: bool) -> io::Result<()> {
        if self.selected.is_none() {
            return self.bad(tag, "No mailbox selected").await;
        }
        let Some(criterion) = search::parse(args) else {
            return self.bad(tag, "Unsupported search criteria").await;
        };
        let entries = self.entries().await?;
        let draft = self.selected_is_drafts();
        let mut hits: Vec<u32> = Vec::new();
        for (index, entry) in entries.iter().enumerate() {
            let sequence = (index + 1) as u32;
            let value = if uid_mode { entry.uid as u32 } else { sequence };
            match search::evaluate(&criterion, entry, None, draft) {
                Some(true) => hits.push(value),
                Some(false) => {}
                None => {
                    let raw = self
                        .state
                        .store
                        .get(&entry.storage_key)
                        .await
                        .map_err(io_err)?;
                    let parsed = mail_parser::MessageParser::default()
                        .parse(&raw)
                        .map(|message| message.into_owned());
                    if search::evaluate(&criterion, entry, parsed.as_ref(), draft) == Some(true) {
                        hits.push(value);
                    }
                }
            }
        }
        let list: Vec<String> = hits.iter().map(u32::to_string).collect();
        let label = if uid_mode { "SEARCH UID" } else { "SEARCH" };
        if list.is_empty() {
            self.untagged(label).await?;
        } else {
            self.untagged(&format!("{label} {}", list.join(" ")))
                .await?;
        }
        self.ok(tag, "SEARCH completed").await
    }

    async fn store(&mut self, tag: &str, args: &[Arg], uid_mode: bool) -> io::Result<()> {
        if self.selected.is_none() {
            return self.bad(tag, "No mailbox selected").await;
        }
        if self.read_only() {
            return self
                .no(tag, "[READ-ONLY] The mailbox is open read-only")
                .await;
        }
        let Some(set) = args
            .first()
            .and_then(|arg| wire::parse_sequence_set(arg.text()))
        else {
            return self.bad(tag, "A sequence set is required").await;
        };
        let operation = args.get(1).map(Arg::upper).unwrap_or_default();
        let silent = operation.ends_with(".SILENT");
        let mode = operation.trim_end_matches(".SILENT").to_string();
        if !matches!(mode.as_str(), "FLAGS" | "+FLAGS" | "-FLAGS") {
            return self.bad(tag, "Unsupported STORE item").await;
        }
        let wanted: Vec<String> = args
            .iter()
            .find_map(Arg::list)
            .map(|list| list.iter().map(Arg::upper).collect())
            .unwrap_or_default();
        let listed = |flag: &str| wanted.iter().any(|candidate| candidate == flag);
        let next = |flag: &str| -> Option<bool> {
            match mode.as_str() {
                "FLAGS" => Some(listed(flag)),
                "+FLAGS" => listed(flag).then_some(true),
                _ => listed(flag).then_some(false),
            }
        };

        let entries = self.entries().await?;
        let draft = self.selected_is_drafts();
        for (index, entry) in entries.iter().enumerate() {
            let value = if uid_mode {
                entry.uid as u32
            } else {
                (index + 1) as u32
            };
            if !wire::sequence_set_matches(&set, value, u32::MAX) {
                continue;
            }
            let update = repo::FlagUpdate {
                read: next("\\SEEN"),
                starred: next("\\FLAGGED"),
                answered: next("\\ANSWERED"),
                deleted: next("\\DELETED"),
            };
            if update == repo::FlagUpdate::default() {
                continue;
            }
            let flags = repo::update_flags(&self.state.db, entry.id, update)
                .await
                .map_err(io_err)?;
            if !silent {
                let rendered = wire::flag_list(&flags_for(&flags, draft));
                let uid = if uid_mode {
                    format!(" UID {}", entry.uid)
                } else {
                    String::new()
                };
                self.untagged(&format!("{} FETCH (FLAGS {rendered}{uid})", index + 1))
                    .await?;
            }
        }
        self.ok(tag, "STORE completed").await
    }

    async fn copy(&mut self, tag: &str, args: &[Arg], move_it: bool) -> io::Result<()> {
        if self.selected.is_none() {
            return self.bad(tag, "No mailbox selected").await;
        }
        if self.read_only() {
            return self
                .no(tag, "[READ-ONLY] The mailbox is open read-only")
                .await;
        }
        let Some(set) = args
            .first()
            .and_then(|arg| wire::parse_sequence_set(arg.text()))
        else {
            return self.bad(tag, "A sequence set is required").await;
        };
        let name = args.get(1).map(Arg::text).unwrap_or("").to_string();
        let Some(target) = self.resolve_mailbox(&name).await? else {
            return self.no(tag, "[TRYCREATE] No such mailbox").await;
        };
        let Some(account) = self.account.clone() else {
            return self.bad(tag, "Please authenticate first").await;
        };
        let entries = self.entries().await?;
        let uid_validity = self.uid_validity();
        let mut source_uids = Vec::new();
        let mut target_uids = Vec::new();
        let mut removed = 0usize;

        for (index, entry) in entries.iter().enumerate() {
            if !wire::sequence_set_matches(&set, entry.uid as u32, u32::MAX) {
                continue;
            }
            source_uids.push(entry.uid);
            if move_it {
                repo::set_message_folder(&self.state.db, entry.id, &target.key())
                    .await
                    .map_err(io_err)?;
                target_uids.push(entry.uid);
                self.untagged(&format!("{} EXPUNGE", index + 1 - removed))
                    .await?;
                removed += 1;
                continue;
            }
            let raw = self
                .state
                .store
                .get(&entry.storage_key)
                .await
                .map_err(io_err)?;
            let direction = if entry.direction == "outbound" {
                message::Direction::Outbound
            } else {
                message::Direction::Inbound
            };
            let id = match delivery::store_local(
                &self.state,
                &account,
                direction,
                &entry.envelope_from,
                &entry.envelope_to,
                &raw,
                &AuthOutcome::skipped(),
            )
            .await
            {
                Ok(id) => id,
                Err(err) => return self.no(tag, &format!("COPY failed: {err}")).await,
            };
            repo::update_flags(
                &self.state.db,
                id,
                repo::FlagUpdate {
                    read: Some(entry.read),
                    starred: Some(entry.starred),
                    answered: Some(entry.answered),
                    deleted: Some(false),
                },
            )
            .await
            .map_err(io_err)?;
            repo::set_message_folder(&self.state.db, id, &target.key())
                .await
                .map_err(io_err)?;
            let uid = repo::get_message(&self.state.db, id)
                .await
                .map_err(io_err)?
                .map(|row| row.uid)
                .unwrap_or_default();
            target_uids.push(uid);
        }

        let copy_uid = if source_uids.is_empty() {
            String::new()
        } else {
            format!(
                " [COPYUID {uid_validity} {} {}]",
                join_uids(&source_uids),
                join_uids(&target_uids)
            )
        };
        let label = if move_it { "MOVE" } else { "COPY" };
        let count = source_uids.len();
        self.ok(tag, &format!("{label} {count} completed{copy_uid}"))
            .await
    }

    async fn fetch(&mut self, tag: &str, args: &[Arg], uid_mode: bool) -> io::Result<()> {
        if self.selected.is_none() {
            return self.bad(tag, "No mailbox selected").await;
        }
        let Some(set) = args
            .first()
            .and_then(|arg| wire::parse_sequence_set(arg.text()))
        else {
            return self.bad(tag, "A sequence set is required").await;
        };
        let items: Vec<Item> = args
            .iter()
            .skip(1)
            .flat_map(|arg| match arg {
                Arg::List(list) => list.clone(),
                other => vec![other.clone()],
            })
            .filter_map(|arg| parse_item(&arg))
            .collect();
        if items.is_empty() {
            return self.bad(tag, "No FETCH items").await;
        }

        let entries = self.entries().await?;
        let draft = self.selected_is_drafts();
        for (index, entry) in entries.iter().enumerate() {
            let value = if uid_mode {
                entry.uid as u32
            } else {
                (index + 1) as u32
            };
            if !wire::sequence_set_matches(&set, value, u32::MAX) {
                continue;
            }
            if let Some(line) = self
                .fetch_one(entry, (index + 1) as u32, &items, draft)
                .await?
            {
                self.send(&line).await?;
            }
        }
        self.ok(tag, "FETCH completed").await
    }

    async fn fetch_one(
        &self,
        entry: &FolderEntry,
        sequence: u32,
        items: &[Item],
        draft: bool,
    ) -> io::Result<Option<Vec<u8>>> {
        let raw = if items.iter().any(Item::needs_message) {
            Some(
                self.state
                    .store
                    .get(&entry.storage_key)
                    .await
                    .map_err(io_err)?,
            )
        } else {
            None
        };
        let parsed = raw.as_ref().and_then(|raw| {
            mail_parser::MessageParser::default()
                .parse(raw)
                .map(|message| message.into_owned())
        });

        let mut out = format!("* {sequence} FETCH (").into_bytes();
        for (position, item) in items.iter().enumerate() {
            if position > 0 {
                out.push(b' ');
            }
            match item {
                Item::Flags => out.extend_from_slice(
                    format!("FLAGS {}", wire::flag_list(&flags_for_entry(entry, draft))).as_bytes(),
                ),
                Item::Uid => out.extend_from_slice(format!("UID {}", entry.uid).as_bytes()),
                Item::InternalDate => out.extend_from_slice(
                    format!(
                        "INTERNALDATE {}",
                        wire::quote(&internal_date(entry.received_at))
                    )
                    .as_bytes(),
                ),
                Item::Size => {
                    out.extend_from_slice(format!("RFC822.SIZE {}", entry.size_bytes).as_bytes())
                }
                Item::Envelope => {
                    let value = parsed
                        .as_ref()
                        .map(fetch::envelope)
                        .unwrap_or_else(|| "NIL".to_string());
                    out.extend_from_slice(format!("ENVELOPE {value}").as_bytes());
                }
                Item::Structure => {
                    let value = parsed
                        .as_ref()
                        .map(|parsed| {
                            fetch::body_structure(parsed, raw.as_deref().unwrap_or_default())
                        })
                        .unwrap_or_else(|| "NIL".to_string());
                    out.extend_from_slice(format!("BODYSTRUCTURE {value}").as_bytes());
                }
                Item::Body {
                    section,
                    label,
                    partial,
                } => {
                    let bytes = body_bytes(parsed.as_ref(), raw.as_deref(), section.as_deref());
                    write_body(&mut out, label, &bytes, *partial);
                }
                Item::Rfc822 { partial } => {
                    let bytes = raw.clone().unwrap_or_default();
                    write_body(&mut out, "RFC822", &bytes, *partial);
                }
                Item::Rfc822Header => {
                    let bytes = body_bytes(parsed.as_ref(), raw.as_deref(), Some("HEADER"));
                    write_body(&mut out, "RFC822.HEADER", &bytes, None);
                }
                Item::Rfc822Text => {
                    let bytes = body_bytes(parsed.as_ref(), raw.as_deref(), Some("TEXT"));
                    write_body(&mut out, "RFC822.TEXT", &bytes, None);
                }
            }
        }
        out.extend_from_slice(b")\r\n");
        Ok(Some(out))
    }

    async fn uid_command(&mut self, tag: &str, args: &[Arg]) -> io::Result<()> {
        let Some(subcommand) = args.first().map(Arg::upper) else {
            return self.bad(tag, "UID needs a subcommand").await;
        };
        let rest = &args[1..];
        match subcommand.as_str() {
            "FETCH" => self.fetch(tag, rest, true).await,
            "STORE" => self.store(tag, rest, true).await,
            "SEARCH" => self.search(tag, rest, true).await,
            "COPY" => self.copy(tag, rest, false).await,
            "MOVE" => self.copy(tag, rest, true).await,
            "EXPUNGE" => {
                let set = rest
                    .first()
                    .and_then(|arg| wire::parse_sequence_set(arg.text()));
                self.expunge(tag, set.as_deref(), false).await
            }
            _ => self.bad(tag, "Unknown UID subcommand").await,
        }
    }

    // -- idle --------------------------------------------------------------

    async fn idle(&mut self, tag: &str) -> io::Result<()> {
        if self.account.is_none() {
            return self.bad(tag, "Please authenticate first").await;
        }
        self.line("+ idling\r\n").await?;
        let mut events = self.state.events.subscribe();
        let mailbox_id = self.mailbox_id();

        loop {
            let mut done = false;
            let mut refresh = false;
            let mut keepalive = false;
            {
                let conn = self.conn.as_mut().expect("connection is present");
                tokio::select! {
                    line = conn.read_line(64 * 1024) => {
                        match line? {
                            None => return Ok(()),
                            Some(line) if line.eq_ignore_ascii_case(b"DONE") => done = true,
                            Some(_) => {}
                        }
                    }
                    event = events.recv() => {
                        if let Ok(frame) = event
                            && frame_mentions(&frame, mailbox_id)
                        {
                            refresh = true;
                        }
                    }
                    _ = tokio::time::sleep(IDLE_KEEPALIVE) => {
                        keepalive = true;
                    }
                }
            }
            if keepalive {
                self.untagged("OK Still here").await?;
                continue;
            }
            if refresh {
                let entries = self.entries().await?;
                self.untagged(&format!("{} EXISTS", entries.len())).await?;
            }
            if done {
                break;
            }
        }
        self.ok(tag, "IDLE terminated").await
    }
}
// -- helpers ---------------------------------------------------------------

fn capabilities() -> String {
    "IMAP4rev1 IDLE NAMESPACE UIDPLUS MOVE UNSELECT SPECIAL-USE AUTH=PLAIN AUTH=LOGIN SASL-IR ID LITERAL+"
        .to_string()
}

/// The flags a message carries, as a client sees them.
fn flags_for_entry(entry: &FolderEntry, draft: bool) -> Vec<String> {
    let mut flags = Vec::new();
    if entry.read {
        flags.push("\\Seen".to_string());
    }
    if entry.answered {
        flags.push("\\Answered".to_string());
    }
    if entry.starred {
        flags.push("\\Flagged".to_string());
    }
    if entry.deleted {
        flags.push("\\Deleted".to_string());
    }
    if draft {
        flags.push("\\Draft".to_string());
    }
    flags
}

fn flags_for(flag: &message_flag::Model, draft: bool) -> Vec<String> {
    let mut flags = Vec::new();
    if flag.read {
        flags.push("\\Seen".to_string());
    }
    if flag.answered {
        flags.push("\\Answered".to_string());
    }
    if flag.starred {
        flags.push("\\Flagged".to_string());
    }
    if flag.deleted {
        flags.push("\\Deleted".to_string());
    }
    if draft {
        flags.push("\\Draft".to_string());
    }
    flags
}

/// The date an IMAP client reads back as INTERNALDATE.
fn internal_date(value: DateTime<Utc>) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let month = MONTHS
        .get((value.month() as usize).saturating_sub(1))
        .copied()
        .unwrap_or("Jan");
    format!(
        "{:02}-{month}-{} {:02}:{:02}:{:02} +0000",
        value.day(),
        value.year(),
        value.hour(),
        value.minute(),
        value.second(),
    )
}

fn join_uids(uids: &[i64]) -> String {
    uids.iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

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

/// A LIST pattern, with a star matching anything and a percent anything but a
/// hierarchy delimiter.
fn pattern_matches(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    matches_from(&pattern, &name)
}

fn matches_from(pattern: &[char], name: &[char]) -> bool {
    let Some((head, rest)) = pattern.split_first() else {
        return name.is_empty();
    };
    match head {
        '*' => (0..=name.len()).any(|skip| matches_from(rest, &name[skip..])),
        '%' => (0..=name.iter().position(|ch| *ch == '/').unwrap_or(name.len()))
            .any(|skip| matches_from(rest, &name[skip..])),
        _ => match name.split_first() {
            Some((first, tail)) if first.eq_ignore_ascii_case(head) => matches_from(rest, tail),
            _ => false,
        },
    }
}

/// The bytes of one body section, empty when the message cannot be parsed.
fn body_bytes(
    parsed: Option<&mail_parser::Message<'_>>,
    raw: Option<&[u8]>,
    section: Option<&str>,
) -> Vec<u8> {
    let Some(raw) = raw else {
        return Vec::new();
    };
    match (parsed, section) {
        (_, None) => raw.to_vec(),
        (Some(parsed), Some(spec)) => fetch::section(parsed, raw, spec).unwrap_or_default(),
        (None, Some(_)) => Vec::new(),
    }
}

/// Writes one body item: the label, a literal and the bytes.
fn write_body(out: &mut Vec<u8>, label: &str, bytes: &[u8], partial: Option<(usize, usize)>) {
    let slice = match partial {
        Some((start, count)) => {
            let start = start.min(bytes.len());
            let end = start.saturating_add(count).min(bytes.len());
            &bytes[start..end]
        }
        None => bytes,
    };
    out.extend_from_slice(format!("{label} {{{}}}\r\n", slice.len()).as_bytes());
    out.extend_from_slice(slice);
}
/// One FETCH item.
#[derive(Debug, Clone, PartialEq)]
enum Item {
    Flags,
    Uid,
    InternalDate,
    Size,
    Envelope,
    Structure,
    Body {
        section: Option<String>,
        label: String,
        partial: Option<(usize, usize)>,
    },
    Rfc822 {
        partial: Option<(usize, usize)>,
    },
    Rfc822Header,
    Rfc822Text,
}

impl Item {
    /// True when answering the item needs the stored bytes.
    fn needs_message(&self) -> bool {
        !matches!(
            self,
            Item::Flags | Item::Uid | Item::InternalDate | Item::Size
        )
    }
}

fn parse_item(arg: &Arg) -> Option<Item> {
    let text = arg.text().trim();
    let upper = text.to_ascii_uppercase();
    Some(match upper.as_str() {
        "FLAGS" => Item::Flags,
        "UID" => Item::Uid,
        "INTERNALDATE" => Item::InternalDate,
        "RFC822.SIZE" => Item::Size,
        "ENVELOPE" => Item::Envelope,
        "BODYSTRUCTURE" | "BODY" => Item::Structure,
        "RFC822" => Item::Rfc822 { partial: None },
        "RFC822.HEADER" => Item::Rfc822Header,
        "RFC822.TEXT" => Item::Rfc822Text,
        _ => {
            let (head, partial) = split_partial(text);
            let section = head
                .strip_prefix("BODY.PEEK[")
                .or_else(|| head.strip_prefix("BODY["))?
                .strip_suffix(']')?
                .to_string();
            Item::Body {
                label: format!("BODY[{section}]"),
                section: Some(section),
                partial,
            }
        }
    })
}

/// Splits a partial marker like <0.1024> off a fetch item.
fn split_partial(text: &str) -> (&str, Option<(usize, usize)>) {
    let Some(open) = text.rfind('<') else {
        return (text, None);
    };
    let Some(close) = text.rfind('>') else {
        return (text, None);
    };
    if close < open {
        return (text, None);
    }
    let inner = &text[open + 1..close];
    let partial = inner
        .split_once('.')
        .and_then(|(start, count)| Some((start.trim().parse().ok()?, count.trim().parse().ok()?)));
    (&text[..open], partial)
}

/// True when a live-update frame is about this mailbox.
fn frame_mentions(frame: &str, mailbox_id: Option<Uuid>) -> bool {
    let Some(mailbox_id) = mailbox_id else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(frame) else {
        return false;
    };
    value.get("type").and_then(|value| value.as_str()) == Some("new_message")
        && value
            .get("mailboxId")
            .and_then(|value| value.as_str())
            .is_some_and(|value| value == mailbox_id.to_string())
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_the_folders_a_client_expects() {
        let names: Vec<String> = system_mailboxes()
            .into_iter()
            .map(|entry| entry.name)
            .collect();
        assert_eq!(
            names,
            vec!["INBOX", "Sent", "Drafts", "Trash", "Junk", "Archive"]
        );
        let inbox = &system_mailboxes()[0];
        assert_eq!(inbox.folders, vec!["inbox", "snoozed"]);
        assert_eq!(inbox.key(), "inbox");
        assert!(
            system_mailboxes()[1]
                .attributes
                .contains(&"\\Sent".to_string())
        );
    }

    #[test]
    fn matches_list_patterns() {
        assert!(pattern_matches("*", "Sent"));
        assert!(pattern_matches("Sent", "Sent"));
        assert!(pattern_matches("S*", "Sent"));
        assert!(pattern_matches("%", "Sent"));
        assert!(!pattern_matches("%", "Sent/Sub"));
        assert!(pattern_matches("Sent/%", "Sent/Sub"));
        assert!(!pattern_matches("Sent", "Sent/Sub"));
    }

    #[test]
    fn renders_the_flags_of_a_row() {
        let flag = message_flag::Model {
            message_id: Uuid::now_v7(),
            read: true,
            starred: true,
            answered: false,
            deleted: true,
            folder: "inbox".to_string(),
            snoozed_until: None,
            updated_at: Utc::now(),
        };
        assert_eq!(
            flags_for(&flag, false),
            vec![
                "\\Seen".to_string(),
                "\\Flagged".to_string(),
                "\\Deleted".to_string()
            ]
        );
        assert_eq!(flags_for(&flag, true).last().unwrap(), "\\Draft");
    }

    #[test]
    fn parses_fetch_items_and_partials() {
        assert_eq!(
            parse_item(&Arg::Atom("FLAGS".to_string())),
            Some(Item::Flags)
        );
        assert_eq!(
            parse_item(&Arg::Atom("BODY.PEEK[]".to_string())),
            Some(Item::Body {
                section: Some(String::new()),
                label: "BODY[]".to_string(),
                partial: None
            })
        );
        assert_eq!(
            parse_item(&Arg::Atom(
                "BODY[HEADER.FIELDS (DATE FROM)]<0.100>".to_string()
            )),
            Some(Item::Body {
                section: Some("HEADER.FIELDS (DATE FROM)".to_string()),
                label: "BODY[HEADER.FIELDS (DATE FROM)]".to_string(),
                partial: Some((0, 100))
            })
        );
        assert_eq!(parse_item(&Arg::Atom("BOGUS".to_string())), None);
    }

    #[test]
    fn formats_the_internal_date() {
        let value = DateTime::parse_from_rfc3339("2026-03-04T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(internal_date(value), "04-Mar-2026 10:00:00 +0000");
    }

    #[test]
    fn reads_a_live_update_frame() {
        let id = Uuid::now_v7();
        let frame =
            format!("{{\"type\":\"new_message\",\"mailboxId\":\"{id}\",\"subject\":\"hi\"}}");
        assert!(frame_mentions(&frame, Some(id)));
        assert!(!frame_mentions(&frame, Some(Uuid::now_v7())));
        assert!(!frame_mentions("not json", Some(id)));
    }

    #[test]
    fn reads_the_credentials_a_sasl_plain_payload_carries() {
        // AUTHENTICATE PLAIN AGlsYXBjaGVja0BleGFtcGxlLnRlc3QAc2VjcmV0MTIz
        let (user, password) = split_plain(&decode_base64(
            "AGltYXBjaGVja0BleGFtcGxlLnRlc3QAc2VjcmV0MTIz",
        ));
        assert_eq!(user, "imapcheck@example.test");
        assert_eq!(password, "secret123");
    }
}
