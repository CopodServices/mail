use crate::delivery;
use crate::session;
use crate::state::AppState;
use axum::Json;
use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRequestParts, Multipart, Path, Query, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use entity::{contact, folder, message, message_flag, outbound_job, setting, template};
use mailcore::policy::AuthOutcome;
use mailcore::{address, repo};
use mailcore::{body, compose, store};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, QueryFilter, QueryOrder, QuerySelect,
    Set,
};
use serde::{Deserialize, Serialize};
use utoipa::{OpenApi, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_scalar::{Scalar, Servable};
use uuid::Uuid;

#[derive(OpenApi)]
#[openapi(
    tags(
        (name = "session", description = "Webmail sign-in"),
        (name = "messages", description = "Stored mail"),
        (name = "mailboxes", description = "Mailboxes"),
        (name = "domains", description = "Tenant domains"),
        (name = "send", description = "Outbound submission through the relay")
    ),
    info(
        title = "cPOD Mail API",
        version = "0.1.0",
        description = "Inbound MTA that stores mail in S3, indexes it in PostgreSQL and relays outbound mail through a smarthost."
    )
)]
struct ApiDoc;

pub fn router(state: AppState) -> axum::Router {
    let (router, api) = OpenApiRouter::with_openapi(ApiDoc::openapi())
        .merge(session_router())
        .merge(messages_router())
        .merge(mailboxes_router())
        .merge(domains_router())
        .merge(send_router())
        .merge(drafts_router())
        .merge(contacts_router())
        .with_state(state)
        .split_for_parts();

    router
        .route("/healthz", get(health))
        .route("/readyz", get(ready))
        .merge(Scalar::with_url("/scalar", api.clone()))
        .route("/scalar/openapi.json", get(Json(api)))
}

fn session_router() -> OpenApiRouter<AppState> {
    use utoipa_axum::routes;
    OpenApiRouter::new()
        .routes(routes!(create_session))
        .routes(routes!(get_session))
        .routes(routes!(delete_session))
}

fn messages_router() -> OpenApiRouter<AppState> {
    use utoipa_axum::routes;
    OpenApiRouter::new()
        .routes(routes!(list_messages))
        .routes(routes!(message_counts))
        .routes(routes!(message_navigation))
        .routes(routes!(bulk_messages))
        .routes(routes!(empty_folder))
        .routes(routes!(get_message))
        .routes(routes!(get_message_raw))
        .routes(routes!(get_message_thread))
        .routes(routes!(get_message_metadata))
        .routes(routes!(cancel_scheduled_send))
        .routes(routes!(get_message_attachment))
        .routes(routes!(set_message_read))
        .routes(routes!(set_message_starred))
        .routes(routes!(snooze_message))
        .routes(routes!(unsnooze_message))
        .routes(routes!(realtime))
        .routes(routes!(delete_message))
        .routes(routes!(list_folders))
        .routes(routes!(create_folder))
}

fn mailboxes_router() -> OpenApiRouter<AppState> {
    use utoipa_axum::routes;
    OpenApiRouter::new()
        .routes(routes!(list_mailboxes))
        .routes(routes!(create_mailbox))
        .routes(routes!(delete_mailbox))
        .routes(routes!(update_mailbox))
}

fn domains_router() -> OpenApiRouter<AppState> {
    use utoipa_axum::routes;
    OpenApiRouter::new()
        .routes(routes!(list_domains))
        .routes(routes!(create_domain))
        .routes(routes!(get_domain))
        .routes(routes!(update_domain))
        .routes(routes!(delete_domain))
}

fn contacts_router() -> OpenApiRouter<AppState> {
    use utoipa_axum::routes;
    OpenApiRouter::new()
        .routes(routes!(get_contact))
        .routes(routes!(update_contact))
        .routes(routes!(block_contact))
        .routes(routes!(get_contact_avatar))
        .routes(routes!(upload_contact_avatar))
        .routes(routes!(delete_contact_avatar))
}

fn drafts_router() -> OpenApiRouter<AppState> {
    use utoipa_axum::routes;
    OpenApiRouter::new()
        .routes(routes!(create_draft))
        .routes(routes!(get_draft))
        .routes(routes!(update_draft))
        .routes(routes!(delete_draft))
        .routes(routes!(upload_draft_attachments))
        .routes(routes!(delete_draft_attachment))
}

fn send_router() -> OpenApiRouter<AppState> {
    use utoipa_axum::routes;
    OpenApiRouter::new()
        .routes(routes!(send))
        .routes(routes!(attachment_policy))
        .routes(routes!(list_templates))
        .routes(routes!(create_template))
        .routes(routes!(get_template))
        .routes(routes!(delete_template))
        .routes(routes!(get_shortcuts_setting))
        .routes(routes!(update_shortcuts_setting))
        .routes(routes!(get_recipient_addresses_setting))
        .routes(routes!(update_recipient_addresses_setting))
}

async fn health() -> &'static str {
    "ok"
}

async fn ready() -> &'static str {
    "ok"
}

// ---------------------------------------------------------------------------
// Authentication
// ---------------------------------------------------------------------------

/// The mailbox a signed-in session may reach: exactly one address.
#[derive(Debug, Clone)]
pub struct SessionScope {
    pub mailbox_id: Uuid,
    pub address: String,
    pub domain: String,
}

/// The authenticated caller on a /api route: the operator (holding the API key,
/// which manages every tenant) or a mailbox session limited to its own mail.
#[derive(Debug, Clone)]
pub enum Principal {
    Operator,
    Mailbox(SessionScope),
}

impl Principal {
    pub fn scope(&self) -> Option<&SessionScope> {
        match self {
            Principal::Mailbox(scope) => Some(scope),
            Principal::Operator => None,
        }
    }

    /// The mailbox a session is limited to; None for the operator, who sees all.
    pub fn mailbox_scope(&self) -> Option<Uuid> {
        self.scope().map(|scope| scope.mailbox_id)
    }

    /// Admin operations (domains, mailbox lifecycle) are operator-only.
    pub fn require_operator(&self) -> Result<(), ApiError> {
        match self {
            Principal::Operator => Ok(()),
            Principal::Mailbox(_) => Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "Forbidden",
                "This operation requires the operator API key",
            )
            .code("forbidden")),
        }
    }
}

impl FromRequestParts<AppState> for Principal {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        if let Some(key) = state.config.api_key.as_deref() {
            let provided = bearer_token(parts);
            if let Some(value) = provided
                && constant_time_eq(value.as_bytes(), key.as_bytes())
            {
                return Ok(Principal::Operator);
            }
            return session_principal(parts, state)?
                .ok_or_else(|| unauthorized("Missing or invalid API key or session"));
        }

        // No API key configured: an unauthenticated browser is the operator
        // (the local development mode the client contract tests rely on), but a
        // valid session cookie still narrows the caller to its own mailbox.
        Ok(session_principal(parts, state)?.unwrap_or(Principal::Operator))
    }
}

fn bearer_token(parts: &Parts) -> Option<&str> {
    parts
        .headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
}

fn unauthorized(detail: &str) -> ApiError {
    ApiError::new(StatusCode::UNAUTHORIZED, "Unauthorized", detail).code("unauthorized")
}

/// Resolves a mailbox session from the session cookie and, for cookie-backed
/// writes, enforces the double-submit CSRF token.
///
/// When no API key is configured the token is also accepted as a bearer token
/// or a token query parameter, so a cross-origin dev server can sign in without
/// a same-origin cookie; production uses the cookie.
fn session_principal(parts: &Parts, state: &AppState) -> Result<Option<Principal>, ApiError> {
    let (token, from_cookie) = match request_cookie(&parts.headers, session::SESSION_COOKIE) {
        Some(token) => (token, true),
        None if state.config.api_key.is_none() => {
            let query = parts
                .uri
                .query()
                .and_then(|query| query_value(query, "token"));
            match bearer_token(parts).map(str::to_string).or(query) {
                Some(token) => (token, false),
                None => return Ok(None),
            }
        }
        None => return Ok(None),
    };

    let Some(claims) = session::verify(&state.config.session_secret, &token) else {
        return Ok(None);
    };
    let Ok(mailbox_id) = Uuid::parse_str(&claims.sub) else {
        return Ok(None);
    };

    if from_cookie && !matches!(parts.method.as_str(), "GET" | "HEAD" | "OPTIONS") {
        let expected = request_cookie(&parts.headers, session::CSRF_COOKIE).unwrap_or_default();
        let provided = parts
            .headers
            .get("x-csrf-token")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        if expected.is_empty() || !constant_time_eq(provided.as_bytes(), expected.as_bytes()) {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "Forbidden",
                "Missing or invalid CSRF token",
            )
            .code("csrf"));
        }
    }

    Ok(Some(Principal::Mailbox(SessionScope {
        mailbox_id,
        address: claims.email,
        domain: claims.domain,
    })))
}

/// Reads a cookie value out of a request's Cookie header.
fn request_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    let header = headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())?;
    session::cookie_value(header, name)
}

/// Reads one value out of a URL query string.
fn query_value(query: &str, name: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == name).then(|| value.to_string())
    })
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, ToSchema)]
pub struct SessionRequest {
    /// The mailbox address, e.g. user@example.com.
    pub address: String,
    pub password: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SessionResponse {
    pub mailbox_id: Uuid,
    pub address: String,
    pub domain: String,
}

/// Signs a mailbox in: the password is checked against the stored argon2 hash,
/// then an httpOnly session cookie and a readable CSRF cookie are set.
#[utoipa::path(
    post,
    path = "/api/session",
    tag = "session",
    request_body = SessionRequest,
    responses(
        (status = 200, description = "Signed in", body = DataResponse<SessionResponse>),
        (status = 401, description = "Wrong address or password")
    )
)]
pub async fn create_session(
    State(state): State<AppState>,
    Json(body): Json<SessionRequest>,
) -> Result<Response, ApiError> {
    let Some(mailbox) = repo::find_active_mailbox(&state.db, &body.address)
        .await
        .map_err(db_error)?
    else {
        return Err(unauthorized("Wrong address or password"));
    };
    if !mailcore::auth::verify_password(&mailbox.password_hash, &body.password) {
        return Err(unauthorized("Wrong address or password"));
    }

    let domain = mailbox
        .address
        .rsplit_once('@')
        .map(|(_, domain)| domain.to_string())
        .unwrap_or_default();
    let token = session::mint(
        &state.config.session_secret,
        mailbox.id,
        &mailbox.address,
        &domain,
    )
    .map_err(internal)?;
    let secure = state.config.session_cookie_secure;

    Ok(with_cookies(
        Json(DataResponse {
            data: SessionResponse {
                mailbox_id: mailbox.id,
                address: mailbox.address.clone(),
                domain,
            },
        })
        .into_response(),
        [
            session::session_cookie(&token, secure),
            session::csrf_cookie(&session::csrf_token(), secure),
        ],
    ))
}

/// The mailbox behind the current session, or null for the operator API key.
#[utoipa::path(
    get,
    path = "/api/session",
    tag = "session",
    responses((status = 200, description = "The signed-in mailbox", body = DataResponse<Option<SessionResponse>>))
)]
pub async fn get_session(principal: Principal) -> Response {
    let data = principal.scope().map(|scope| SessionResponse {
        mailbox_id: scope.mailbox_id,
        address: scope.address.clone(),
        domain: scope.domain.clone(),
    });
    Json(DataResponse { data }).into_response()
}

/// Clears both session cookies.
#[utoipa::path(
    delete,
    path = "/api/session",
    tag = "session",
    responses((status = 204, description = "Signed out"))
)]
pub async fn delete_session() -> Response {
    with_cookies(
        StatusCode::NO_CONTENT.into_response(),
        [
            session::cleared_cookie(session::SESSION_COOKIE),
            session::cleared_cookie(session::CSRF_COOKIE),
        ],
    )
}

/// Appends Set-Cookie headers to a response.
fn with_cookies<const N: usize>(mut response: Response, cookies: [String; N]) -> Response {
    for cookie in cookies {
        if let Ok(value) = HeaderValue::from_str(&cookie) {
            response.headers_mut().append(header::SET_COOKIE, value);
        }
    }
    response
}

// ---------------------------------------------------------------------------
// Errors and envelopes
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    title: &'static str,
    detail: String,
    code: Option<&'static str>,
}

impl ApiError {
    fn new(status: StatusCode, title: &'static str, detail: impl Into<String>) -> Self {
        Self {
            status,
            title,
            detail: detail.into(),
            code: None,
        }
    }

    fn code(mut self, code: &'static str) -> Self {
        self.code = Some(code);
        self
    }
}

#[derive(Serialize)]
struct Problem {
    #[serde(rename = "type")]
    kind: String,
    title: String,
    status: u16,
    detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<String>,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = Problem {
            kind: format!(
                "https://developer.mozilla.org/en-US/docs/Web/HTTP/Status/{}",
                self.status.as_u16()
            ),
            title: self.title.to_string(),
            status: self.status.as_u16(),
            detail: self.detail,
            code: self.code.map(str::to_string),
        };
        (self.status, Json(body)).into_response()
    }
}

/// Maps a persistence failure (sea-orm wrapped in anyhow) onto the API surface.
fn db_error(err: impl std::fmt::Display) -> ApiError {
    tracing::error!(error = %err, "database error");
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "Internal Server Error",
        "A database error occurred",
    )
    .code("database_error")
}

