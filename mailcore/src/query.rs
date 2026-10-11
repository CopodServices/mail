//! The SQL the message list, the folder counts and the reading order share.
//!
//! A message's folder is its reader flag when it has one and its direction and
//! delivery status otherwise, so the list, the counts and the navigation all
//! classify rows in the database. That lets them agree with each other and page
//! with an index. [`FOLDER_SQL`] mirrors `folder_of` in the API one for one; the
//! integration tests in `server/src/integration_tests.rs` pin the pair against
//! real rows.

use anyhow::{Result, anyhow};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use entity::{message, outbound_job};
use sea_orm::sea_query::Value;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseBackend, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder, QuerySelect, Statement,
};
use std::collections::{BTreeMap, HashMap};
use uuid::Uuid;

/// The folder a message shows in, derived from its reader flag when there is
/// one and from its direction and delivery status otherwise.
pub const FOLDER_SQL: &str = "CASE \
     WHEN f.snoozed_until IS NOT NULL AND f.snoozed_until > now() THEN 'snoozed' \
     WHEN f.folder IS NOT NULL AND f.folder <> 'inbox' THEN f.folder \
     WHEN m.direction = 'outbound' THEN 'sent' \
     WHEN m.status = 'quarantined' THEN 'spam' \
     ELSE 'inbox' END";

/// A message is unread only when it arrived and its flag does not say read. A
/// copy in the sender's own Sent folder is never unread.
pub const UNREAD_SQL: &str = "(m.direction = 'inbound' AND COALESCE(f.read, false) = false)";

/// Every message query starts from the same left join, so a message without a
/// flag row still appears.
const MESSAGE_FROM: &str = "FROM messages m LEFT JOIN message_flags f ON f.message_id = m.id";

/// How many ids the reading order hands back at most.
pub const NAVIGATION_CAP: u64 = 5000;

/// The filters a list, a count or a reading order may apply.
#[derive(Debug, Default, Clone)]
pub struct MessageFilters {
    pub mailbox_id: Option<Uuid>,
    /// A system folder, or a custom folder id.
    pub folder: Option<String>,
    pub direction: Option<String>,
    pub q: Option<String>,
    /// "read" or "unread".
    pub read: Option<String>,
    /// Restrict to starred messages, the view the starred folder shows.
    pub starred: bool,
}

/// A WHERE clause and the values bound into it.
struct Clause {
    sql: String,
    values: Vec<Value>,
}

impl Clause {
    fn new() -> Self {
        Self {
            sql: String::new(),
            values: Vec::new(),
        }
    }

    /// Binds a value and returns its placeholder number.
    fn bind(&mut self, value: Value) -> usize {
        self.values.push(value);
        self.values.len()
    }

    fn and(&mut self, condition: String) {
        if self.sql.is_empty() {
            self.sql = format!("WHERE {condition}");
        } else {
            self.sql.push_str(" AND ");
            self.sql.push_str(&condition);
        }
    }
}

impl MessageFilters {
    fn clause(&self) -> Clause {
        let mut clause = Clause::new();
        if let Some(mailbox_id) = self.mailbox_id {
            let index = clause.bind(mailbox_id.into());
            clause.and(format!("m.mailbox_id = ${index}"));
        }
        if let Some(direction @ ("inbound" | "outbound")) = self.direction.as_deref() {
            let index = clause.bind(direction.to_string().into());
            clause.and(format!("m.direction = ${index}"));
        }
        if let Some(text) = self
            .q
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            let index = clause.bind(format!("%{text}%").into());
            clause.and(format!(
                "(m.subject ILIKE ${index} OR m.envelope_from ILIKE ${index} OR m.envelope_to ILIKE ${index})"
            ));
        }
        // Starred is a flag view, not a folder a message is moved into.
        if self.starred || self.folder.as_deref() == Some("starred") {
            clause.and("COALESCE(f.starred, false) = true".to_string());
        } else if let Some(folder) = self
            .folder
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            let index = clause.bind(folder.to_string().into());
            clause.and(format!("({FOLDER_SQL}) = ${index}"));
        }
        match self.read.as_deref() {
            // Read and unread split what the client can mark; the copy the
            // sender owns is read by definition rather than by a flag.
            Some("read") => clause
                .and("(m.direction = 'outbound' OR COALESCE(f.read, false) = true)".to_string()),
            Some("unread") => clause.and(UNREAD_SQL.to_string()),
            _ => {}
        }
        clause
    }
}