fn bad_request(detail: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "Bad Request", detail).code("bad_request")
}

fn not_found(detail: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "Not Found", detail).code("not_found")
}

fn conflict(detail: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::CONFLICT, "Conflict", detail).code("conflict")
}

fn internal(detail: impl std::fmt::Display) -> ApiError {
    tracing::error!(error = %detail, "internal error");
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "Internal Server Error",
        detail.to_string(),
    )
    .code("internal_error")
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DataResponse<T> {
    pub data: T,
}

impl<T: Serialize> IntoResponse for DataResponse<T> {
    fn into_response(self) -> Response {
        (StatusCode::OK, Json(self)).into_response()
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ListResponse<T> {
    pub data: Vec<T>,
    pub meta: Meta,
}

impl<T: Serialize> ListResponse<T> {
    fn new(data: Vec<T>) -> Self {
        let total = data.len() as i64;
        Self {
            data,
            meta: Meta {
                total,
                limit: None,
                offset: None,
            },
        }
    }

    /// A paginated collection: total is the filtered count, not the page size.
    fn page(data: Vec<T>, total: i64, limit: u64, offset: u64) -> Self {
        Self {
            data,
            meta: Meta {
                total,
                limit: Some(limit),
                offset: Some(offset),
            },
        }
    }
}

impl<T: Serialize> IntoResponse for ListResponse<T> {
    fn into_response(self) -> Response {
        (StatusCode::OK, Json(self)).into_response()
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct Meta {
    pub total: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<u64>,
}

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
pub struct MessageResponse {
    pub id: Uuid,
    pub mailbox_id: Option<Uuid>,
    pub direction: String,
    pub status: String,
    pub envelope_from: String,
    pub envelope_to: String,
    pub message_id: Option<String>,
    pub subject: Option<String>,
    pub header_from: Option<String>,
    pub size_bytes: i64,
    pub spf_result: Option<String>,
    pub dkim_result: Option<String>,
    pub dmarc_result: Option<String>,
    pub dmarc_policy: Option<String>,
    pub received_at: chrono::DateTime<chrono::Utc>,
    pub read: bool,
    pub starred: bool,
    /// When a snoozed message returns to the inbox.
    pub snoozed_until: Option<chrono::DateTime<chrono::Utc>>,
    /// inbox, sent, spam, trash, archived, snoozed, scheduled, or a folder id.
    pub folder: String,
}

/// The folder a message is in: the stored flag when there is one, otherwise the
/// folder implied by its direction and delivery status.
fn folder_of(model: &message::Model, flag: Option<&message_flag::Model>) -> String {
    if let Some(flag) = flag {
        if flag
            .snoozed_until
            .is_some_and(|until| until > chrono::Utc::now())
        {
            return "snoozed".to_string();
        }
        if flag.folder != "inbox" {
            return flag.folder.clone();
        }
    }
    match model.direction {
        message::Direction::Outbound => "sent".to_string(),
        message::Direction::Inbound => match model.status {
            message::Status::Quarantined => "spam".to_string(),
            message::Status::Stored => "inbox".to_string(),
        },
    }
}

impl MessageResponse {
    fn with_flags(model: message::Model, flag: Option<&message_flag::Model>) -> Self {
        Self {
            folder: folder_of(&model, flag),
            read: flag.map(|flag| flag.read).unwrap_or(false),
            starred: flag.map(|flag| flag.starred).unwrap_or(false),
            snoozed_until: flag.and_then(|flag| flag.snoozed_until),
            ..MessageResponse::from(model)
        }
    }
}

impl From<message::Model> for MessageResponse {
    fn from(model: message::Model) -> Self {
        Self {
            id: model.id,
            mailbox_id: model.mailbox_id,
            direction: format!("{:?}", model.direction).to_ascii_lowercase(),
            status: format!("{:?}", model.status).to_ascii_lowercase(),
            envelope_from: model.envelope_from,
            envelope_to: model.envelope_to,
            message_id: model.message_id,
            subject: model.subject,
            header_from: model.header_from,
            size_bytes: model.size_bytes,
            spf_result: model.spf_result,
            dkim_result: model.dkim_result,
            dmarc_result: model.dmarc_result,
            dmarc_policy: model.dmarc_policy,
            received_at: model.received_at,
            read: false,
            starred: false,
            snoozed_until: None,
            folder: "inbox".to_string(),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct MailboxResponse {
    pub id: Uuid,
    pub address: String,
    pub active: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// Appended to new messages composed from this mailbox.
    pub signature: Option<String>,
}

impl From<entity::mailbox::Model> for MailboxResponse {
    fn from(model: entity::mailbox::Model) -> Self {
        Self {
            id: model.id,
            address: model.address,
            active: model.active,
            created_at: model.created_at,
            signature: model.signature,
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateMailboxRequest {
    /// Replaces the mailbox signature; explicit null clears it, omitted leaves
    /// it unchanged. The one field a mailbox session may set on itself.
    #[serde(default, deserialize_with = "double_option")]
    pub signature: Option<Option<String>>,
    /// Operator only: rotate the mailbox password.
    #[serde(default)]
    pub password: Option<String>,
    /// Operator only: storage quota in bytes (0 for unlimited).
    #[serde(default)]
    pub quota_bytes: Option<i64>,
    /// Operator only: activate or deactivate the mailbox.
    #[serde(default)]
    pub active: Option<bool>,
}

/// Distinguishes an absent JSON field (None) from an explicit null (Some(None)).
fn double_option<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DomainResponse {
    pub id: Uuid,
    pub name: String,
    pub active: bool,
}

impl From<entity::domain::Model> for DomainResponse {
    fn from(model: entity::domain::Model) -> Self {
        Self {
            id: model.id,
            name: model.name,
            active: model.active,
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateDomainRequest {
    /// Operator only: activate or deactivate the whole tenant.
    pub active: bool,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateMailbox {
    pub address: String,
    pub password: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateDomain {
    pub name: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SendRequest {
    pub from: String,
    pub to: Vec<String>,
    #[serde(default)]
    pub cc: Vec<String>,
    #[serde(default)]
    pub bcc: Vec<String>,
    pub subject: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub html: Option<String>,
    /// Draft whose stored attachments should go out with this message.
    #[serde(default)]
    pub draft_id: Option<Uuid>,
    /// Send no earlier than this time; the message waits in Scheduled until then.
    #[serde(default)]
    pub scheduled_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Strips a display name, leaving the bare address inside angle brackets.
fn bare_address(value: &str) -> String {
    let trimmed = value.trim().trim_matches('"');
    if let (Some(start), Some(end)) = (trimmed.find('<'), trimmed.rfind('>'))
        && start < end
    {
        return trimmed[start + 1..end].trim().to_string();
    }
    trimmed.to_string()
}

/// The bare sender address behind what a client sends as the sender.
///
/// A mail client sends the full header (`"Info" <info@example.com>`): the display
/// name belongs on the message's From header, but the SMTP envelope sender and
/// the mailbox-scope check need the address alone. Recipients are read the same
/// way. None when nothing usable is left.
fn bare_sender(value: &str) -> Option<String> {
    address::parse(&bare_address(value)).map(|address| address.normalized())
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SendResponse {
    pub stored: usize,
    pub queued: usize,
    /// True when the message was queued for a later time instead of sent now.
    pub scheduled: bool,
}

#[derive(Debug, Default, Deserialize)]
pub struct MessageQuery {
    /// Mailbox address, or use mailbox_id.
    pub mailbox: Option<String>,
    pub mailbox_id: Option<Uuid>,
    /// System folder or custom folder id.
    pub folder: Option<String>,
    pub direction: Option<String>,
    /// Free-text match on subject and envelope addresses.
    pub q: Option<String>,
    /// "read" or "unread".
    pub read: Option<String>,
    /// "true" restricts the list to starred messages.
    pub starred: Option<String>,
    pub limit: Option<u64>,
    pub offset: Option<u64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AttachmentResponse {
    /// Position of the attachment in the message, used to fetch its bytes.
    pub index: usize,
    pub filename: Option<String>,
    pub content_type: Option<String>,
    pub size_bytes: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct MessageDetailResponse {
    pub message: MessageResponse,
    pub text: Option<String>,
    pub html: Option<String>,
    pub attachments: Vec<AttachmentResponse>,
    pub unsubscribe_url: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct MessageMetadataResponse {
    pub attachments: Vec<AttachmentResponse>,
    pub unsubscribe_url: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct FolderCount {
    pub total: i64,
    pub unread: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct MailboxCount {
    pub mailbox_id: Uuid,
    pub total: i64,
    pub unread: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CountsResponse {
    pub folders: std::collections::BTreeMap<String, FolderCount>,
    pub custom_folders: std::collections::BTreeMap<String, FolderCount>,
    pub mailboxes: Vec<MailboxCount>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct FolderResponse {
    pub id: String,
    pub name: String,
    /// "system" for the built-in folders, "custom" for user-created ones.
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateFolderRequest {
    pub mailbox: Option<String>,
    pub mailbox_id: Option<Uuid>,
    pub name: String,
    #[serde(default)]
    pub color: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SnoozeRequest {
    /// When the message should return to the inbox, RFC 3339.
    pub snoozed_until: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct FlagRequest {
    #[serde(default)]
    pub read: Option<bool>,
    #[serde(default)]
    pub starred: Option<bool>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct BulkRequest {
    pub message_ids: Vec<Uuid>,
    /// archive, trash, spam, inbox, read, unread, starred, unstarred, delete.
    pub action: String,
    /// Target folder for the "folder" action.
    pub folder: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct BulkResponse {
    pub updated: usize,
    pub deleted: usize,
}

/// The system folders every mailbox has, matching what the web client renders.
const SYSTEM_FOLDERS: [(&str, &str); 9] = [
    ("inbox", "Inbox"),
    ("starred", "Starred"),
    ("snoozed", "Snoozed"),
    ("scheduled", "Scheduled"),
    ("sent", "Sent"),
    ("drafts", "Drafts"),
    ("archived", "Archived"),
    ("spam", "Spam"),
    ("trash", "Trash"),
];

/// The mailbox a request targets. A session is pinned to its own mailbox, so
/// the client's mailbox/mailbox_id filters are ignored for it.
async fn resolve_mailbox(
    state: &AppState,
    query: &MessageQuery,
    scope: Option<Uuid>,
) -> Result<Option<Uuid>, ApiError> {
    if let Some(mailbox_id) = scope {
        return Ok(Some(mailbox_id));
    }
    if let Some(id) = query.mailbox_id {
        return Ok(Some(id));
    }
    match query.mailbox.as_deref() {
        Some(address) => Ok(repo::find_mailbox(&state.db, address)
            .await
            .map_err(db_error)?
            .map(|mailbox| mailbox.id)),
        None => Ok(None),
    }
}

/// 404s when a mailbox session reaches for anything but its own mailbox.
fn ensure_scope(
    principal: &Principal,
    mailbox_id: Option<Uuid>,
    label: &str,
) -> Result<(), ApiError> {
    match principal.scope() {
        Some(scope) if mailbox_id != Some(scope.mailbox_id) => Err(not_found(label)),
        _ => Ok(()),
    }
}

/// Client preferences are per mailbox for a session, one shared row for the
/// operator.
fn settings_scope(principal: &Principal) -> &str {
    principal
        .scope()
        .map(|scope| scope.address.as_str())
        .unwrap_or(SETTINGS_SCOPE)
}

async fn load_flags(state: &AppState, ids: &[Uuid]) -> Result<Vec<message_flag::Model>, ApiError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    message_flag::Entity::find()
        .filter(message_flag::Column::MessageId.is_in(ids.to_vec()))
        .all(&state.db)
        .await
        .map_err(db_error)
}

async fn upsert_flag(
    state: &AppState,
    message_id: Uuid,
    read: Option<bool>,
    starred: Option<bool>,
    folder: Option<String>,
) -> Result<message_flag::Model, ApiError> {
    let existing = message_flag::Entity::find_by_id(message_id)
        .one(&state.db)
        .await
        .map_err(db_error)?;
    let model = message_flag::ActiveModel {
        message_id: Set(message_id),
        read: Set(read
            .or(existing.as_ref().map(|flag| flag.read))
            .unwrap_or(false)),
        starred: Set(starred
            .or(existing.as_ref().map(|flag| flag.starred))
            .unwrap_or(false)),
        folder: Set(folder
            .or_else(|| existing.as_ref().map(|flag| flag.folder.clone()))
            .unwrap_or_else(|| "inbox".to_string())),
        snoozed_until: Set(existing.as_ref().and_then(|flag| flag.snoozed_until)),
        updated_at: Set(chrono::Utc::now()),
    };
    // The row keyed by message_id may already exist, so update instead of insert.
    if existing.is_some() {
        model.update(&state.db).await.map_err(db_error)
    } else {
        model.insert(&state.db).await.map_err(db_error)
    }
}

/// Loads the messages matching a query, newest first, with their reader flags.
/// Folder, read and starred live in message_flags, so filtering happens here.
async fn filtered_messages(
    state: &AppState,
    query: &MessageQuery,
    scope: Option<Uuid>,
) -> Result<Vec<(message::Model, Option<message_flag::Model>)>, ApiError> {
    let mailbox_id = resolve_mailbox(state, query, scope).await?;

    let mut condition = Condition::all();
    if let Some(mailbox_id) = mailbox_id {
        condition = condition.add(message::Column::MailboxId.eq(mailbox_id));
    }
    match query.direction.as_deref() {
        Some("inbound") => {
            condition = condition.add(message::Column::Direction.eq(message::Direction::Inbound));
        }
        Some("outbound") => {
            condition = condition.add(message::Column::Direction.eq(message::Direction::Outbound));
        }
        _ => {}
    }
    if let Some(text) = query.q.as_deref().filter(|value| !value.trim().is_empty()) {
        let like = format!("%{}%", text.trim());
        condition = condition.add(
            Condition::any()
                .add(message::Column::Subject.like(&like))
                .add(message::Column::EnvelopeFrom.like(&like))
                .add(message::Column::EnvelopeTo.like(&like)),
        );
    }

    let rows = message::Entity::find()
        .filter(condition)
        .order_by_desc(message::Column::ReceivedAt)
        .limit(5000)
        .all(&state.db)
        .await
        .map_err(db_error)?;

    let ids: Vec<Uuid> = rows.iter().map(|row| row.id).collect();
    let flags = load_flags(state, &ids).await?;
    let mut items: Vec<(message::Model, Option<message_flag::Model>)> = rows
        .into_iter()
        .map(|row| {
            let flag = flags.iter().find(|flag| flag.message_id == row.id).cloned();
            (row, flag)
        })
        .collect();

    if let Some(folder) = query.folder.as_deref().filter(|value| !value.is_empty()) {
        if folder == "starred" {
            // Starred is a flag view, not a folder a message is moved into.
            items.retain(|(_, flag)| flag.as_ref().map(|flag| flag.starred).unwrap_or(false));
        } else {
            items.retain(|(row, flag)| folder_of(row, flag.as_ref()) == folder);
        }
    }
    match query.read.as_deref() {
        Some("read") => {
            items.retain(|(_, flag)| flag.as_ref().map(|flag| flag.read).unwrap_or(false))
        }
        Some("unread") => {
            items.retain(|(_, flag)| !flag.as_ref().map(|flag| flag.read).unwrap_or(false))
        }
        _ => {}
    }
    if query.starred.as_deref() == Some("true") {
        items.retain(|(_, flag)| flag.as_ref().map(|flag| flag.starred).unwrap_or(false));
    }

    Ok(items)
}

#[derive(Debug, Serialize, ToSchema)]
pub struct NavigationEntry {
    pub id: Uuid,
    pub unread: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ThreadMessageResponse {
    #[serde(flatten)]
    pub message: MessageResponse,
    pub text: Option<String>,
    pub html: Option<String>,
    pub attachments: Vec<AttachmentResponse>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ThreadResponse {
    pub thread_id: String,
    pub messages: Vec<ThreadMessageResponse>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct EmptyRequest {
    pub mailbox_id: Option<Uuid>,
    /// Mailbox address, when the id is not supplied.
    pub mailbox: Option<String>,
    /// trash or spam.
    pub folder: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct EmptyResponse {
    pub deleted: usize,
    pub remaining: usize,
}

/// Walks In-Reply-To back to the oldest stored ancestor and returns its id.
fn thread_root(start: &message::Model, rows: &[message::Model]) -> String {
    let mut current = start;
    let mut visited = std::collections::HashSet::new();
    while let Some(parent_id) = current.in_reply_to.as_deref() {
        if !visited.insert(parent_id.to_string()) {
            break;
        }
        match rows
            .iter()
            .find(|row| row.message_id.as_deref() == Some(parent_id))
        {
            Some(parent) => current = parent,
            None => break,
        }
    }
    current
        .message_id
        .clone()
        .unwrap_or_else(|| current.id.to_string())
}

/// True when a message belongs to the conversation identified by the root id.
fn in_thread(row: &message::Model, root: &str) -> bool {
    if row.message_id.as_deref() == Some(root) {
        return true;
    }
    if row.in_reply_to.as_deref() == Some(root) {
        return true;
    }
    row.references_header
        .as_deref()
        .is_some_and(|references| references.split_whitespace().any(|id| id == root))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AttachmentPolicyResponse {
    /// Largest total attachment size the server accepts, in megabytes.
    pub max_mb: u64,
    /// Above this the composer warns that the message may exceed the size budget.
    pub warn_threshold_bytes: i64,
}

#[utoipa::path(
    get,
    path = "/api/attachment-policy",
    tag = "send",
    responses((status = 200, description = "Attachment limits for the composer", body = DataResponse<AttachmentPolicyResponse>))
)]
pub async fn attachment_policy(
    State(state): State<AppState>,
    _principal: Principal,
) -> Result<Response, ApiError> {
    let max_bytes = state.config.max_message_size as i64;
    Ok(Json(DataResponse {
        data: AttachmentPolicyResponse {
            max_mb: (max_bytes / (1024 * 1024)).max(1) as u64,
            warn_threshold_bytes: max_bytes / 2,
        },
    })
    .into_response())
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

#[utoipa::path(
    get,
    path = "/api/messages",
    tag = "messages",
    params(
        ("mailbox" = Option<String>, Query, description = "Filter by mailbox address"),
        ("mailbox_id" = Option<Uuid>, Query, description = "Filter by mailbox id"),
        ("folder" = Option<String>, Query, description = "Filter by folder"),
        ("direction" = Option<String>, Query, description = "inbound or outbound"),
        ("q" = Option<String>, Query, description = "Match subject and addresses"),
        ("read" = Option<String>, Query, description = "read or unread"),
        ("starred" = Option<String>, Query, description = "true for starred only"),
        ("limit" = Option<u64>, Query, description = "Page size (default 50)"),
        ("offset" = Option<u64>, Query, description = "Rows to skip")
    ),
    responses((status = 200, description = "Stored messages", body = ListResponse<MessageResponse>))
)]
pub async fn list_messages(
    State(state): State<AppState>,
    principal: Principal,
    Query(query): Query<MessageQuery>,
) -> Result<Response, ApiError> {
    let items = filtered_messages(&state, &query, principal.mailbox_scope()).await?;
    let total = items.len() as i64;
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let offset = query.offset.unwrap_or(0);
    let page = items
        .into_iter()
        .skip(offset as usize)
        .take(limit as usize)
        .map(|(row, flag)| MessageResponse::with_flags(row, flag.as_ref()))
        .collect();

    Ok(ListResponse::page(page, total, limit, offset).into_response())
}

#[utoipa::path(
    get,
    path = "/api/messages/{id}",
    tag = "messages",
    responses(
        (status = 200, description = "Message with body and attachments", body = DataResponse<MessageDetailResponse>),
        (status = 404, description = "Unknown message")
    )
)]
pub async fn get_message(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let message = repo::get_message(&state.db, id)
        .await
        .map_err(db_error)?
        .ok_or_else(|| not_found("Unknown message"))?;
    ensure_scope(&principal, message.mailbox_id, "Unknown message")?;
    let flag = message_flag::Entity::find_by_id(message.id)
        .one(&state.db)
        .await
        .map_err(db_error)?;
    let raw = state
        .store
        .get(&message.storage_key)
        .await
        .map_err(internal)?;
    let parsed = body::extract(&raw);
    let attachments = parsed
        .attachments
        .iter()
        .enumerate()
        .map(|(index, attachment)| AttachmentResponse {
            index,
            filename: attachment.filename.clone(),
            content_type: attachment.content_type.clone(),
            size_bytes: attachment.bytes.len() as i64,
        })
        .collect();

    Ok((
        StatusCode::OK,
        Json(DataResponse {
            data: MessageDetailResponse {
                message: MessageResponse::with_flags(message, flag.as_ref()),
                text: parsed.text,
                html: parsed.html,
                attachments,
                unsubscribe_url: parsed.unsubscribe_url,
            },
        }),
    )
        .into_response())
}

#[utoipa::path(
    get,
    path = "/api/messages/{id}/raw",
    tag = "messages",
    responses(
        (status = 200, description = "Raw RFC5322 message"),
        (status = 404, description = "Unknown message")
    )
)]
pub async fn get_message_raw(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let message = repo::get_message(&state.db, id)
        .await
        .map_err(db_error)?
        .ok_or_else(|| not_found("Unknown message"))?;
    ensure_scope(&principal, message.mailbox_id, "Unknown message")?;
    let bytes = state
        .store
        .get(&message.storage_key)
        .await
        .map_err(internal)?;

    Ok(Response::builder()
        .header(header::CONTENT_TYPE, "message/rfc822")
        .body(Body::from(bytes))
        .expect("valid response"))
}

#[utoipa::path(
    delete,
    path = "/api/messages/{id}",
    tag = "messages",
    responses(
        (status = 204, description = "Deleted"),
        (status = 404, description = "Unknown message")
    )
)]
pub async fn delete_message(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let message = repo::get_message(&state.db, id)
        .await
        .map_err(db_error)?
        .ok_or_else(|| not_found("Unknown message"))?;
    ensure_scope(&principal, message.mailbox_id, "Unknown message")?;
    repo::delete_message(&state.db, id)
        .await
        .map_err(db_error)?;

    if let Err(err) = state.store.delete(&message.storage_key).await {
        tracing::warn!(error = %err, key = %message.storage_key, "failed to remove the stored object");
    }

    Ok(StatusCode::NO_CONTENT.into_response())
}

#[utoipa::path(
    post,
    path = "/api/messages/{id}/snooze",
    tag = "messages",
    request_body = SnoozeRequest,
    params(("id" = Uuid, Path, description = "Message id")),
    responses((status = 200, description = "Snoozed message", body = DataResponse<MessageResponse>))
)]
pub async fn snooze_message(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<Uuid>,
    Json(body): Json<SnoozeRequest>,
) -> Result<Response, ApiError> {
    set_snooze(&state, &principal, id, Some(body.snoozed_until)).await
}

#[utoipa::path(
    delete,
    path = "/api/messages/{id}/snooze",
    tag = "messages",
    params(("id" = Uuid, Path, description = "Message id")),
    responses((status = 200, description = "Unsnoozed message", body = DataResponse<MessageResponse>))
)]
pub async fn unsnooze_message(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    set_snooze(&state, &principal, id, None).await
}

async fn set_snooze(
    state: &AppState,
    principal: &Principal,
    id: Uuid,
    until: Option<chrono::DateTime<chrono::Utc>>,
) -> Result<Response, ApiError> {
    let message = repo::get_message(&state.db, id)
        .await
        .map_err(db_error)?
        .ok_or_else(|| not_found("Unknown message"))?;
    ensure_scope(principal, message.mailbox_id, "Unknown message")?;
    let existing = message_flag::Entity::find_by_id(id)
        .one(&state.db)
        .await
        .map_err(db_error)?;
    let model = message_flag::ActiveModel {
        message_id: Set(id),
        read: Set(existing.as_ref().map(|flag| flag.read).unwrap_or(false)),
        starred: Set(existing.as_ref().map(|flag| flag.starred).unwrap_or(false)),
        folder: Set(existing
            .as_ref()
            .map(|flag| flag.folder.clone())
            .unwrap_or_else(|| "inbox".to_string())),
        snoozed_until: Set(until),
        updated_at: Set(chrono::Utc::now()),
    };
    let flag = if existing.is_some() {
        model.update(&state.db).await.map_err(db_error)?
    } else {
        model.insert(&state.db).await.map_err(db_error)?
    };
    Ok(Json(DataResponse {
        data: MessageResponse::with_flags(message, Some(&flag)),
    })
    .into_response())
}

// ---------------------------------------------------------------------------
// Reader state, folders and counts
// ---------------------------------------------------------------------------

#[utoipa::path(
    post,
    path = "/api/messages/{id}/read",
    tag = "messages",
    request_body = FlagRequest,
    params(("id" = Uuid, Path, description = "Message id")),
    responses((status = 200, description = "Updated message", body = DataResponse<MessageResponse>))
)]
pub async fn set_message_read(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<Uuid>,
    body: Option<Json<FlagRequest>>,
) -> Result<Response, ApiError> {
    let read = body.and_then(|Json(body)| body.read).unwrap_or(true);
    let message = flag_message(&state, &principal, id, Some(read), None, None).await?;
    Ok(Json(DataResponse { data: message }).into_response())
}

#[utoipa::path(
    post,
    path = "/api/messages/{id}/star",
    tag = "messages",
    request_body = FlagRequest,
    params(("id" = Uuid, Path, description = "Message id")),
    responses((status = 200, description = "Updated message", body = DataResponse<MessageResponse>))
)]
pub async fn set_message_starred(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<Uuid>,
    body: Option<Json<FlagRequest>>,
) -> Result<Response, ApiError> {
    let starred = body.and_then(|Json(body)| body.starred).unwrap_or(true);
    let message = flag_message(&state, &principal, id, None, Some(starred), None).await?;
    Ok(Json(DataResponse { data: message }).into_response())
}

async fn flag_message(
    state: &AppState,
    principal: &Principal,
    id: Uuid,
    read: Option<bool>,
    starred: Option<bool>,
    folder: Option<String>,
) -> Result<MessageResponse, ApiError> {
    let message = repo::get_message(&state.db, id)
        .await
        .map_err(db_error)?
        .ok_or_else(|| not_found("Unknown message"))?;
    ensure_scope(principal, message.mailbox_id, "Unknown message")?;
    let flag = upsert_flag(state, id, read, starred, folder).await?;
    Ok(MessageResponse::with_flags(message, Some(&flag)))
}

#[utoipa::path(
    post,
    path = "/api/messages/bulk",
    tag = "messages",
    request_body = BulkRequest,
    responses((status = 200, description = "How many messages changed", body = DataResponse<BulkResponse>))
)]
pub async fn bulk_messages(
    State(state): State<AppState>,
    principal: Principal,
    Json(body): Json<BulkRequest>,
) -> Result<Response, ApiError> {
    let mut updated = 0usize;
    let mut deleted = 0usize;
    for id in &body.message_ids {
        let Some(message) = repo::get_message(&state.db, *id).await.map_err(db_error)? else {
            continue;
        };
        // A session silently skips any id outside its mailbox.
        if ensure_scope(&principal, message.mailbox_id, "Unknown message").is_err() {
            continue;
        }
        match body.action.as_str() {
            "delete" => {
                repo::delete_message(&state.db, *id)
                    .await
                    .map_err(db_error)?;
                if let Err(err) = state.store.delete(&message.storage_key).await {
                    tracing::warn!(error = %err, key = %message.storage_key, "failed to remove the stored object");
                }
                deleted += 1;
            }
            "read" | "unread" => {
                upsert_flag(&state, *id, Some(body.action == "read"), None, None).await?;
                updated += 1;
            }
            "starred" | "unstarred" => {
                upsert_flag(&state, *id, None, Some(body.action == "starred"), None).await?;
                updated += 1;
            }
            "archive" => {
                upsert_flag(&state, *id, None, None, Some("archived".to_string())).await?;
                updated += 1;
            }
            "trash" => {
                upsert_flag(&state, *id, None, None, Some("trash".to_string())).await?;
                updated += 1;
            }
            "spam" => {
                upsert_flag(&state, *id, None, None, Some("spam".to_string())).await?;
                updated += 1;
            }
            "inbox" => {
                upsert_flag(&state, *id, None, None, Some("inbox".to_string())).await?;
                updated += 1;
            }
            "folder" => {
                let folder = body
                    .folder
                    .clone()
                    .ok_or_else(|| bad_request("folder is required for the folder action"))?;
                upsert_flag(&state, *id, None, None, Some(folder)).await?;
                updated += 1;
            }
            other => return Err(bad_request(format!("unknown action: {other}"))),
        }
    }
    Ok(Json(DataResponse {
        data: BulkResponse { updated, deleted },
    })
    .into_response())
}

#[utoipa::path(
    get,
    path = "/api/messages/{id}/attachments/{index}",
    tag = "messages",
    params(
        ("id" = Uuid, Path, description = "Message id"),
        ("index" = usize, Path, description = "Attachment position from the message detail")
    ),
    responses(
        (status = 200, description = "Attachment bytes"),
        (status = 404, description = "Unknown message or attachment")
    )
)]
pub async fn get_message_attachment(
    State(state): State<AppState>,
    principal: Principal,
    Path((id, index)): Path<(Uuid, usize)>,
) -> Result<Response, ApiError> {
    let message = repo::get_message(&state.db, id)
        .await
        .map_err(db_error)?
        .ok_or_else(|| not_found("Unknown message"))?;
    ensure_scope(&principal, message.mailbox_id, "Unknown message")?;
    let raw = state
        .store
        .get(&message.storage_key)
        .await
        .map_err(internal)?;
    let parsed = body::extract(&raw);
    let attachment = parsed
        .attachments
        .into_iter()
        .nth(index)
        .ok_or_else(|| not_found("Unknown attachment"))?;

    let filename = attachment
        .filename
        .unwrap_or_else(|| "attachment".to_string());
    let disposition = format!("attachment; filename={}", filename.replace('"', ""));
    Ok(Response::builder()
        .header(
            header::CONTENT_TYPE,
            attachment
                .content_type
                .unwrap_or_else(|| "application/octet-stream".to_string()),
        )
        .header(header::CONTENT_DISPOSITION, disposition)
        .body(Body::from(attachment.bytes))
        .expect("valid response"))
}

#[utoipa::path(
    get,
    path = "/api/messages/counts",
    tag = "messages",
    params(
        ("mailbox" = Option<String>, Query, description = "Limit to one mailbox address"),
        ("mailbox_id" = Option<Uuid>, Query, description = "Limit to one mailbox id")
    ),
    responses((status = 200, description = "Folder and mailbox counts", body = DataResponse<CountsResponse>))
)]
pub async fn message_counts(
    State(state): State<AppState>,
    principal: Principal,
    Query(query): Query<MessageQuery>,
) -> Result<Response, ApiError> {
    let items = filtered_messages(&state, &query, principal.mailbox_scope()).await?;

    let mut folders: std::collections::BTreeMap<String, FolderCount> = SYSTEM_FOLDERS
        .iter()
        .map(|(id, _)| {
            (
                (*id).to_string(),
                FolderCount {
                    total: 0,
                    unread: 0,
                },
            )
        })
        .collect();
    let mut mailboxes: std::collections::BTreeMap<Uuid, MailboxCount> =
        std::collections::BTreeMap::new();

    for (row, flag) in &items {
        let folder = folder_of(row, flag.as_ref());
        let unread = !flag.as_ref().map(|flag| flag.read).unwrap_or(false);

        if let Some(entry) = folders.get_mut(&folder) {
            entry.total += 1;
            if unread {
                entry.unread += 1;
            }
        }
        if flag.as_ref().map(|flag| flag.starred).unwrap_or(false)
            && let Some(entry) = folders.get_mut("starred")
        {
            entry.total += 1;
            if unread {
                entry.unread += 1;
            }
        }
        if let Some(mailbox_id) = row.mailbox_id {
            let entry = mailboxes.entry(mailbox_id).or_insert(MailboxCount {
                mailbox_id,
                total: 0,
                unread: 0,
            });
            entry.total += 1;
            if unread {
                entry.unread += 1;
            }
        }
    }

    // Custom folders are counted by their folder id.
    let mut custom_folders: std::collections::BTreeMap<String, FolderCount> =
        std::collections::BTreeMap::new();
    for (row, flag) in &items {
        let folder = folder_of(row, flag.as_ref());
        if SYSTEM_FOLDERS.iter().any(|(id, _)| *id == folder) {
            continue;
        }
        let entry = custom_folders.entry(folder).or_insert(FolderCount {
            total: 0,
            unread: 0,
        });
        entry.total += 1;
        if !flag.as_ref().map(|flag| flag.read).unwrap_or(false) {
            entry.unread += 1;
        }
    }

    Ok(Json(DataResponse {
        data: CountsResponse {
            folders,
            custom_folders,
            mailboxes: mailboxes.into_values().collect(),
        },
    })
    .into_response())
}

#[utoipa::path(
    get,
    path = "/api/folders",
    tag = "mailboxes",
    params(("mailbox" = Option<String>, Query, description = "Mailbox address")),
    responses((status = 200, description = "Folders", body = ListResponse<FolderResponse>))
)]
pub async fn list_folders(
    State(state): State<AppState>,
    principal: Principal,
    Query(query): Query<MessageQuery>,
) -> Result<Response, ApiError> {
    let mut folders: Vec<FolderResponse> = SYSTEM_FOLDERS
        .iter()
        .map(|(id, name)| FolderResponse {
            id: (*id).to_string(),
            name: (*name).to_string(),
            kind: "system".to_string(),
            color: None,
        })
        .collect();

    if let Some(mailbox_id) = resolve_mailbox(&state, &query, principal.mailbox_scope()).await? {
        let custom = folder::Entity::find()
            .filter(folder::Column::MailboxId.eq(mailbox_id))
            .order_by_asc(folder::Column::Name)
            .all(&state.db)
            .await
            .map_err(db_error)?;
        folders.extend(custom.into_iter().map(|row| FolderResponse {
            id: row.id.to_string(),
            name: row.name,
            kind: "custom".to_string(),
            color: Some(row.color),
        }));
    }

    Ok(ListResponse::new(folders).into_response())
}

#[utoipa::path(
    post,
    path = "/api/folders",
    tag = "mailboxes",
    request_body = CreateFolderRequest,
    responses(
        (status = 201, description = "Folder created", body = DataResponse<FolderResponse>),
        (status = 400, description = "Invalid folder"),
        (status = 409, description = "Folder already exists")
    )
)]
pub async fn create_folder(
    State(state): State<AppState>,
    principal: Principal,
    Json(body): Json<CreateFolderRequest>,
) -> Result<Response, ApiError> {
    let name = body.name.trim().to_string();
    if name.is_empty() {
        return Err(bad_request("Folder name must not be empty"));
    }
    let query = MessageQuery {
        mailbox: body.mailbox.clone(),
        mailbox_id: body.mailbox_id,
        ..MessageQuery::default()
    };
    let mailbox_id = resolve_mailbox(&state, &query, principal.mailbox_scope())
        .await?
        .ok_or_else(|| bad_request("A mailbox is required"))?;

    let existing = folder::Entity::find()
        .filter(folder::Column::MailboxId.eq(mailbox_id))
        .filter(folder::Column::Name.eq(name.clone()))
        .one(&state.db)
        .await
        .map_err(db_error)?;
    if existing.is_some() {
        return Err(conflict("Folder already exists"));
    }

    let row = folder::ActiveModel {
        id: Set(Uuid::now_v7()),
        mailbox_id: Set(mailbox_id),
        name: Set(name),
        color: Set(body.color.unwrap_or_else(|| "#2563eb".to_string())),
        created_at: Set(chrono::Utc::now()),
    }
    .insert(&state.db)
    .await
    .map_err(db_error)?;

    Ok((
        StatusCode::CREATED,
        Json(DataResponse {
            data: FolderResponse {
                id: row.id.to_string(),
                name: row.name,
                kind: "custom".to_string(),
                color: Some(row.color),
            },
        }),
    )
        .into_response())
}

#[utoipa::path(
    get,
    path = "/api/messages/navigation",
    tag = "messages",
    params(
        ("mailbox" = Option<String>, Query, description = "Mailbox address"),
        ("mailbox_id" = Option<Uuid>, Query, description = "Mailbox id"),
        ("folder" = Option<String>, Query, description = "Folder"),
        ("q" = Option<String>, Query, description = "Match subject and addresses"),
        ("read" = Option<String>, Query, description = "read or unread")
    ),
    responses((status = 200, description = "Ordered ids for the open filter", body = DataResponse<Vec<NavigationEntry>>))
)]
pub async fn message_navigation(
    State(state): State<AppState>,
    principal: Principal,
    Query(query): Query<MessageQuery>,
) -> Result<Response, ApiError> {
    let entries: Vec<NavigationEntry> =
        filtered_messages(&state, &query, principal.mailbox_scope())
            .await?
            .into_iter()
            .map(|(row, flag)| NavigationEntry {
                id: row.id,
                unread: !flag.as_ref().map(|flag| flag.read).unwrap_or(false),
            })
            .collect();
    Ok(Json(DataResponse { data: entries }).into_response())
}

#[utoipa::path(
    get,
    path = "/api/messages/{id}/thread",
    tag = "messages",
    params(("id" = Uuid, Path, description = "Message id")),
    responses(
        (status = 200, description = "The conversation", body = DataResponse<ThreadResponse>),
        (status = 404, description = "Unknown message")
    )
)]
pub async fn get_message_thread(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let target = repo::get_message(&state.db, id)
        .await
        .map_err(db_error)?
        .ok_or_else(|| not_found("Unknown message"))?;
    ensure_scope(&principal, target.mailbox_id, "Unknown message")?;

    let rows = match target.mailbox_id {
        Some(mailbox_id) => message::Entity::find()
            .filter(message::Column::MailboxId.eq(mailbox_id))
            .order_by_asc(message::Column::ReceivedAt)
            .limit(2000)
            .all(&state.db)
            .await
            .map_err(db_error)?,
        None => vec![target.clone()],
    };
    let root = thread_root(&target, &rows);
    let members: Vec<message::Model> = rows
        .into_iter()
        .filter(|row| in_thread(row, &root))
        .collect();

    let ids: Vec<Uuid> = members.iter().map(|row| row.id).collect();
    let flags = load_flags(&state, &ids).await?;

    let mut messages = Vec::with_capacity(members.len());
    for row in members {
        let flag = flags.iter().find(|flag| flag.message_id == row.id);
        let raw = state.store.get(&row.storage_key).await.map_err(internal)?;
        let parsed = body::extract(&raw);
        let attachments = parsed
            .attachments
            .iter()
            .enumerate()
            .map(|(index, attachment)| AttachmentResponse {
                index,
                filename: attachment.filename.clone(),
                content_type: attachment.content_type.clone(),
                size_bytes: attachment.bytes.len() as i64,
            })
            .collect();
        messages.push(ThreadMessageResponse {
            message: MessageResponse::with_flags(row, flag),
            text: parsed.text,
            html: parsed.html,
            attachments,
        });
    }

    Ok(Json(DataResponse {
        data: ThreadResponse {
            thread_id: root,
            messages,
        },
    })
    .into_response())
}

#[utoipa::path(
    post,
    path = "/api/messages/empty",
    tag = "messages",
    request_body = EmptyRequest,
    responses((status = 200, description = "How many messages were removed", body = DataResponse<EmptyResponse>))
)]
pub async fn empty_folder(
    State(state): State<AppState>,
    principal: Principal,
    Json(body): Json<EmptyRequest>,
) -> Result<Response, ApiError> {
    if body.folder != "trash" && body.folder != "spam" {
        return Err(bad_request("Only trash and spam can be emptied"));
    }

    let query = MessageQuery {
        mailbox: body.mailbox.clone(),
        mailbox_id: body.mailbox_id,
        folder: Some(body.folder.clone()),
        direction: None,
        ..MessageQuery::default()
    };

    let items = filtered_messages(&state, &query, principal.mailbox_scope()).await?;
    let mut deleted = 0usize;
    for (row, _) in &items {
        repo::delete_message(&state.db, row.id)
            .await
            .map_err(db_error)?;
        if let Err(err) = state.store.delete(&row.storage_key).await {
            tracing::warn!(error = %err, key = %row.storage_key, "failed to remove the stored object");
        }
        deleted += 1;
    }

    Ok(Json(DataResponse {
        data: EmptyResponse {
            deleted,
            remaining: 0,
        },
    })
    .into_response())
}

// ---------------------------------------------------------------------------
// Drafts
//
// A draft is stored as an ordinary message row whose reader flag puts it in the
// drafts folder, with the composed message itself in object storage. The
// composer rewrites that object in place as the draft changes.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, ToSchema)]
pub struct DraftRequest {
    pub mailbox: Option<String>,
    pub mailbox_id: Option<Uuid>,
    pub from: String,
    #[serde(default)]
    pub to: Option<String>,
    #[serde(default)]
    pub cc: Option<String>,
    #[serde(default)]
    pub bcc: Option<String>,
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub html: Option<String>,
    #[serde(default)]
    pub in_reply_to: Option<String>,
    #[serde(default)]
    pub references_header: Option<String>,
    #[serde(default)]
    pub thread_id: Option<String>,
    /// Start the draft from another message and copy its attachments.
    #[serde(default)]
    pub forward_of_message_id: Option<Uuid>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DraftResponse {
    pub id: Uuid,
    pub mailbox_id: Option<Uuid>,
    pub from: String,
    pub to: String,
    pub cc: String,
    pub bcc: String,
    pub subject: String,
    pub text: String,
    pub html: String,
    pub in_reply_to: Option<String>,
    pub references_header: Option<String>,
    pub thread_id: Option<String>,
    pub attachments: Vec<AttachmentResponse>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Splits a recipient header, keeping quoted display names intact.
fn split_addresses(value: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for ch in value.chars() {
        match ch {
            '"' => {
                quoted = !quoted;
                current.push(ch);
            }
            ',' if !quoted => {
                let entry = current.trim();
                if !entry.is_empty() {
                    out.push(entry.to_string());
                }
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    let entry = current.trim();
    if !entry.is_empty() {
        out.push(entry.to_string());
    }
    out
}

type OwnedAttachment = (String, String, Vec<u8>);

fn owned_attachments(raw: &[u8]) -> Vec<OwnedAttachment> {
    body::extract(raw)
        .attachments
        .into_iter()
        .map(|attachment| {
            (
                attachment
                    .filename
                    .unwrap_or_else(|| "attachment".to_string()),
                attachment
                    .content_type
                    .unwrap_or_else(|| "application/octet-stream".to_string()),
                attachment.bytes,
            )
        })
        .collect()
}

fn attachment_refs(owned: &[OwnedAttachment]) -> Vec<compose::Attachment<'_>> {
    owned
        .iter()
        .map(|(filename, content_type, bytes)| compose::Attachment {
            filename,
            content_type,
            bytes,
        })
        .collect()
}

fn attachment_responses(raw: &[u8]) -> Vec<AttachmentResponse> {
    body::extract(raw)
        .attachments
        .iter()
        .enumerate()
        .map(|(index, attachment)| AttachmentResponse {
            index,
            filename: attachment.filename.clone(),
            content_type: attachment.content_type.clone(),
            size_bytes: attachment.bytes.len() as i64,
        })
        .collect()
}

fn draft_response(row: &message::Model, raw: &[u8]) -> DraftResponse {
    let parsed = body::extract(raw);
    DraftResponse {
        id: row.id,
        mailbox_id: row.mailbox_id,
        from: row.envelope_from.clone(),
        to: parsed.to.unwrap_or_default(),
        cc: parsed.cc.unwrap_or_default(),
        bcc: parsed.bcc.unwrap_or_default(),
        subject: row.subject.clone().unwrap_or_default(),
        text: parsed.text.unwrap_or_default(),
        html: parsed.html.unwrap_or_default(),
        in_reply_to: row.in_reply_to.clone(),
        references_header: row.references_header.clone(),
        thread_id: row.message_id.clone(),
        attachments: attachment_responses(raw),
        updated_at: row.received_at,
    }
}

async fn load_draft(
    state: &AppState,
    principal: &Principal,
    id: Uuid,
) -> Result<(message::Model, Vec<u8>), ApiError> {
    let row = repo::get_message(&state.db, id)
        .await
        .map_err(db_error)?
        .ok_or_else(|| not_found("Unknown draft"))?;
    ensure_scope(principal, row.mailbox_id, "Unknown draft")?;
    let raw = state.store.get(&row.storage_key).await.map_err(internal)?;
    Ok((row, raw))
}

/// The editable parts of a draft, as sent by the composer.
struct DraftFields<'a> {
    from: &'a str,
    to: &'a str,
    cc: &'a str,
    bcc: &'a str,
    subject: Option<&'a str>,
    text: Option<&'a str>,
    html: Option<&'a str>,
    in_reply_to: Option<&'a str>,
    references: Option<&'a str>,
    message_id: Option<&'a str>,
}

/// Rebuilds and stores a draft object, keeping the row in step.
async fn write_draft(
    state: &AppState,
    principal: &Principal,
    id: Uuid,
    fields: DraftFields<'_>,
    attachments: &[OwnedAttachment],
) -> Result<Vec<u8>, ApiError> {
    let refs = attachment_refs(attachments);
    let raw = compose::build(&compose::Compose {
        from: fields.from,
        to: &split_addresses(fields.to),
        cc: &split_addresses(fields.cc),
        bcc: &split_addresses(fields.bcc),
        subject: fields.subject.unwrap_or_default(),
        text: fields.text,
        html: fields.html,
        attachments: &refs,
        message_id: fields.message_id,
        in_reply_to: fields.in_reply_to,
        references: fields.references,
    })
    .map_err(internal)?;

    let row = repo::get_message(&state.db, id)
        .await
        .map_err(db_error)?
        .ok_or_else(|| not_found("Unknown draft"))?;
    ensure_scope(principal, row.mailbox_id, "Unknown draft")?;
    state
        .store
        .put(&row.storage_key, raw.clone())
        .await
        .map_err(internal)?;

    let mut active: message::ActiveModel = row.into();
    active.envelope_from = Set(fields.from.to_string());
    active.envelope_to = Set(fields.to.to_string());
    active.subject = Set(fields.subject.map(str::to_string));
    active.in_reply_to = Set(fields.in_reply_to.map(str::to_string));
    active.references_header = Set(fields.references.map(str::to_string));
    active.size_bytes = Set(raw.len() as i64);
    active.received_at = Set(chrono::Utc::now());
    active.update(&state.db).await.map_err(db_error)?;

    // Drafts live in the drafts folder and never count as unread.
    upsert_flag(state, id, Some(true), None, Some("drafts".to_string())).await?;
    Ok(raw)
}

#[utoipa::path(
    post,
    path = "/api/drafts",
    tag = "messages",
    request_body = DraftRequest,
    responses((status = 201, description = "Draft created", body = DataResponse<DraftResponse>))
)]
pub async fn create_draft(
    State(state): State<AppState>,
    principal: Principal,
    Json(body): Json<DraftRequest>,
) -> Result<Response, ApiError> {
    let mailbox_id = resolve_mailbox(
        &state,
        &MessageQuery {
            mailbox: body.mailbox.clone(),
            mailbox_id: body.mailbox_id,
            ..MessageQuery::default()
        },
        principal.mailbox_scope(),
    )
    .await?;

    // Forwarding carries the original's attachments onto the new draft.
    let forwarded = match body.forward_of_message_id {
        Some(source_id) => {
            let source = repo::get_message(&state.db, source_id)
                .await
                .map_err(db_error)?
                .ok_or_else(|| not_found("Unknown message"))?;
            ensure_scope(&principal, source.mailbox_id, "Unknown message")?;
            let source_raw = state
                .store
                .get(&source.storage_key)
                .await
                .map_err(internal)?;
            owned_attachments(&source_raw)
        }
        None => Vec::new(),
    };
    let forwarded_refs = attachment_refs(&forwarded);

    let id = Uuid::now_v7();
    let now = chrono::Utc::now();
    let key = store::draft_key(&id, now);
    let raw = compose::build(&compose::Compose {
        from: &body.from,
        to: &split_addresses(body.to.as_deref().unwrap_or_default()),
        cc: &split_addresses(body.cc.as_deref().unwrap_or_default()),
        bcc: &split_addresses(body.bcc.as_deref().unwrap_or_default()),
        subject: body.subject.as_deref().unwrap_or_default(),
        text: body.text.as_deref(),
        html: body.html.as_deref(),
        attachments: &forwarded_refs,
        message_id: None,
        in_reply_to: body.in_reply_to.as_deref(),
        references: body.references_header.as_deref(),
    })
    .map_err(internal)?;
    state.store.put(&key, raw.clone()).await.map_err(internal)?;

    let row = repo::insert_message(
        &state.db,
        message::ActiveModel {
            id: Set(id),
            mailbox_id: Set(mailbox_id),
            direction: Set(message::Direction::Outbound),
            status: Set(message::Status::Stored),
            envelope_from: Set(body.from.clone()),
            envelope_to: Set(body.to.clone().unwrap_or_default()),
            message_id: Set(body.thread_id.clone()),
            subject: Set(body.subject.clone()),
            header_from: Set(Some(body.from.clone())),
            in_reply_to: Set(body.in_reply_to.clone()),
            references_header: Set(body.references_header.clone()),
            size_bytes: Set(raw.len() as i64),
            storage_key: Set(key),
            spf_result: Set(None),
            dkim_result: Set(None),
            dmarc_result: Set(None),
            dmarc_policy: Set(None),
            received_at: Set(now),
        },
    )
    .await
    .map_err(db_error)?;

    upsert_flag(&state, id, Some(true), None, Some("drafts".to_string())).await?;
    Ok((
        StatusCode::CREATED,
        Json(DataResponse {
            data: draft_response(&row, &raw),
        }),
    )
        .into_response())
}

#[utoipa::path(
    get,
    path = "/api/drafts/{id}",
    tag = "messages",
    params(("id" = Uuid, Path, description = "Draft id")),
    responses(
        (status = 200, description = "The draft", body = DataResponse<DraftResponse>),
        (status = 404, description = "Unknown draft")
    )
)]
pub async fn get_draft(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let (row, raw) = load_draft(&state, &principal, id).await?;
    Ok(Json(DataResponse {
        data: draft_response(&row, &raw),
    })
    .into_response())
}

#[utoipa::path(
    patch,
    path = "/api/drafts/{id}",
    tag = "messages",
    request_body = DraftRequest,
    params(("id" = Uuid, Path, description = "Draft id")),
    responses((status = 200, description = "Updated draft", body = DataResponse<DraftResponse>))
)]
pub async fn update_draft(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<Uuid>,
    Json(body): Json<DraftRequest>,
) -> Result<Response, ApiError> {
    let (row, current) = load_draft(&state, &principal, id).await?;
    // A body change keeps whatever the composer already attached.
    let attachments = owned_attachments(&current);
    let raw = write_draft(
        &state,
        &principal,
        id,
        DraftFields {
            from: &body.from,
            to: body.to.as_deref().unwrap_or_default(),
            cc: body.cc.as_deref().unwrap_or_default(),
            bcc: body.bcc.as_deref().unwrap_or_default(),
            subject: body.subject.as_deref(),
            text: body.text.as_deref(),
            html: body.html.as_deref(),
            in_reply_to: body.in_reply_to.as_deref(),
            references: body.references_header.as_deref(),
            message_id: row.message_id.as_deref(),
        },
        &attachments,
    )
    .await?;
    let updated = repo::get_message(&state.db, id)
        .await
        .map_err(db_error)?
        .ok_or_else(|| not_found("Unknown draft"))?;
    Ok(Json(DataResponse {
        data: draft_response(&updated, &raw),
    })
    .into_response())
}