/// The last row of a page, which the next page resumes after.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessageCursor {
    pub received_at: DateTime<Utc>,
    pub id: Uuid,
}

impl MessageCursor {
    /// An opaque, URL-safe cursor. Clients treat it as a token.
    pub fn encode(&self) -> String {
        URL_SAFE_NO_PAD.encode(format!(
            "{}.{}",
            self.received_at.timestamp_micros(),
            self.id
        ))
    }

    pub fn decode(value: &str) -> Option<Self> {
        let text = String::from_utf8(URL_SAFE_NO_PAD.decode(value.trim()).ok()?).ok()?;
        let (micros, id) = text.split_once('.')?;
        Some(Self {
            received_at: DateTime::from_timestamp_micros(micros.parse().ok()?)?,
            id: Uuid::parse_str(id).ok()?,
        })
    }
}

/// One page of messages, newest first.
#[derive(Debug)]
pub struct MessagePage {
    pub ids: Vec<Uuid>,
    /// The number of messages matching the filters, not the page size.
    pub total: i64,
    pub next: Option<MessageCursor>,
}

/// One keyset page of ids, newest first, without the filtered total. The count
/// is a second aggregate over the whole mailbox, so a caller that already holds
/// the totals reads the page with this and skips it.
#[derive(Debug)]
pub struct MessagePageIds {
    pub ids: Vec<Uuid>,
    pub next: Option<MessageCursor>,
}

/// Reads one keyset page of ids, without counting the rows the filters match.
pub async fn message_page_ids(
    db: &DatabaseConnection,
    filters: &MessageFilters,
    cursor: Option<MessageCursor>,
    limit: u64,
) -> Result<MessagePageIds> {
    let mut clause = filters.clause();
    if let Some(cursor) = cursor {
        let received_at = clause.bind(cursor.received_at.into());
        let id = clause.bind(cursor.id.into());
        clause.and(format!(
            "(m.received_at < ${received_at} OR (m.received_at = ${received_at} AND m.id < ${id}))"
        ));
    }
    // One row past the page says whether a next page exists.
    let limit_index = clause.bind(((limit + 1) as i64).into());
    let sql = format!(
        "SELECT m.id AS id, m.received_at AS received_at {MESSAGE_FROM} {} \
         ORDER BY m.received_at DESC, m.id DESC LIMIT ${limit_index}",
        clause.sql
    );
    let rows = db
        .query_all_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &sql,
            clause.values.clone(),
        ))
        .await?;

    let mut page: Vec<(Uuid, DateTime<Utc>)> = Vec::with_capacity(rows.len());
    for row in &rows {
        page.push((row.try_get("", "id")?, row.try_get("", "received_at")?));
    }
    let has_more = page.len() as u64 > limit;
    page.truncate(limit as usize);
    let next = has_more.then(|| {
        let (id, received_at) = *page.last().expect("a page with a next page has rows");
        MessageCursor { received_at, id }
    });

    Ok(MessagePageIds {
        ids: page.into_iter().map(|(id, _)| id).collect(),
        next,
    })
}

/// Reads one keyset page, plus the total the filters match.
pub async fn message_page(
    db: &DatabaseConnection,
    filters: &MessageFilters,
    cursor: Option<MessageCursor>,
    limit: u64,
) -> Result<MessagePage> {
    let page = message_page_ids(db, filters, cursor, limit).await?;
    let total = count_matching(db, filters).await?;
    Ok(MessagePage {
        ids: page.ids,
        total,
        next: page.next,
    })
}

/// The number of messages the filters match.
pub async fn count_matching(db: &DatabaseConnection, filters: &MessageFilters) -> Result<i64> {
    let clause = filters.clause();
    let sql = format!("SELECT count(*) AS count {MESSAGE_FROM} {}", clause.sql);
    let row = db
        .query_one_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &sql,
            clause.values,
        ))
        .await?
        .ok_or_else(|| anyhow!("the count query returned no row"))?;
    Ok(row.try_get("", "count")?)
}