#[utoipa::path(
    delete,
    path = "/api/drafts/{id}",
    tag = "messages",
    params(("id" = Uuid, Path, description = "Draft id")),
    responses((status = 204, description = "Deleted"))
)]
pub async fn delete_draft(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let row = repo::get_message(&state.db, id)
        .await
        .map_err(db_error)?
        .ok_or_else(|| not_found("Unknown draft"))?;
    ensure_scope(&principal, row.mailbox_id, "Unknown draft")?;
    repo::delete_message(&state.db, id)
        .await
        .map_err(db_error)?;
    if let Err(err) = state.store.delete(&row.storage_key).await {
        tracing::warn!(error = %err, key = %row.storage_key, "failed to remove the draft object");
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[utoipa::path(
    post,
    path = "/api/drafts/{id}/attachments",
    tag = "messages",
    params(("id" = Uuid, Path, description = "Draft id")),
    responses((status = 200, description = "The draft's attachments", body = DataResponse<Vec<AttachmentResponse>>))
)]
pub async fn upload_draft_attachments(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<Uuid>,
    mut multipart: Multipart,
) -> Result<Response, ApiError> {
    let (row, current) = load_draft(&state, &principal, id).await?;
    let mut attachments = owned_attachments(&current);

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|err| bad_request(err.to_string()))?
    {
        if field.name() != Some("attachments") {
            continue;
        }
        let filename = field.file_name().unwrap_or("attachment").to_string();
        let content_type = field
            .content_type()
            .unwrap_or("application/octet-stream")
            .to_string();
        let bytes = field
            .bytes()
            .await
            .map_err(|err| bad_request(err.to_string()))?
            .to_vec();
        attachments.push((filename, content_type, bytes));
    }

    let parsed = body::extract(&current);
    let raw = write_draft(
        &state,
        &principal,
        id,
        DraftFields {
            from: &row.envelope_from,
            to: parsed.to.as_deref().unwrap_or_default(),
            cc: parsed.cc.as_deref().unwrap_or_default(),
            bcc: parsed.bcc.as_deref().unwrap_or_default(),
            subject: row.subject.as_deref(),
            text: parsed.text.as_deref(),
            html: parsed.html.as_deref(),
            in_reply_to: row.in_reply_to.as_deref(),
            references: row.references_header.as_deref(),
            message_id: row.message_id.as_deref(),
        },
        &attachments,
    )
    .await?;

    Ok(Json(DataResponse {
        data: attachment_responses(&raw),
    })
    .into_response())
}

#[utoipa::path(
    delete,
    path = "/api/drafts/{id}/attachments/{index}",
    tag = "messages",
    params(
        ("id" = Uuid, Path, description = "Draft id"),
        ("index" = usize, Path, description = "Attachment position")
    ),
    responses((status = 200, description = "The remaining attachments", body = DataResponse<Vec<AttachmentResponse>>))
)]
pub async fn delete_draft_attachment(
    State(state): State<AppState>,
    principal: Principal,
    Path((id, index)): Path<(Uuid, usize)>,
) -> Result<Response, ApiError> {
    let (row, current) = load_draft(&state, &principal, id).await?;
    let mut attachments = owned_attachments(&current);
    if index >= attachments.len() {
        return Err(not_found("Unknown attachment"));
    }
    attachments.remove(index);

    let parsed = body::extract(&current);
    let raw = write_draft(
        &state,
        &principal,
        id,
        DraftFields {
            from: &row.envelope_from,
            to: parsed.to.as_deref().unwrap_or_default(),
            cc: parsed.cc.as_deref().unwrap_or_default(),
            bcc: parsed.bcc.as_deref().unwrap_or_default(),
            subject: row.subject.as_deref(),
            text: parsed.text.as_deref(),
            html: parsed.html.as_deref(),
            in_reply_to: row.in_reply_to.as_deref(),
            references: row.references_header.as_deref(),
            message_id: row.message_id.as_deref(),
        },
        &attachments,
    )
    .await?;

    Ok(Json(DataResponse {
        data: attachment_responses(&raw),
    })
    .into_response())
}