/// Per-folder, starred and per-mailbox totals and unread counts.
#[derive(Debug, Default)]
pub struct FolderCounts {
    /// Keyed by effective folder, system folders and custom folder ids alike.
    pub folders: BTreeMap<String, (i64, i64)>,
    pub starred: (i64, i64),
    pub mailboxes: BTreeMap<Uuid, (i64, i64)>,
}

/// Counts every folder a mailbox holds with one aggregate per view, instead of
/// walking the rows in Rust.
pub async fn folder_counts(
    db: &DatabaseConnection,
    mailbox_id: Option<Uuid>,
) -> Result<FolderCounts> {
    let scope = MessageFilters {
        mailbox_id,
        ..MessageFilters::default()
    };

    let clause = scope.clause();
    let sql = format!(
        "SELECT ({FOLDER_SQL}) AS folder, count(*) AS total, \
         COALESCE(sum(CASE WHEN {UNREAD_SQL} THEN 1 ELSE 0 END), 0) AS unread \
         {MESSAGE_FROM} {} GROUP BY 1",
        clause.sql
    );
    let rows = db
        .query_all_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &sql,
            clause.values,
        ))
        .await?;
    let mut folders = BTreeMap::new();
    for row in &rows {
        let folder: String = row.try_get("", "folder")?;
        folders.insert(
            folder,
            (row.try_get("", "total")?, row.try_get("", "unread")?),
        );
    }

    let mut starred_clause = scope.clause();
    starred_clause.and("COALESCE(f.starred, false) = true".to_string());
    let sql = format!(
        "SELECT count(*) AS total, \
         COALESCE(sum(CASE WHEN {UNREAD_SQL} THEN 1 ELSE 0 END), 0) AS unread \
         {MESSAGE_FROM} {}",
        starred_clause.sql
    );
    let row = db
        .query_one_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &sql,
            starred_clause.values,
        ))
        .await?
        .ok_or_else(|| anyhow!("the starred count returned no row"))?;
    let starred = (row.try_get("", "total")?, row.try_get("", "unread")?);

    let mut mailbox_clause = scope.clause();
    mailbox_clause.and("m.mailbox_id IS NOT NULL".to_string());
    let sql = format!(
        "SELECT m.mailbox_id AS mailbox_id, count(*) AS total, \
         COALESCE(sum(CASE WHEN {UNREAD_SQL} THEN 1 ELSE 0 END), 0) AS unread \
         {MESSAGE_FROM} {} GROUP BY 1",
        mailbox_clause.sql
    );
    let rows = db
        .query_all_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &sql,
            mailbox_clause.values,
        ))
        .await?;
    let mut mailboxes = BTreeMap::new();
    for row in &rows {
        let id: Uuid = row.try_get("", "mailbox_id")?;
        mailboxes.insert(id, (row.try_get("", "total")?, row.try_get("", "unread")?));
    }

    Ok(FolderCounts {
        folders,
        starred,
        mailboxes,
    })
}

/// The ids the reading order walks, with whether each one is unread.
pub async fn navigation(
    db: &DatabaseConnection,
    filters: &MessageFilters,
    cap: u64,
) -> Result<Vec<(Uuid, bool)>> {
    let mut clause = filters.clause();
    let limit_index = clause.bind((cap as i64).into());
    let sql = format!(
        "SELECT m.id AS id, {UNREAD_SQL} AS unread {MESSAGE_FROM} {} \
         ORDER BY m.received_at DESC, m.id DESC LIMIT ${limit_index}",
        clause.sql
    );
    let rows = db
        .query_all_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &sql,
            clause.values,
        ))
        .await?;
    let mut entries = Vec::with_capacity(rows.len());
    for row in &rows {
        entries.push((row.try_get("", "id")?, row.try_get("", "unread")?));
    }
    Ok(entries)
}

/// The ids matching the filters, newest first, capped. Used where a caller only
/// needs to act on every message of a folder.
pub async fn message_ids(
    db: &DatabaseConnection,
    filters: &MessageFilters,
    cap: u64,
) -> Result<Vec<Uuid>> {
    Ok(navigation(db, filters, cap)
        .await?
        .into_iter()
        .map(|(id, _)| id)
        .collect())
}