// ---------------------------------------------------------------------------
// Templates and client preferences
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
pub struct TemplateResponse {
    pub id: Uuid,
    pub mailbox_id: Option<Uuid>,
    pub from: String,
    /// The client calls this the template's subject.
    pub title: String,
    pub text: String,
    pub html: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateTemplateRequest {
    pub mailbox: Option<String>,
    pub mailbox_id: Option<Uuid>,
    pub from: String,
    pub title: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub html: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SettingResponse {
    pub enabled: bool,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SettingRequest {
    pub enabled: bool,
}

const SETTINGS_SCOPE: &str = "default";

async fn settings_row(state: &AppState, scope: &str) -> Result<setting::Model, ApiError> {
    if let Some(row) = setting::Entity::find()
        .filter(setting::Column::Scope.eq(scope))
        .one(&state.db)
        .await
        .map_err(db_error)?
    {
        return Ok(row);
    }
    setting::ActiveModel {
        id: Set(Uuid::now_v7()),
        scope: Set(scope.to_string()),
        keyboard_shortcuts_enabled: Set(true),
        show_full_recipient_addresses: Set(false),
        updated_at: Set(chrono::Utc::now()),
    }
    .insert(&state.db)
    .await
    .map_err(db_error)
}

async fn update_settings(
    state: &AppState,
    scope: &str,
    shortcuts: Option<bool>,
    recipient_addresses: Option<bool>,
) -> Result<setting::Model, ApiError> {
    let row = settings_row(state, scope).await?;
    let mut active: setting::ActiveModel = row.into();
    if let Some(enabled) = shortcuts {
        active.keyboard_shortcuts_enabled = Set(enabled);
    }
    if let Some(enabled) = recipient_addresses {
        active.show_full_recipient_addresses = Set(enabled);
    }
    active.updated_at = Set(chrono::Utc::now());
    active.update(&state.db).await.map_err(db_error)
}

fn template_response(row: template::Model) -> TemplateResponse {
    TemplateResponse {
        id: row.id,
        mailbox_id: row.mailbox_id,
        from: row.from_addr,
        title: row.title,
        text: row.text_body,
        html: row.html_body,
        created_at: row.created_at,
    }
}

#[utoipa::path(
    get,
    path = "/api/templates",
    tag = "send",
    params(("mailbox" = Option<String>, Query, description = "Limit to one mailbox")),
    responses((status = 200, description = "Composer templates", body = ListResponse<TemplateResponse>))
)]
pub async fn list_templates(
    State(state): State<AppState>,
    principal: Principal,
    Query(query): Query<MessageQuery>,
) -> Result<Response, ApiError> {
    let mut request = template::Entity::find().order_by_asc(template::Column::Title);
    if let Some(mailbox_id) = resolve_mailbox(&state, &query, principal.mailbox_scope()).await? {
        request = request.filter(template::Column::MailboxId.eq(mailbox_id));
    }
    let rows = request.all(&state.db).await.map_err(db_error)?;
    let payload = rows.into_iter().map(template_response).collect();
    Ok(ListResponse::new(payload).into_response())
}

#[utoipa::path(
    post,
    path = "/api/templates",
    tag = "send",
    request_body = CreateTemplateRequest,
    responses((status = 201, description = "Template created", body = DataResponse<TemplateResponse>))
)]
pub async fn create_template(
    State(state): State<AppState>,
    principal: Principal,
    Json(body): Json<CreateTemplateRequest>,
) -> Result<Response, ApiError> {
    let title = body.title.trim().to_string();
    if title.is_empty() {
        return Err(bad_request("A template title is required"));
    }
    let mailbox_id = resolve_mailbox(
        &state,
        &MessageQuery {
            mailbox: body.mailbox.clone(),
            mailbox_id: body.mailbox_id,
            ..MessageQuery::default()
        },
        principal.mailbox_scope(),
    )
    .await?;

    let row = template::ActiveModel {
        id: Set(Uuid::now_v7()),
        mailbox_id: Set(mailbox_id),
        from_addr: Set(body.from.clone()),
        title: Set(title),
        text_body: Set(body.text.clone().unwrap_or_default()),
        html_body: Set(body.html.clone().unwrap_or_default()),
        created_at: Set(chrono::Utc::now()),
    }
    .insert(&state.db)
    .await
    .map_err(db_error)?;

    Ok((
        StatusCode::CREATED,
        Json(DataResponse {
            data: template_response(row),
        }),
    )
        .into_response())
}