/// The neighbours of one message in a folder's reading order, with the open
/// message's position in that order. The order matches the web client's: the
/// unread messages newest first, then the read ones newest first.
#[derive(Debug, Default)]
pub struct Neighbors {
    /// The message just newer than the open one in the reading order.
    pub previous_id: Option<Uuid>,
    /// The message just older than the open one in the reading order.
    pub next_id: Option<Uuid>,
    /// One-based position of the open message in the reading order.
    pub position: i64,
    /// Every message the filters match.
    pub total: i64,
    /// One-based position among the unread messages; zero when the open message
    /// is read.
    pub unread_position: i64,
    /// Every unread message the filters match.
    pub unread_total: i64,
}

/// Which side of the open message a lookup walks.
#[derive(Clone, Copy)]
enum Side {
    /// A newer message (a larger `(received_at, id)` key).
    Newer,
    /// An older message (a smaller `(received_at, id)` key).
    Older,
}

/// The filter plus, optionally, one side of the open message and the unread
/// state a lookup is restricted to.
fn scoped_clause(
    filters: &MessageFilters,
    unread: Option<bool>,
    side: Option<(Side, MessageCursor)>,
) -> Clause {
    let mut clause = filters.clause();
    if let Some((side, cursor)) = side {
        let received_at = clause.bind(cursor.received_at.into());
        let id = clause.bind(cursor.id.into());
        clause.and(match side {
            Side::Newer => format!(
                "(m.received_at > ${received_at} OR (m.received_at = ${received_at} AND m.id > ${id}))"
            ),
            Side::Older => format!(
                "(m.received_at < ${received_at} OR (m.received_at = ${received_at} AND m.id < ${id}))"
            ),
        });
    }
    match unread {
        Some(true) => clause.and(UNREAD_SQL.to_string()),
        Some(false) => clause.and(format!("NOT {UNREAD_SQL}")),
        None => {}
    }
    clause
}

/// How many rows the filters match, restricted to the unread (or read) set and
/// to one side of the open message.
async fn count_scoped(
    db: &DatabaseConnection,
    filters: &MessageFilters,
    unread: Option<bool>,
    side: Option<(Side, MessageCursor)>,
) -> Result<i64> {
    let clause = scoped_clause(filters, unread, side);
    let sql = format!("SELECT count(*) AS count {MESSAGE_FROM} {}", clause.sql);
    let row = db
        .query_one_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &sql,
            clause.values,
        ))
        .await?
        .ok_or_else(|| anyhow!("the count query returned no row"))?;
    Ok(row.try_get("", "count")?)
}

/// The nearest id on one side of the open message, within the unread (or read)
/// set.
async fn neighbor_scoped(
    db: &DatabaseConnection,
    filters: &MessageFilters,
    unread: bool,
    side: Side,
    cursor: MessageCursor,
) -> Result<Option<Uuid>> {
    let clause = scoped_clause(filters, Some(unread), Some((side, cursor)));
    let order = match side {
        Side::Newer => "ASC",
        Side::Older => "DESC",
    };
    let sql = format!(
        "SELECT m.id AS id {MESSAGE_FROM} {} ORDER BY m.received_at {order}, m.id {order} LIMIT 1",
        clause.sql
    );
    let row = db
        .query_one_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &sql,
            clause.values,
        ))
        .await?;
    match row {
        Some(row) => Ok(Some(row.try_get("", "id")?)),
        None => Ok(None),
    }
}

/// The newest (or oldest) id of the unread (or read) set.
async fn edge_scoped(
    db: &DatabaseConnection,
    filters: &MessageFilters,
    unread: bool,
    newest: bool,
) -> Result<Option<Uuid>> {
    let clause = scoped_clause(filters, Some(unread), None);
    let order = if newest { "DESC" } else { "ASC" };
    let sql = format!(
        "SELECT m.id AS id {MESSAGE_FROM} {} ORDER BY m.received_at {order}, m.id {order} LIMIT 1",
        clause.sql
    );
    let row = db
        .query_one_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &sql,
            clause.values,
        ))
        .await?;
    match row {
        Some(row) => Ok(Some(row.try_get("", "id")?)),
        None => Ok(None),
    }
}

/// The neighbours and position of one message, walked with a handful of index
/// lookups instead of returning the whole folder.
///
/// `target_unread` is the caller's view of the open message, so the report stays
/// stable while opening the message marks it read.
pub async fn neighbors(
    db: &DatabaseConnection,
    filters: &MessageFilters,
    target: MessageCursor,
    target_unread: bool,
) -> Result<Neighbors> {
    let total = count_matching(db, filters).await?;
    let unread_total = count_scoped(db, filters, Some(true), None).await?;
    let position = if target_unread {
        count_scoped(db, filters, Some(true), Some((Side::Newer, target))).await? + 1
    } else {
        let read_newer =
            count_scoped(db, filters, Some(false), Some((Side::Newer, target))).await?;
        unread_total + read_newer + 1
    };
    // The reading order is the unread messages then the read ones, so the unread
    // set sits before the read set: an unread message's next message is the
    // newest read one, and a read message's previous is the oldest unread one.
    let (previous_id, next_id) = if target_unread {
        let previous = neighbor_scoped(db, filters, true, Side::Newer, target).await?;
        let next = match neighbor_scoped(db, filters, true, Side::Older, target).await? {
            Some(id) => Some(id),
            None => edge_scoped(db, filters, false, true).await?,
        };
        (previous, next)
    } else {
        let previous = match neighbor_scoped(db, filters, false, Side::Newer, target).await? {
            Some(id) => Some(id),
            None => edge_scoped(db, filters, true, false).await?,
        };
        let next = neighbor_scoped(db, filters, false, Side::Older, target).await?;
        (previous, next)
    };
    Ok(Neighbors {
        previous_id,
        next_id,
        position,
        total,
        unread_position: if target_unread { position } else { 0 },
        unread_total,
    })
}

/// The state of the job that carries a stored copy out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendState {
    /// queued, sending, sent or failed.
    pub state: &'static str,
    pub error: Option<String>,
}

/// When a scheduled copy is due to leave the Scheduled folder, keyed by that
/// copy's message id.
pub async fn scheduled_times(
    db: &DatabaseConnection,
    ids: &[Uuid],
) -> Result<HashMap<Uuid, DateTime<Utc>>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let jobs = outbound_job::Entity::find()
        .filter(outbound_job::Column::ScheduledMessageId.is_in(ids.to_vec()))
        .filter(outbound_job::Column::Status.eq(outbound_job::JobStatus::Pending))
        .all(db)
        .await?;
    Ok(jobs
        .into_iter()
        .filter_map(|job| job.scheduled_message_id.map(|id| (id, job.next_attempt_at)))
        .collect())
}

/// The state of the job that delivers each stored copy, keyed by copy message
/// id. A copy whose job is gone is simply absent.
pub async fn send_states(
    db: &DatabaseConnection,
    ids: &[Uuid],
) -> Result<HashMap<Uuid, SendState>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let jobs = outbound_job::Entity::find()
        .filter(outbound_job::Column::CopyMessageId.is_in(ids.to_vec()))
        .order_by_asc(outbound_job::Column::CreatedAt)
        .all(db)
        .await?;
    let mut states = HashMap::new();
    for job in jobs {
        let Some(copy_message_id) = job.copy_message_id else {
            continue;
        };
        // One copy can have several jobs behind it (a blind recipient travels on
        // its own), so the copy reports the worst of them.
        let state = job_state(&job.status);
        let worse = states
            .get(&copy_message_id)
            .is_none_or(|current: &SendState| severity(state) > severity(current.state));
        if worse {
            states.insert(
                copy_message_id,
                SendState {
                    state,
                    error: job.last_error,
                },
            );
        }
    }
    Ok(states)
}