#[utoipa::path(
    get,
    path = "/api/templates/{id}",
    tag = "send",
    params(("id" = Uuid, Path, description = "Template id")),
    responses(
        (status = 200, description = "A template", body = DataResponse<TemplateResponse>),
        (status = 404, description = "Unknown template")
    )
)]
pub async fn get_template(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let row = template::Entity::find_by_id(id)
        .one(&state.db)
        .await
        .map_err(db_error)?
        .ok_or_else(|| not_found("Unknown template"))?;
    ensure_scope(&principal, row.mailbox_id, "Unknown template")?;
    Ok(Json(DataResponse {
        data: template_response(row),
    })
    .into_response())
}

#[utoipa::path(
    delete,
    path = "/api/templates/{id}",
    tag = "send",
    params(("id" = Uuid, Path, description = "Template id")),
    responses((status = 204, description = "Deleted"))
)]
pub async fn delete_template(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let row = template::Entity::find_by_id(id)
        .one(&state.db)
        .await
        .map_err(db_error)?
        .ok_or_else(|| not_found("Unknown template"))?;
    ensure_scope(&principal, row.mailbox_id, "Unknown template")?;
    template::Entity::delete_by_id(id)
        .exec(&state.db)
        .await
        .map_err(db_error)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[utoipa::path(
    get,
    path = "/api/settings/shortcuts",
    tag = "send",
    responses((status = 200, description = "Whether keyboard shortcuts are on", body = DataResponse<SettingResponse>))
)]
pub async fn get_shortcuts_setting(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Response, ApiError> {
    let row = settings_row(&state, settings_scope(&principal)).await?;
    Ok(Json(DataResponse {
        data: SettingResponse {
            enabled: row.keyboard_shortcuts_enabled,
        },
    })
    .into_response())
}

#[utoipa::path(
    patch,
    path = "/api/settings/shortcuts",
    tag = "send",
    request_body = SettingRequest,
    responses((status = 200, description = "Updated setting", body = DataResponse<SettingResponse>))
)]
pub async fn update_shortcuts_setting(
    State(state): State<AppState>,
    principal: Principal,
    Json(body): Json<SettingRequest>,
) -> Result<Response, ApiError> {
    let row = update_settings(&state, settings_scope(&principal), Some(body.enabled), None).await?;
    Ok(Json(DataResponse {
        data: SettingResponse {
            enabled: row.keyboard_shortcuts_enabled,
        },
    })
    .into_response())
}

#[utoipa::path(
    get,
    path = "/api/settings/recipient-addresses",
    tag = "send",
    responses((status = 200, description = "Whether full recipient addresses are shown", body = DataResponse<SettingResponse>))
)]
pub async fn get_recipient_addresses_setting(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Response, ApiError> {
    let row = settings_row(&state, settings_scope(&principal)).await?;
    Ok(Json(DataResponse {
        data: SettingResponse {
            enabled: row.show_full_recipient_addresses,
        },
    })
    .into_response())
}

#[utoipa::path(
    patch,
    path = "/api/settings/recipient-addresses",
    tag = "send",
    request_body = SettingRequest,
    responses((status = 200, description = "Updated setting", body = DataResponse<SettingResponse>))
)]
pub async fn update_recipient_addresses_setting(
    State(state): State<AppState>,
    principal: Principal,
    Json(body): Json<SettingRequest>,
) -> Result<Response, ApiError> {
    let row = update_settings(&state, settings_scope(&principal), None, Some(body.enabled)).await?;
    Ok(Json(DataResponse {
        data: SettingResponse {
            enabled: row.show_full_recipient_addresses,
        },
    })
    .into_response())
}

// ---------------------------------------------------------------------------
// Contacts
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
pub struct ContactResponse {
    pub email: String,
    pub display_name: Option<String>,
    pub has_avatar: bool,
    /// manual, inbound or outbound.
    pub source: String,
    pub blocked: bool,
    pub last_seen_at: chrono::DateTime<chrono::Utc>,
}