/// One stored message as an IMAP or POP3 reader sees it.
///
/// The readers number a mailbox's messages by uid, so this carries the reader
/// state plus the metadata a FETCH, STAT or LIST answers from, never the bytes.
#[derive(Debug, Clone, PartialEq)]
pub struct FolderEntry {
    pub id: Uuid,
    pub uid: i64,
    pub read: bool,
    pub starred: bool,
    pub answered: bool,
    pub deleted: bool,
    pub size_bytes: i64,
    pub received_at: DateTime<Utc>,
    pub subject: Option<String>,
    pub header_from: Option<String>,
    pub message_id: Option<String>,
    pub envelope_from: String,
    pub envelope_to: String,
    pub in_reply_to: Option<String>,
    pub storage_key: String,
    pub direction: String,
    pub status: String,
}

/// The messages in one mailbox's folders, oldest uid first.
///
/// This is the reader's view of a folder: the readers resolve sequence numbers,
/// answer STATUS from it and run SEARCH over it, so it stays one query per
/// command and never touches the stored bytes.
pub async fn folder_entries(
    db: &DatabaseConnection,
    mailbox_id: Uuid,
    folders: &[&str],
) -> Result<Vec<FolderEntry>> {
    if folders.is_empty() {
        return Ok(Vec::new());
    }
    let mut sql = String::from(
        "SELECT m.id AS id, m.uid AS uid, \
         COALESCE(f.read, false) AS read, COALESCE(f.starred, false) AS starred, \
         COALESCE(f.answered, false) AS answered, COALESCE(f.deleted, false) AS deleted, \
         m.size_bytes AS size_bytes, m.received_at AS received_at, \
         m.subject AS subject, m.header_from AS header_from, m.message_id AS message_id, \
         m.envelope_from AS envelope_from, m.envelope_to AS envelope_to, \
         m.in_reply_to AS in_reply_to, m.storage_key AS storage_key, \
         m.direction AS direction, m.status AS status ",
    );
    sql.push_str(MESSAGE_FROM);
    let mut values: Vec<Value> = vec![mailbox_id.into()];
    sql.push_str(" WHERE m.mailbox_id = $1 AND (");
    sql.push_str(FOLDER_SQL);
    sql.push_str(") IN (");
    for (index, folder) in folders.iter().enumerate() {
        if index > 0 {
            sql.push(',');
        }
        values.push(folder.to_string().into());
        sql.push('$');
        sql.push_str(&(index + 2).to_string());
    }
    sql.push_str(") ORDER BY m.uid ASC");

    let rows = db
        .query_all_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            &sql,
            values,
        ))
        .await?;
    rows.iter()
        .map(|row| {
            Ok(FolderEntry {
                id: row.try_get("", "id")?,
                uid: row.try_get("", "uid")?,
                read: row.try_get("", "read")?,
                starred: row.try_get("", "starred")?,
                answered: row.try_get("", "answered")?,
                deleted: row.try_get("", "deleted")?,
                size_bytes: row.try_get("", "size_bytes")?,
                received_at: row.try_get("", "received_at")?,
                subject: row.try_get("", "subject")?,
                header_from: row.try_get("", "header_from")?,
                message_id: row.try_get("", "message_id")?,
                envelope_from: row.try_get("", "envelope_from")?,
                envelope_to: row.try_get("", "envelope_to")?,
                in_reply_to: row.try_get("", "in_reply_to")?,
                storage_key: row.try_get("", "storage_key")?,
                direction: row.try_get("", "direction")?,
                status: row.try_get("", "status")?,
            })
        })
        .collect()
}

/// The largest uid handed out so far, which is one below the next UIDNEXT.
pub async fn max_uid(db: &DatabaseConnection, mailbox_id: Uuid) -> Result<i64> {
    #[derive(sea_orm::FromQueryResult)]
    struct MaxUid {
        max_uid: Option<i64>,
    }
    let row: Option<MaxUid> = message::Entity::find()
        .filter(message::Column::MailboxId.eq(mailbox_id))
        .select_only()
        .column_as(message::Column::Uid.max(), "max_uid")
        .into_model::<MaxUid>()
        .one(db)
        .await?;
    Ok(row.and_then(|row| row.max_uid).unwrap_or(0))
}

/// How bad a delivery state is, so a copy with several jobs reports the worst.
fn severity(state: &str) -> u8 {
    match state {
        "failed" => 3,
        "queued" | "sending" => 2,
        "sent" => 1,
        _ => 0,
    }
}

fn job_state(status: &outbound_job::JobStatus) -> &'static str {
    match status {
        outbound_job::JobStatus::Pending => "queued",
        outbound_job::JobStatus::Sending => "sending",
        outbound_job::JobStatus::Sent => "sent",
        outbound_job::JobStatus::Failed => "failed",
    }
}

#[cfg(test)]
mod tests {
    use super::{FOLDER_SQL, MessageCursor, MessageFilters, UNREAD_SQL};
    use chrono::DateTime;
    use uuid::Uuid;

    #[test]
    fn a_cursor_round_trips_through_its_opaque_form() {
        let cursor = MessageCursor {
            received_at: DateTime::from_timestamp_micros(1_761_234_567_890_123).unwrap(),
            id: Uuid::now_v7(),
        };
        let encoded = cursor.encode();
        assert!(
            encoded
                .chars()
                .all(|char| char.is_ascii_alphanumeric() || char == '-' || char == '_'),
            "cursors stay URL-safe: {encoded}"
        );
        assert_eq!(MessageCursor::decode(&encoded), Some(cursor));
    }

    #[test]
    fn a_cursor_keeps_microsecond_precision() {
        let cursor = MessageCursor {
            received_at: DateTime::from_timestamp_micros(1_761_234_567_890_123).unwrap(),
            id: Uuid::nil(),
        };
        let decoded = MessageCursor::decode(&cursor.encode()).unwrap();
        assert_eq!(decoded.received_at, cursor.received_at);
        assert_eq!(
            decoded.received_at.timestamp_micros(),
            1_761_234_567_890_123
        );
    }

    #[test]
    fn a_cursor_rejects_anything_it_did_not_write() {
        for value in ["", "not-a-cursor", "123.", "abc.def", "123.not-a-uuid"] {
            assert_eq!(MessageCursor::decode(value), None, "value: {value}");
        }
    }

    #[test]
    fn the_shared_expressions_name_every_folder() {
        // The SQL has to keep in step with folder_of in the API: snoozed wins,
        // then a flag folder, then direction and delivery status.
        for folder in ["snoozed", "sent", "spam", "inbox"] {
            assert!(
                FOLDER_SQL.contains(folder),
                "{folder} missing from {FOLDER_SQL}"
            );
        }
        assert!(FOLDER_SQL.find("snoozed").unwrap() < FOLDER_SQL.find("outbound").unwrap());
        assert!(UNREAD_SQL.contains("m.direction = 'inbound'"));
        assert!(!UNREAD_SQL.contains("outbound"));
    }

    #[test]
    fn filters_bind_a_mailbox_then_a_folder() {
        let filters = MessageFilters {
            mailbox_id: Some(Uuid::now_v7()),
            folder: Some("drafts".to_string()),
            read: Some("unread".to_string()),
            ..MessageFilters::default()
        };
        let clause = filters.clause();
        assert!(
            clause.sql.starts_with("WHERE m.mailbox_id = $1"),
            "{}",
            clause.sql
        );
        assert!(clause.sql.contains("= $2"), "{}", clause.sql);
        assert!(clause.sql.contains(UNREAD_SQL));
        assert_eq!(clause.values.len(), 2);
    }

    #[test]
    fn a_starred_view_does_not_also_filter_by_folder() {
        let filters = MessageFilters {
            folder: Some("starred".to_string()),
            ..MessageFilters::default()
        };
        let clause = filters.clause();
        assert!(clause.sql.contains("COALESCE(f.starred, false) = true"));
        assert!(!clause.sql.contains("CASE"), "{}", clause.sql);
    }

    #[test]
    fn a_read_filter_keeps_the_senders_own_copy() {
        let filters = MessageFilters {
            read: Some("read".to_string()),
            ..MessageFilters::default()
        };
        let clause = filters.clause();
        assert!(clause.sql.contains("m.direction = 'outbound'"));
        assert!(clause.sql.contains("COALESCE(f.read, false) = true"));
        assert!(!clause.sql.contains("CASE"), "{}", clause.sql);
    }

    #[test]
    fn an_empty_filter_list_has_no_where() {
        let clause = MessageFilters::default().clause();
        assert!(clause.sql.is_empty());
        assert!(clause.values.is_empty());
    }
}