fn contact_response(row: &contact::Model) -> ContactResponse {
    ContactResponse {
        email: row.address.clone(),
        display_name: row.display_name.clone(),
        has_avatar: row.avatar_key.is_some(),
        source: row.source.clone(),
        blocked: row.blocked,
        last_seen_at: row.last_seen_at,
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ContactQuery {
    pub mailbox: Option<String>,
    pub mailbox_id: Option<Uuid>,
    pub address: String,
    /// "preview" returns the small rendition.
    pub variant: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateContactRequest {
    pub mailbox: Option<String>,
    pub mailbox_id: Option<Uuid>,
    pub address: String,
    pub display_name: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct BlockContactRequest {
    pub mailbox: Option<String>,
    pub mailbox_id: Option<Uuid>,
    pub address: String,
}

async fn contact_mailbox(
    state: &AppState,
    mailbox: &Option<String>,
    mailbox_id: Option<Uuid>,
    scope: Option<Uuid>,
) -> Result<Uuid, ApiError> {
    resolve_mailbox(
        state,
        &MessageQuery {
            mailbox: mailbox.clone(),
            mailbox_id,
            ..MessageQuery::default()
        },
        scope,
    )
    .await?
    .ok_or_else(|| bad_request("A mailbox is required"))
}

async fn find_contact(
    state: &AppState,
    mailbox_id: Uuid,
    address: &str,
) -> Result<Option<contact::Model>, ApiError> {
    contact::Entity::find()
        .filter(contact::Column::MailboxId.eq(mailbox_id))
        .filter(contact::Column::Address.eq(address.to_ascii_lowercase()))
        .one(&state.db)
        .await
        .map_err(db_error)
}

async fn upsert_contact(
    state: &AppState,
    mailbox_id: Uuid,
    address: &str,
    display_name: Option<Option<String>>,
    blocked: Option<bool>,
    source: Option<&str>,
) -> Result<contact::Model, ApiError> {
    let address = address.trim().to_ascii_lowercase();
    if address::parse(&address).is_none() {
        return Err(bad_request("Invalid email address"));
    }
    let existing = find_contact(state, mailbox_id, &address).await?;
    let now = chrono::Utc::now();
    match existing {
        Some(row) => {
            let mut active: contact::ActiveModel = row.into();
            if let Some(display_name) = display_name {
                active.display_name = Set(display_name);
            }
            if let Some(blocked) = blocked {
                active.blocked = Set(blocked);
            }
            if let Some(source) = source {
                active.source = Set(source.to_string());
            }
            active.last_seen_at = Set(now);
            active.updated_at = Set(now);
            active.update(&state.db).await.map_err(db_error)
        }
        None => contact::ActiveModel {
            id: Set(Uuid::now_v7()),
            mailbox_id: Set(mailbox_id),
            address: Set(address),
            display_name: Set(display_name.flatten()),
            avatar_key: Set(None),
            preview_key: Set(None),
            avatar_content_type: Set(None),
            blocked: Set(blocked.unwrap_or(false)),
            source: Set(source.unwrap_or("manual").to_string()),
            last_seen_at: Set(now),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(&state.db)
        .await
        .map_err(db_error),
    }
}

#[utoipa::path(
    get,
    path = "/api/contacts",
    tag = "mailboxes",
    params(
        ("mailbox" = Option<String>, Query, description = "Mailbox address"),
        ("mailbox_id" = Option<Uuid>, Query, description = "Mailbox id"),
        ("address" = String, Query, description = "Contact email address")
    ),
    responses(
        (status = 200, description = "The contact", body = DataResponse<ContactResponse>),
        (status = 404, description = "Unknown contact")
    )
)]
pub async fn get_contact(
    State(state): State<AppState>,
    principal: Principal,
    Query(query): Query<ContactQuery>,
) -> Result<Response, ApiError> {
    let mailbox_id = contact_mailbox(
        &state,
        &query.mailbox,
        query.mailbox_id,
        principal.mailbox_scope(),
    )
    .await?;
    let row = find_contact(&state, mailbox_id, &query.address)
        .await?
        .ok_or_else(|| not_found("Unknown contact"))?;
    Ok(Json(DataResponse {
        data: contact_response(&row),
    })
    .into_response())
}

#[utoipa::path(
    patch,
    path = "/api/contacts",
    tag = "mailboxes",
    request_body = UpdateContactRequest,
    responses((status = 200, description = "Updated contact", body = DataResponse<ContactResponse>))
)]
pub async fn update_contact(
    State(state): State<AppState>,
    principal: Principal,
    Json(body): Json<UpdateContactRequest>,
) -> Result<Response, ApiError> {
    let mailbox_id = contact_mailbox(
        &state,
        &body.mailbox,
        body.mailbox_id,
        principal.mailbox_scope(),
    )
    .await?;
    let row = upsert_contact(
        &state,
        mailbox_id,
        &body.address,
        Some(body.display_name),
        None,
        Some("manual"),
    )
    .await?;
    Ok(Json(DataResponse {
        data: contact_response(&row),
    })
    .into_response())
}

#[utoipa::path(
    post,
    path = "/api/contacts/block",
    tag = "mailboxes",
    request_body = BlockContactRequest,
    responses((status = 200, description = "Blocked contact", body = DataResponse<ContactResponse>))
)]
pub async fn block_contact(
    State(state): State<AppState>,
    principal: Principal,
    Json(body): Json<BlockContactRequest>,
) -> Result<Response, ApiError> {
    let mailbox_id = contact_mailbox(
        &state,
        &body.mailbox,
        body.mailbox_id,
        principal.mailbox_scope(),
    )
    .await?;
    let row = upsert_contact(&state, mailbox_id, &body.address, None, Some(true), None).await?;
    Ok(Json(DataResponse {
        data: contact_response(&row),
    })
    .into_response())
}

#[utoipa::path(
    post,
    path = "/api/contacts/avatar",
    tag = "mailboxes",
    responses((status = 200, description = "Stored contact picture", body = DataResponse<ContactResponse>))
)]
pub async fn upload_contact_avatar(
    State(state): State<AppState>,
    principal: Principal,
    mut multipart: Multipart,
) -> Result<Response, ApiError> {
    let mut mailbox_id = None;
    let mut mailbox_address = None;
    let mut address = None;
    let mut full: Option<(Vec<u8>, String)> = None;
    let mut preview: Option<(Vec<u8>, String)> = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|err| bad_request(err.to_string()))?
    {
        let name = field.name().unwrap_or_default().to_string();
        match name.as_str() {
            "mailbox_id" => mailbox_id = field.text().await.ok().and_then(|v| v.parse().ok()),
            "mailbox" => mailbox_address = field.text().await.ok(),
            "address" => address = field.text().await.ok(),
            "file" => {
                let content_type = field.content_type().unwrap_or("image/webp").to_string();
                let bytes = field
                    .bytes()
                    .await
                    .map_err(|err| bad_request(err.to_string()))?
                    .to_vec();
                full = Some((bytes, content_type));
            }
            "preview" => {
                let content_type = field.content_type().unwrap_or("image/webp").to_string();
                let bytes = field
                    .bytes()
                    .await
                    .map_err(|err| bad_request(err.to_string()))?
                    .to_vec();
                preview = Some((bytes, content_type));
            }
            _ => {}
        }
    }

    let mailbox_id = contact_mailbox(
        &state,
        &mailbox_address,
        mailbox_id,
        principal.mailbox_scope(),
    )
    .await?;
    let address = address.ok_or_else(|| bad_request("An address is required"))?;
    let (bytes, content_type) = full.ok_or_else(|| bad_request("A file is required"))?;

    let row = upsert_contact(&state, mailbox_id, &address, None, None, None).await?;
    let full_key = format!("contacts/{}/{}/avatar", mailbox_id, row.id);
    state.store.put(&full_key, bytes).await.map_err(internal)?;

    let preview_key = match preview {
        Some((bytes, _)) => {
            let key = format!("contacts/{}/{}/preview", mailbox_id, row.id);
            state.store.put(&key, bytes).await.map_err(internal)?;
            Some(key)
        }
        None => None,
    };

    let mut active: contact::ActiveModel = row.into();
    active.avatar_key = Set(Some(full_key));
    active.preview_key = Set(preview_key);
    active.avatar_content_type = Set(Some(content_type));
    active.updated_at = Set(chrono::Utc::now());
    let updated = active.update(&state.db).await.map_err(db_error)?;

    Ok(Json(DataResponse {
        data: contact_response(&updated),
    })
    .into_response())
}

#[utoipa::path(
    delete,
    path = "/api/contacts/avatar",
    tag = "mailboxes",
    params(
        ("mailbox" = Option<String>, Query, description = "Mailbox address"),
        ("mailbox_id" = Option<Uuid>, Query, description = "Mailbox id"),
        ("address" = String, Query, description = "Contact email address")
    ),
    responses((status = 200, description = "Picture removed", body = DataResponse<ContactResponse>))
)]
pub async fn delete_contact_avatar(
    State(state): State<AppState>,
    principal: Principal,
    Query(query): Query<ContactQuery>,
) -> Result<Response, ApiError> {
    let mailbox_id = contact_mailbox(
        &state,
        &query.mailbox,
        query.mailbox_id,
        principal.mailbox_scope(),
    )
    .await?;
    let row = find_contact(&state, mailbox_id, &query.address)
        .await?
        .ok_or_else(|| not_found("Unknown contact"))?;

    for key in [row.avatar_key.clone(), row.preview_key.clone()]
        .into_iter()
        .flatten()
    {
        if let Err(err) = state.store.delete(&key).await {
            tracing::warn!(error = %err, key = %key, "failed to remove the contact picture");
        }
    }

    let mut active: contact::ActiveModel = row.into();
    active.avatar_key = Set(None);
    active.preview_key = Set(None);
    active.avatar_content_type = Set(None);
    active.updated_at = Set(chrono::Utc::now());
    let updated = active.update(&state.db).await.map_err(db_error)?;

    Ok(Json(DataResponse {
        data: contact_response(&updated),
    })
    .into_response())
}

#[utoipa::path(
    get,
    path = "/api/contacts/avatar",
    tag = "mailboxes",
    params(
        ("mailbox" = Option<String>, Query, description = "Mailbox address"),
        ("mailbox_id" = Option<Uuid>, Query, description = "Mailbox id"),
        ("address" = String, Query, description = "Contact email address"),
        ("variant" = Option<String>, Query, description = "preview for the small rendition")
    ),
    responses(
        (status = 200, description = "Contact picture"),
        (status = 404, description = "No picture")
    )
)]
pub async fn get_contact_avatar(
    State(state): State<AppState>,
    principal: Principal,
    Query(query): Query<ContactQuery>,
) -> Result<Response, ApiError> {
    let mailbox_id = contact_mailbox(
        &state,
        &query.mailbox,
        query.mailbox_id,
        principal.mailbox_scope(),
    )
    .await?;
    let row = find_contact(&state, mailbox_id, &query.address)
        .await?
        .ok_or_else(|| not_found("Unknown contact"))?;

    let key = if query.variant.as_deref() == Some("preview") {
        row.preview_key.clone().or_else(|| row.avatar_key.clone())
    } else {
        row.avatar_key.clone()
    }
    .ok_or_else(|| not_found("No picture"))?;

    let bytes = state.store.get(&key).await.map_err(internal)?;
    Ok(Response::builder()
        .header(
            header::CONTENT_TYPE,
            row.avatar_content_type
                .unwrap_or_else(|| "image/webp".to_string()),
        )
        .header(header::CACHE_CONTROL, "private, max-age=300")
        .body(Body::from(bytes))
        .expect("valid response"))
}

// ---------------------------------------------------------------------------
// Live updates
// ---------------------------------------------------------------------------

/// Upgrades to a WebSocket that pushes a frame whenever mail is stored.
///
/// A mailbox session only receives frames for its own mailbox. The API key (or
/// the session cookie) is taken from the headers like every other route.
#[utoipa::path(
    get,
    path = "/api/realtime",
    tag = "messages",
    responses((status = 101, description = "WebSocket upgrade"))
)]
pub async fn realtime(
    State(state): State<AppState>,
    principal: Principal,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    let scope = principal.mailbox_scope();
    Ok(ws.on_upgrade(move |socket| serve_realtime(socket, state, scope)))
}

async fn serve_realtime(mut socket: WebSocket, state: AppState, scope: Option<Uuid>) {
    let mut events = state.events.subscribe();
    loop {
        tokio::select! {
            received = events.recv() => match received {
                Ok(frame) => {
                    if let Some(mailbox_id) = scope
                        && !frame_belongs_to(&frame, mailbox_id)
                    {
                        continue;
                    }
                    if socket.send(Message::Text(frame.into())).await.is_err() {
                        break;
                    }
                }
                // A slow reader missed frames; the next one still lands.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    if text.contains("ping")
                        && socket.send(Message::Text("pong".into())).await.is_err()
                    {
                        break;
                    }
                }
                Some(Ok(Message::Close(_))) | None => break,
                Some(Err(_)) => break,
                _ => {}
            },
        }
    }
}

/// True when a live-update frame is addressed to the given mailbox.
fn frame_belongs_to(frame: &str, mailbox_id: Uuid) -> bool {
    serde_json::from_str::<serde_json::Value>(frame)
        .ok()
        .and_then(|value| {
            value
                .get("mailboxId")
                .and_then(|id| id.as_str())
                .map(str::to_string)
        })
        .is_some_and(|id| id == mailbox_id.to_string())
}

#[utoipa::path(
    get,
    path = "/api/messages/{id}/metadata",
    tag = "messages",
    params(("id" = Uuid, Path, description = "Message id")),
    responses(
        (status = 200, description = "Supplemental message metadata", body = DataResponse<MessageMetadataResponse>),
        (status = 404, description = "Unknown message")
    )
)]
pub async fn get_message_metadata(
    State(state): State<AppState>,
    _principal: Principal,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let message = repo::get_message(&state.db, id)
        .await
        .map_err(db_error)?
        .ok_or_else(|| not_found("Unknown message"))?;
    let raw = state
        .store
        .get(&message.storage_key)
        .await
        .map_err(internal)?;
    let parsed = body::extract(&raw);
    let attachments = parsed
        .attachments
        .iter()
        .enumerate()
        .map(|(index, attachment)| AttachmentResponse {
            index,
            filename: attachment.filename.clone(),
            content_type: attachment.content_type.clone(),
            size_bytes: attachment.bytes.len() as i64,
        })
        .collect();

    Ok(Json(DataResponse {
        data: MessageMetadataResponse {
            attachments,
            unsubscribe_url: parsed.unsubscribe_url,
        },
    })
    .into_response())
}

#[utoipa::path(
    post,
    path = "/api/messages/{id}/cancel-schedule",
    tag = "messages",
    params(("id" = Uuid, Path, description = "Scheduled message id")),
    responses(
        (status = 200, description = "Cancelled; the copy becomes a draft", body = DataResponse<MessageResponse>),
        (status = 404, description = "Unknown message")
    )
)]
pub async fn cancel_scheduled_send(
    State(state): State<AppState>,
    _principal: Principal,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let message = repo::get_message(&state.db, id)
        .await
        .map_err(db_error)?
        .ok_or_else(|| not_found("Unknown message"))?;

    // Drop the queued delivery, then hand the message back to the composer.
    let queued = outbound_job::Entity::find()
        .filter(outbound_job::Column::ScheduledMessageId.eq(id))
        .filter(outbound_job::Column::Status.eq(outbound_job::JobStatus::Pending))
        .all(&state.db)
        .await
        .map_err(db_error)?;
    for job in queued {
        outbound_job::Entity::delete_by_id(job.id)
            .exec(&state.db)
            .await
            .map_err(db_error)?;
    }

    let flag = upsert_flag(&state, id, None, None, Some("drafts".to_string())).await?;
    Ok(Json(DataResponse {
        data: MessageResponse::with_flags(message, Some(&flag)),
    })
    .into_response())
}

// ---------------------------------------------------------------------------
// Mailboxes
// ---------------------------------------------------------------------------

#[utoipa::path(
    get,
    path = "/api/mailboxes",
    tag = "mailboxes",
    responses((status = 200, description = "Mailboxes", body = ListResponse<MailboxResponse>))
)]
pub async fn list_mailboxes(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Response, ApiError> {
    // A session sees only its own mailbox; the operator sees every tenant's.
    let mailboxes = match principal.scope() {
        Some(scope) => repo::find_mailbox(&state.db, &scope.address)
            .await
            .map_err(db_error)?
            .into_iter()
            .collect(),
        None => repo::list_mailboxes(&state.db).await.map_err(db_error)?,
    };
    let payload = mailboxes.into_iter().map(MailboxResponse::from).collect();
    Ok(ListResponse::new(payload).into_response())
}

#[utoipa::path(
    post,
    path = "/api/mailboxes",
    tag = "mailboxes",
    request_body = CreateMailbox,
    responses(
        (status = 201, description = "Mailbox created", body = MailboxResponse),
        (status = 400, description = "Invalid address"),
        (status = 409, description = "Mailbox already exists")
    )
)]
pub async fn create_mailbox(
    State(state): State<AppState>,
    principal: Principal,
    Json(body): Json<CreateMailbox>,
) -> Result<Response, ApiError> {
    principal.require_operator()?;
    if address::parse(&body.address).is_none() {
        return Err(bad_request("Invalid email address"));
    }
    if body.password.is_empty() {
        return Err(bad_request("Password must not be empty"));
    }

    let mailbox = repo::create_mailbox(&state.db, &body.address, &body.password)
        .await
        .map_err(|err| conflict(err.to_string()))?;

    Ok((
        StatusCode::CREATED,
        Json(DataResponse {
            data: MailboxResponse::from(mailbox),
        }),
    )
        .into_response())
}

#[utoipa::path(
    delete,
    path = "/api/mailboxes/{address}",
    tag = "mailboxes",
    responses(
        (status = 204, description = "Deleted"),
        (status = 404, description = "Unknown mailbox")
    )
)]
pub async fn delete_mailbox(
    State(state): State<AppState>,
    principal: Principal,
    Path(address): Path<String>,
) -> Result<Response, ApiError> {
    principal.require_operator()?;
    let deleted = repo::delete_mailbox(&state.db, &address)
        .await
        .map_err(db_error)?;
    if !deleted {
        return Err(not_found("Unknown mailbox"));
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[utoipa::path(
    patch,
    path = "/api/mailboxes/{address}",
    tag = "mailboxes",
    request_body = UpdateMailboxRequest,
    params(("address" = String, Path, description = "Mailbox address")),
    responses(
        (status = 200, description = "Updated mailbox", body = DataResponse<MailboxResponse>),
        (status = 404, description = "Unknown mailbox")
    )
)]
pub async fn update_mailbox(
    State(state): State<AppState>,
    principal: Principal,
    Path(address): Path<String>,
    Json(body): Json<UpdateMailboxRequest>,
) -> Result<Response, ApiError> {
    let address = address.to_ascii_lowercase();
    if let Some(scope) = principal.scope() {
        // A session may only edit the signature of its own mailbox.
        if address != scope.address {
            return Err(not_found("Unknown mailbox"));
        }
        if body.password.is_some() || body.quota_bytes.is_some() || body.active.is_some() {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "Forbidden",
                "A session may only change its own signature",
            )
            .code("forbidden"));
        }
    }
    let updated = repo::update_mailbox(
        &state.db,
        &address,
        body.password.as_deref(),
        body.signature,
        body.quota_bytes,
        body.active,
    )
    .await
    .map_err(db_error)?
    .ok_or_else(|| not_found("Unknown mailbox"))?;
    Ok(Json(DataResponse {
        data: MailboxResponse::from(updated),
    })
    .into_response())
}

// ---------------------------------------------------------------------------
// Domains
// ---------------------------------------------------------------------------

#[utoipa::path(
    get,
    path = "/api/domains",
    tag = "domains",
    responses((status = 200, description = "Domains", body = ListResponse<DomainResponse>))
)]
pub async fn list_domains(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Response, ApiError> {
    principal.require_operator()?;
    let domains = repo::list_domains(&state.db).await.map_err(db_error)?;
    let payload = domains.into_iter().map(DomainResponse::from).collect();
    Ok(ListResponse::new(payload).into_response())
}

#[utoipa::path(
    post,
    path = "/api/domains",
    tag = "domains",
    request_body = CreateDomain,
    responses((status = 201, description = "Domain created", body = DomainResponse))
)]
pub async fn create_domain(
    State(state): State<AppState>,
    principal: Principal,
    Json(body): Json<CreateDomain>,
) -> Result<Response, ApiError> {
    principal.require_operator()?;
    let name = body.name.trim().to_ascii_lowercase();
    if name.is_empty() || !name.contains('.') {
        return Err(bad_request("Invalid domain name"));
    }

    let domain = repo::ensure_domain(&state.db, &name)
        .await
        .map_err(internal)?;

    Ok((
        StatusCode::CREATED,
        Json(DataResponse {
            data: DomainResponse::from(domain),
        }),
    )
        .into_response())
}

#[utoipa::path(
    get,
    path = "/api/domains/{name}",
    tag = "domains",
    params(("name" = String, Path, description = "Domain name")),
    responses(
        (status = 200, description = "One domain", body = DataResponse<DomainResponse>),
        (status = 404, description = "Unknown domain")
    )
)]
pub async fn get_domain(
    State(state): State<AppState>,
    principal: Principal,
    Path(name): Path<String>,
) -> Result<Response, ApiError> {
    principal.require_operator()?;
    let domain = repo::find_domain(&state.db, &name)
        .await
        .map_err(db_error)?
        .ok_or_else(|| not_found("Unknown domain"))?;
    Ok(Json(DataResponse {
        data: DomainResponse::from(domain),
    })
    .into_response())
}

/// Activates or deactivates a whole tenant: a disabled domain stops receiving
/// (RCPT) and sending (submission AUTH) while its data stays intact.
#[utoipa::path(
    patch,
    path = "/api/domains/{name}",
    tag = "domains",
    request_body = UpdateDomainRequest,
    params(("name" = String, Path, description = "Domain name")),
    responses(
        (status = 200, description = "Updated domain", body = DataResponse<DomainResponse>),
        (status = 404, description = "Unknown domain")
    )
)]
pub async fn update_domain(
    State(state): State<AppState>,
    principal: Principal,
    Path(name): Path<String>,
    Json(body): Json<UpdateDomainRequest>,
) -> Result<Response, ApiError> {
    principal.require_operator()?;
    let domain = repo::set_domain_active(&state.db, &name, body.active)
        .await
        .map_err(db_error)?
        .ok_or_else(|| not_found("Unknown domain"))?;
    Ok(Json(DataResponse {
        data: DomainResponse::from(domain),
    })
    .into_response())
}

/// Removes a tenant domain. Its mailboxes cascade away with it.
#[utoipa::path(
    delete,
    path = "/api/domains/{name}",
    tag = "domains",
    params(("name" = String, Path, description = "Domain name")),
    responses((status = 204, description = "Deleted"), (status = 404, description = "Unknown domain"))
)]
pub async fn delete_domain(
    State(state): State<AppState>,
    principal: Principal,
    Path(name): Path<String>,
) -> Result<Response, ApiError> {
    principal.require_operator()?;
    let deleted = repo::delete_domain(&state.db, &name)
        .await
        .map_err(db_error)?;
    if !deleted {
        return Err(not_found("Unknown domain"));
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

// ---------------------------------------------------------------------------
// Send
// ---------------------------------------------------------------------------

#[utoipa::path(
    post,
    path = "/api/send",
    tag = "send",
    request_body = SendRequest,
    responses(
        (status = 202, description = "Accepted for delivery", body = SendResponse),
        (status = 400, description = "Invalid request")
    )
)]
pub async fn send(
    State(state): State<AppState>,
    principal: Principal,
    Json(body): Json<SendRequest>,
) -> Result<Response, ApiError> {
    let Some(sender) = bare_sender(&body.from) else {
        return Err(bad_request("Invalid from address"));
    };
    if let Some(scope) = principal.scope()
        && sender.to_ascii_lowercase() != scope.address
    {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "Forbidden",
            "A session may only send from its own mailbox",
        )
        .code("forbidden"));
    }
    if body.to.is_empty() {
        return Err(bad_request("At least one recipient is required"));
    }

    let mut local = Vec::new();
    let mut remote = Vec::new();
    let recipients: Vec<String> = body
        .to
        .iter()
        .chain(body.cc.iter())
        .chain(body.bcc.iter())
        .map(|recipient| bare_address(recipient))
        .collect();
    for recipient in &recipients {
        let Some(parsed) = address::parse(recipient) else {
            return Err(bad_request(format!("Invalid recipient: {recipient}")));
        };
        let normalized = parsed.normalized();
        match repo::find_mailbox(&state.db, &normalized)
            .await
            .map_err(db_error)?
        {
            Some(mailbox) if mailbox.active => local.push(mailbox),
            _ => remote.push(normalized),
        }
    }

    // Attachments the composer stored on the draft ride along with the message.
    let (draft_row, draft_attachments) = match body.draft_id {
        Some(draft_id) => {
            let row = repo::get_message(&state.db, draft_id)
                .await
                .map_err(db_error)?
                .ok_or_else(|| not_found("Unknown draft"))?;
            ensure_scope(&principal, row.mailbox_id, "Unknown draft")?;
            let draft_raw = state.store.get(&row.storage_key).await.map_err(internal)?;
            let attachments = owned_attachments(&draft_raw);
            (Some(row), attachments)
        }
        None => (None, Vec::new()),
    };
    let attachment_refs = attachment_refs(&draft_attachments);

    let raw = compose::build(&compose::Compose {
        from: &body.from,
        to: &body.to,
        cc: &body.cc,
        bcc: &body.bcc,
        subject: &body.subject,
        text: body.text.as_deref(),
        html: body.html.as_deref(),
        attachments: &attachment_refs,
        message_id: None,
        in_reply_to: None,
        references: None,
    })
    .map_err(internal)?;

    let outcome = AuthOutcome::skipped();
    let scheduled_at = body.scheduled_at.filter(|at| *at > chrono::Utc::now());
    let mut stored = 0usize;
    let mut scheduled_copy = None;

    // The sender's own copy: it is what its Sent folder shows, and the copy a
    // scheduled send moves to Sent once the relay accepts it.
    if let Some(mailbox) = repo::find_mailbox(&state.db, &sender)
        .await
        .map_err(db_error)?
    {
        let message_id = delivery::store_local(
            &state,
            &mailbox,
            message::Direction::Outbound,
            &sender,
            &raw,
            &outcome,
        )
        .await
        .map_err(internal)?;
        if scheduled_at.is_some() {
            // It waits in Scheduled until the queue sends it.
            upsert_flag(
                &state,
                message_id,
                Some(true),
                None,
                Some("scheduled".to_string()),
            )
            .await?;
            scheduled_copy = Some(message_id);
        }
        stored += 1;
    }

    // A local recipient gets the message in its inbox.
    for mailbox in &local {
        delivery::store_local(
            &state,
            mailbox,
            message::Direction::Inbound,
            &sender,
            &raw,
            &outcome,
        )
        .await
        .map_err(internal)?;
        stored += 1;
    }

    let mut queued = 0usize;
    if scheduled_at.is_some() {
        // Always queue, even with no remote recipients, so the copy moves.
        delivery::enqueue_remote_at(
            &state,
            None,
            &sender,
            &remote,
            &raw,
            scheduled_at,
            scheduled_copy,
        )
        .await
        .map_err(internal)?;
        queued = remote.len();
    } else if !remote.is_empty() {
        delivery::enqueue_remote(&state, None, &sender, &remote, &raw)
            .await
            .map_err(internal)?;
        queued = remote.len();
    }

    // The draft has become the sent message, so it leaves the drafts folder.
    if let Some(draft) = draft_row {
        repo::delete_message(&state.db, draft.id)
            .await
            .map_err(db_error)?;
        if let Err(err) = state.store.delete(&draft.storage_key).await {
            tracing::warn!(error = %err, key = %draft.storage_key, "failed to remove the sent draft");
        }
    }

    Ok((
        StatusCode::ACCEPTED,
        Json(DataResponse {
            data: SendResponse {
                stored,
                queued,
                scheduled: scheduled_at.is_some(),
            },
        }),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_from_header_may_carry_a_display_name() {
        assert_eq!(
            bare_sender("info@example.test").as_deref(),
            Some("info@example.test")
        );
        assert_eq!(
            bare_sender("<info@Example.COM>").as_deref(),
            Some("info@example.com")
        );
        assert_eq!(
            bare_sender(r#""Info" <info@example.test>"#).as_deref(),
            Some("info@example.test")
        );
        assert_eq!(bare_sender(r#""Info""#), None);
        assert_eq!(bare_sender("not-an-address"), None);
        assert_eq!(bare_sender(""), None);
    }
}
