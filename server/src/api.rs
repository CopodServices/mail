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
use mailcore::query;
use mailcore::{address, repo};
use mailcore::{body, compose, store};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, NotSet, QueryFilter, QueryOrder, Set};
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
        .routes(routes!(get_message_neighbors))
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
        .routes(routes!(update_contact_spam_list))
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
        .routes(routes!(get_client_connection))
        .routes(routes!(list_templates))
        .routes(routes!(create_template))
        .routes(routes!(get_template))
        .routes(routes!(delete_template))
        .routes(routes!(get_shortcuts_setting))
        .routes(routes!(update_shortcuts_setting))
        .routes(routes!(get_recipient_addresses_setting))
        .routes(routes!(update_recipient_addresses_setting))
        .routes(routes!(get_spam_setting))
        .routes(routes!(update_spam_setting))
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
                total: Some(total),
                limit: None,
                next_cursor: None,
            },
        }
    }

    /// A paginated collection: total is the filtered count, not the page size,
    /// and is absent when the request did not ask for a count.
    fn page(data: Vec<T>, total: Option<i64>, limit: u64, next_cursor: Option<String>) -> Self {
        Self {
            data,
            meta: Meta {
                total,
                limit: Some(limit),
                next_cursor,
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
    /// The number of rows the filters match; absent when the request asked for
    /// the page alone (`with_total=false`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    /// The opaque cursor for the page after this one, absent on the last page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
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
    /// The spam verdict recorded for this copy, when the filter scored it.
    pub spam_score: Option<f64>,
    pub spam_tags: Option<String>,
    pub spam_action: Option<String>,
    pub received_at: chrono::DateTime<chrono::Utc>,
    pub read: bool,
    pub starred: bool,
    /// When a snoozed message returns to the inbox.
    pub snoozed_until: Option<chrono::DateTime<chrono::Utc>>,
    /// inbox, sent, spam, trash, archived, snoozed, scheduled, or a folder id.
    pub folder: String,
    /// When a scheduled message leaves the Scheduled folder.
    pub scheduled_at: Option<chrono::DateTime<chrono::Utc>>,
    /// The state of the job delivering this copy: queued, sending, sent or
    /// failed. Absent when nothing queued it.
    pub send_state: Option<String>,
    /// Why a delivery failed, when it did.
    pub send_error: Option<String>,
}

/// The delivery state of a stored copy, read from the outbound job that owns
/// it. Absent for inbound mail and for a message delivered only locally.
#[derive(Debug, Default, Clone)]
pub struct MessageExtras {
    pub scheduled_at: Option<chrono::DateTime<chrono::Utc>>,
    pub send_state: Option<String>,
    pub send_error: Option<String>,
}

/// Loads the delivery state of a page of messages in two lookups.
async fn message_extras(
    state: &AppState,
    ids: &[Uuid],
) -> Result<std::collections::HashMap<Uuid, MessageExtras>, ApiError> {
    let scheduled = query::scheduled_times(&state.db, ids)
        .await
        .map_err(db_error)?;
    let sends = query::send_states(&state.db, ids).await.map_err(db_error)?;
    let mut extras = std::collections::HashMap::new();
    for id in ids {
        let scheduled_at = scheduled.get(id).copied();
        let send = sends.get(id);
        if scheduled_at.is_none() && send.is_none() {
            continue;
        }
        extras.insert(
            *id,
            MessageExtras {
                scheduled_at,
                send_state: send.map(|send| send.state.to_string()),
                send_error: send.and_then(|send| send.error.clone()),
            },
        );
    }
    Ok(extras)
}

/// The delivery state of one message.
async fn message_extras_for(state: &AppState, id: Uuid) -> Result<MessageExtras, ApiError> {
    Ok(message_extras(state, &[id])
        .await?
        .remove(&id)
        .unwrap_or_default())
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
    fn with_flags(
        model: message::Model,
        flag: Option<&message_flag::Model>,
        extras: MessageExtras,
    ) -> Self {
        Self {
            folder: folder_of(&model, flag),
            // The copy the sender owns is read by definition; only inbound mail
            // carries an unread state.
            read: flag.map(|flag| flag.read).unwrap_or(false)
                || model.direction == message::Direction::Outbound,
            starred: flag.map(|flag| flag.starred).unwrap_or(false),
            snoozed_until: flag.and_then(|flag| flag.snoozed_until),
            scheduled_at: extras.scheduled_at,
            send_state: extras.send_state,
            send_error: extras.send_error,
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
            spam_score: model.spam_score,
            spam_tags: model.spam_tags,
            spam_action: model.spam_action,
            received_at: model.received_at,
            read: false,
            starred: false,
            snoozed_until: None,
            folder: "inbox".to_string(),
            scheduled_at: None,
            send_state: None,
            send_error: None,
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
    /// The name recipients see beside the address in the From header.
    pub display_name: Option<String>,
    /// Address every arriving message is copied to; absent when forwarding is off.
    pub forward_to: Option<String>,
    /// Storage limit for the mailbox in bytes; 0 means unlimited.
    pub quota_bytes: i64,
    /// Bytes stored for the mailbox, summed across its messages.
    pub used_bytes: i64,
}

impl MailboxResponse {
    fn new(model: entity::mailbox::Model, used_bytes: i64) -> Self {
        Self {
            id: model.id,
            address: model.address,
            active: model.active,
            created_at: model.created_at,
            signature: model.signature,
            display_name: model.display_name,
            forward_to: model.forward_to,
            quota_bytes: model.quota_bytes,
            used_bytes,
        }
    }

    /// Builds the response from the mailbox's running byte total.
    pub fn load(model: entity::mailbox::Model) -> Self {
        let used_bytes = model.used_bytes;
        Self::new(model, used_bytes)
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateMailboxRequest {
    /// Replaces the mailbox signature; explicit null clears it, omitted leaves
    /// it unchanged. The one field a mailbox session may set on itself.
    #[serde(default, deserialize_with = "double_option")]
    pub signature: Option<Option<String>>,
    /// Replaces the name recipients see beside the address in the From header;
    /// explicit null clears it, omitted leaves it unchanged. A mailbox session
    /// may set this on its own mailbox, alongside the signature.
    #[serde(default, deserialize_with = "double_option")]
    pub display_name: Option<Option<String>>,
    /// Replaces the auto-forward address; explicit null turns forwarding off,
    /// omitted leaves it unchanged. A mailbox session may set this on its own
    /// mailbox, alongside the signature.
    #[serde(default, deserialize_with = "double_option")]
    pub forward_to: Option<Option<String>>,
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
    /// The message this one answers, and its ancestor chain. A reply the
    /// composer has not stored as a draft yet carries them here; a stored draft
    /// is the fallback, so a client may send either.
    #[serde(default)]
    pub in_reply_to: Option<String>,
    #[serde(default)]
    pub references_header: Option<String>,
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
    /// The opaque cursor a previous page returned; the list resumes after it.
    pub cursor: Option<String>,
    /// Whether to count every matching message for `meta.total` (default true).
    /// The web client passes false and takes its totals from
    /// /api/messages/counts, so the list does not run a second aggregate.
    pub with_total: Option<bool>,
}

impl MessageQuery {
    /// The SQL filters this query asks for. Only the mailbox scope is shared
    /// with the counts, which ignore the list's own filters.
    async fn filters(
        &self,
        state: &AppState,
        scope: Option<Uuid>,
    ) -> Result<query::MessageFilters, ApiError> {
        Ok(query::MessageFilters {
            mailbox_id: resolve_mailbox(state, self, scope).await?,
            folder: self.folder.clone(),
            direction: self.direction.clone(),
            q: self.q.clone(),
            read: self.read.clone(),
            starred: self.starred.as_deref() == Some("true"),
        })
    }

    /// The keyset cursor the client asked for, if it is one we wrote.
    fn cursor(&self) -> Result<Option<query::MessageCursor>, ApiError> {
        match self.cursor.as_deref() {
            None => Ok(None),
            Some(value) => query::MessageCursor::decode(value)
                .map(Some)
                .ok_or_else(|| bad_request("Unknown cursor")),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AttachmentResponse {
    /// Position of the attachment in the message, used to fetch its bytes.
    pub index: usize,
    pub filename: Option<String>,
    pub content_type: Option<String>,
    /// Content-ID of an inline part, the value a body's cid: reference names.
    pub content_id: Option<String>,
    pub size_bytes: i64,
}

impl AttachmentResponse {
    /// The response for one MIME part of a stored message. The position doubles
    /// as the id the client fetches the bytes by.
    fn from_part(index: usize, attachment: &body::Attachment) -> Self {
        Self {
            index,
            filename: attachment.filename.clone(),
            content_type: attachment.content_type.clone(),
            content_id: attachment.content_id.clone(),
            size_bytes: attachment.bytes.len() as i64,
        }
    }
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
    let previous = existing.as_ref().map(|flag| flag.folder.clone());
    let requested = folder.clone();
    let model = message_flag::ActiveModel {
        message_id: Set(message_id),
        read: Set(read
            .or(existing.as_ref().map(|flag| flag.read))
            .unwrap_or(false)),
        starred: Set(starred
            .or(existing.as_ref().map(|flag| flag.starred))
            .unwrap_or(false)),
        answered: Set(existing.as_ref().map(|flag| flag.answered).unwrap_or(false)),
        deleted: Set(existing.as_ref().map(|flag| flag.deleted).unwrap_or(false)),
        folder: Set(folder
            .or_else(|| existing.as_ref().map(|flag| flag.folder.clone()))
            .unwrap_or_else(|| "inbox".to_string())),
        snoozed_until: Set(existing.as_ref().and_then(|flag| flag.snoozed_until)),
        updated_at: Set(chrono::Utc::now()),
    };
    // The row keyed by message_id may already exist, so update instead of insert.
    let row = if existing.is_some() {
        model.update(&state.db).await.map_err(db_error)?
    } else {
        model.insert(&state.db).await.map_err(db_error)?
    };
    if let Some(next) = requested.as_deref() {
        spawn_training(state, message_id, previous.as_deref(), next);
    }
    Ok(row)
}

/// Trains the classifier when a message moves into or out of the spam folder.
fn spawn_training(state: &AppState, message_id: Uuid, previous: Option<&str>, next: &str) {
    let Some(engine) = state.spam.clone() else {
        return;
    };
    let spam = match (previous, next) {
        (Some("spam"), "spam") => return,
        (_, "spam") => true,
        (Some("spam"), _) => false,
        _ => return,
    };
    let db = state.db.clone();
    let store = state.store.clone();
    tokio::spawn(async move {
        let Ok(Some(message)) = repo::get_message(&db, message_id).await else {
            return;
        };
        let Ok(raw) = store.get(&message.storage_key).await else {
            return;
        };
        let tokens = engine.tokens_for(&raw);
        if tokens.is_empty() {
            return;
        }
        engine.learn_tokens(message.mailbox_id, &tokens, spam);
        crate::spam_store::record_sample(&db, message.mailbox_id, &tokens, spam).await;
    });
}

#[derive(Debug, Serialize, ToSchema)]
pub struct NavigationEntry {
    pub id: Uuid,
    pub unread: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct NeighborsResponse {
    /// The message just newer than the open one in the reading order.
    pub previous_id: Option<Uuid>,
    /// The message just older than the open one in the reading order.
    pub next_id: Option<Uuid>,
    /// One-based position of the open message in the reading order.
    pub position: i64,
    /// Every message the folder's filters match.
    pub total: i64,
    /// One-based position among the unread messages; zero when read.
    pub unread_position: i64,
    /// Every unread message the folder's filters match.
    pub unread_total: i64,
}

/// The folder an open message is read in, plus the client's view of its read
/// state, so a neighbour report stays stable while opening it marks it read.
#[derive(Debug, Default, Deserialize)]
pub struct NeighborQuery {
    pub mailbox: Option<String>,
    pub mailbox_id: Option<Uuid>,
    /// System folder or custom folder id.
    pub folder: Option<String>,
    pub unread: Option<bool>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ThreadResponse {
    pub thread_id: String,
    /// Every message in the conversation, without bodies or attachments: the
    /// reader already holds the open message's body, and a collapsed member's
    /// body is loaded only when it is expanded, so the thread never reads and
    /// parses every stored copy on the open path.
    pub messages: Vec<MessageResponse>,
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

/// How many ancestors the walk follows before it gives up. A conversation
/// deeper than this is pathological, and the thread list is not the place to
/// resolve it.
const THREAD_DEPTH_CAP: usize = 20;

/// Walks In-Reply-To back to the oldest stored ancestor, one point lookup per
/// hop, and returns its id.
async fn thread_root_for(state: &AppState, start: &message::Model) -> Result<String, ApiError> {
    let Some(mailbox_id) = start.mailbox_id else {
        return Ok(start
            .message_id
            .clone()
            .unwrap_or_else(|| start.id.to_string()));
    };
    let mut current = start.clone();
    let mut visited = std::collections::HashSet::new();
    while visited.len() < THREAD_DEPTH_CAP {
        let Some(parent_id) = current.in_reply_to.clone() else {
            break;
        };
        if !visited.insert(parent_id.clone()) {
            break;
        }
        match repo::find_message_by_message_id(&state.db, mailbox_id, &parent_id)
            .await
            .map_err(db_error)?
        {
            Some(parent) => current = parent,
            None => break,
        }
    }
    Ok(current
        .message_id
        .clone()
        .unwrap_or_else(|| current.id.to_string()))
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
        ("limit" = Option<u64>, Query, description = "Page size (default 50, max 200)"),
        ("cursor" = Option<String>, Query, description = "Cursor the previous page returned"),
        ("with_total" = Option<bool>, Query, description = "Count every matching message for meta.total (default true); false skips the count")
    ),
    responses((status = 200, description = "Stored messages", body = ListResponse<MessageResponse>))
)]
pub async fn list_messages(
    State(state): State<AppState>,
    principal: Principal,
    Query(query): Query<MessageQuery>,
) -> Result<Response, ApiError> {
    let filters = query.filters(&state, principal.mailbox_scope()).await?;
    let cursor = query.cursor()?;
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let page = query::message_page_ids(&state.db, &filters, cursor, limit)
        .await
        .map_err(db_error)?;
    // The count is a second aggregate over the whole mailbox. A caller that
    // already holds the totals (the web client reads /api/messages/counts)
    // passes with_total=false and gets the page alone.
    let total = match query.with_total {
        Some(false) => None,
        _ => Some(
            query::count_matching(&state.db, &filters)
                .await
                .map_err(db_error)?,
        ),
    };

    // The ids come back in page order; fetch the rows and their state to match.
    let rows = message::Entity::find()
        .filter(message::Column::Id.is_in(page.ids.clone()))
        .all(&state.db)
        .await
        .map_err(db_error)?;
    let flags = load_flags(&state, &page.ids).await?;
    let mut extras = message_extras(&state, &page.ids).await?;

    let mut messages = Vec::with_capacity(page.ids.len());
    for id in &page.ids {
        let Some(row) = rows.iter().find(|row| row.id == *id).cloned() else {
            continue;
        };
        let flag = flags.iter().find(|flag| flag.message_id == *id);
        let extras = extras.remove(id).unwrap_or_default();
        messages.push(MessageResponse::with_flags(row, flag, extras));
    }

    let next_cursor = page.next.map(|cursor| cursor.encode());
    Ok(ListResponse::page(messages, total, limit, next_cursor).into_response())
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
    let raw = read_stored(&state, &message.storage_key).await?;
    let parsed = body::extract(&raw);
    let attachments = parsed
        .attachments
        .iter()
        .enumerate()
        .map(|(index, attachment)| AttachmentResponse::from_part(index, attachment))
        .collect();

    Ok((
        StatusCode::OK,
        Json(DataResponse {
            data: MessageDetailResponse {
                message: MessageResponse::with_flags(
                    message,
                    flag.as_ref(),
                    message_extras_for(&state, id).await?,
                ),
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
    let bytes = read_stored(&state, &message.storage_key).await?;

    Ok(Response::builder()
        .header(header::CONTENT_TYPE, "message/rfc822")
        .body(Body::from(bytes))
        .expect("valid response"))
}

/// Reads a stored message, telling a copy object storage no longer holds (410)
/// apart from object storage being unreachable (500).
async fn read_stored(state: &AppState, key: &str) -> Result<Vec<u8>, ApiError> {
    state.store.get(key).await.map_err(|err| {
        if store::is_missing(&err) {
            ApiError::new(
                StatusCode::GONE,
                "Gone",
                "The stored copy of this message is no longer in object storage",
            )
            .code("message_unavailable")
        } else {
            internal(err)
        }
    })
}

/// Removes a stored copy, retrying once and logging loudly when it is still
/// there: the row is gone by then, so an object left behind is invisible.
async fn purge_object(state: &AppState, key: &str) {
    if let Err(err) = state.store.delete(key).await {
        tracing::warn!(error = %err, key, "failed to remove the stored object; retrying");
        if let Err(err) = state.store.delete(key).await {
            tracing::error!(
                error = %err,
                key,
                "orphaned object: the stored copy could not be removed"
            );
        }
    }
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

    purge_object(&state, &message.storage_key).await;

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
        answered: Set(existing.as_ref().map(|flag| flag.answered).unwrap_or(false)),
        deleted: Set(existing.as_ref().map(|flag| flag.deleted).unwrap_or(false)),
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
        data: MessageResponse::with_flags(
            message,
            Some(&flag),
            message_extras_for(state, id).await?,
        ),
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
    Ok(MessageResponse::with_flags(
        message,
        Some(&flag),
        message_extras_for(state, id).await?,
    ))
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

/// Query of the attachment endpoint. preview=1 asks for an inline response so
/// the browser renders the part in place; anything else downloads it.
#[derive(Debug, Default, Deserialize)]
pub struct AttachmentQuery {
    #[serde(default)]
    preview: Option<String>,
}

impl AttachmentQuery {
    fn preview_requested(&self) -> bool {
        match self.preview.as_deref() {
            Some(value) => !matches!(value, "" | "0" | "false" | "no"),
            None => false,
        }
    }
}

/// Whether a preview may be rendered inline. Only the types the web client
/// shows are served inline: HTML, SVG and XML can carry script, so they stay
/// downloads and a preview URL can never run in the mail origin.
fn inline_previewable(content_type: &str) -> bool {
    let essence = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if essence.starts_with("audio/") || essence.starts_with("video/") {
        return true;
    }
    if essence.starts_with("image/") && essence != "image/svg+xml" {
        return true;
    }
    matches!(
        essence.as_str(),
        "application/pdf" | "application/json" | "text/plain" | "text/csv"
    )
}

/// The Content-Disposition value for a part: a quoted ASCII filename with the
/// RFC 5987 UTF-8 form appended when the name is not plain ASCII.
fn content_disposition(kind: &str, filename: &str) -> String {
    let ascii: String = filename
        .chars()
        .map(|ch| {
            if ch.is_ascii() && !ch.is_ascii_control() {
                ch
            } else {
                '_'
            }
        })
        .collect();
    let escaped = ascii.replace('\\', "\\\\").replace('"', "\\\"");
    let mut value = format!("{kind}; filename=\"{escaped}\"");
    if !filename.is_ascii() {
        value.push_str("; filename*=UTF-8''");
        value.push_str(&percent_encode_utf8(filename));
    }
    value
}

/// RFC 5987 attr-char percent-encoding for the UTF-8 filename form.
fn percent_encode_utf8(value: &str) -> String {
    let mut out = String::new();
    for byte in value.as_bytes() {
        let ch = *byte as char;
        if ch.is_ascii_alphanumeric()
            || matches!(
                ch,
                '!' | '#' | '$' | '&' | '+' | '-' | '.' | '^' | '_' | '\u{60}' | '|' | '~'
            )
        {
            out.push(ch);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[utoipa::path(
    get,
    path = "/api/messages/{id}/attachments/{index}",
    tag = "messages",
    params(
        ("id" = Uuid, Path, description = "Message id"),
        ("index" = usize, Path, description = "Attachment position from the message detail"),
        ("preview" = Option<String>, Query, description = "Pass 1 to serve the part inline for in-place preview instead of download")
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
    Query(query): Query<AttachmentQuery>,
) -> Result<Response, ApiError> {
    let message = repo::get_message(&state.db, id)
        .await
        .map_err(db_error)?
        .ok_or_else(|| not_found("Unknown message"))?;
    ensure_scope(&principal, message.mailbox_id, "Unknown message")?;
    let raw = read_stored(&state, &message.storage_key).await?;
    let parsed = body::extract(&raw);
    let attachment = parsed
        .attachments
        .into_iter()
        .nth(index)
        .ok_or_else(|| not_found("Unknown attachment"))?;

    let filename = attachment
        .filename
        .unwrap_or_else(|| "attachment".to_string());
    let content_type = attachment
        .content_type
        .unwrap_or_else(|| "application/octet-stream".to_string());
    let inline = query.preview_requested() && inline_previewable(&content_type);
    let disposition = content_disposition(if inline { "inline" } else { "attachment" }, &filename);
    Ok(Response::builder()
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_DISPOSITION, disposition)
        .header("x-content-type-options", "nosniff")
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
    // The counts cover the whole mailbox: folding the list's own filters in
    // would make the sidebar numbers depend on the page that is open.
    let mailbox_id = resolve_mailbox(&state, &query, principal.mailbox_scope()).await?;
    let counts = query::folder_counts(&state.db, mailbox_id)
        .await
        .map_err(db_error)?;

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
    // A custom folder is counted under its own id; a system folder under its
    // name. Starred is a view over the flags, so it comes from its own count.
    let mut custom_folders: std::collections::BTreeMap<String, FolderCount> =
        std::collections::BTreeMap::new();
    for (folder, (total, unread)) in &counts.folders {
        let entry = FolderCount {
            total: *total,
            unread: *unread,
        };
        if folders.contains_key(folder) {
            folders.insert(folder.clone(), entry);
        } else {
            custom_folders.insert(folder.clone(), entry);
        }
    }
    folders.insert(
        "starred".to_string(),
        FolderCount {
            total: counts.starred.0,
            unread: counts.starred.1,
        },
    );

    let mailboxes: Vec<MailboxCount> = counts
        .mailboxes
        .iter()
        .map(|(mailbox_id, (total, unread))| MailboxCount {
            mailbox_id: *mailbox_id,
            total: *total,
            unread: *unread,
        })
        .collect();

    Ok(Json(DataResponse {
        data: CountsResponse {
            folders,
            custom_folders,
            mailboxes,
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
    let filters = query.filters(&state, principal.mailbox_scope()).await?;
    let entries: Vec<NavigationEntry> =
        query::navigation(&state.db, &filters, query::NAVIGATION_CAP)
            .await
            .map_err(db_error)?
            .into_iter()
            .map(|(id, unread)| NavigationEntry { id, unread })
            .collect();
    Ok(Json(DataResponse { data: entries }).into_response())
}

#[utoipa::path(
    get,
    path = "/api/messages/{id}/neighbors",
    tag = "messages",
    params(
        ("id" = Uuid, Path, description = "Message id"),
        ("mailbox" = Option<String>, Query, description = "Mailbox address"),
        ("mailbox_id" = Option<Uuid>, Query, description = "Mailbox id"),
        ("folder" = Option<String>, Query, description = "Folder"),
        ("unread" = Option<bool>, Query, description = "The client's view of the open message's read state")
    ),
    responses(
        (status = 200, description = "Neighbours and position in the open folder", body = DataResponse<NeighborsResponse>),
        (status = 404, description = "Unknown message")
    )
)]
pub async fn get_message_neighbors(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<Uuid>,
    Query(params): Query<NeighborQuery>,
) -> Result<Response, ApiError> {
    let target = repo::get_message(&state.db, id)
        .await
        .map_err(db_error)?
        .ok_or_else(|| not_found("Unknown message"))?;
    ensure_scope(&principal, target.mailbox_id, "Unknown message")?;

    // The folder the reader is walking; the neighbour walk stays inside it.
    let query = MessageQuery {
        mailbox: params.mailbox.clone(),
        mailbox_id: params.mailbox_id,
        folder: params.folder.clone(),
        ..MessageQuery::default()
    };
    let filters = query.filters(&state, principal.mailbox_scope()).await?;
    // The client's read state for the open message, so the report does not move
    // when opening it marks it read. Without it, fall back to the stored flag.
    let target_unread = match params.unread {
        Some(unread) => unread,
        None => match target.direction {
            message::Direction::Inbound => {
                let flag = message_flag::Entity::find_by_id(target.id)
                    .one(&state.db)
                    .await
                    .map_err(db_error)?;
                !flag.map(|flag| flag.read).unwrap_or(false)
            }
            message::Direction::Outbound => false,
        },
    };
    let neighbors = query::neighbors(
        &state.db,
        &filters,
        query::MessageCursor {
            received_at: target.received_at,
            id: target.id,
        },
        target_unread,
    )
    .await
    .map_err(db_error)?;

    Ok(Json(DataResponse {
        data: NeighborsResponse {
            previous_id: neighbors.previous_id,
            next_id: neighbors.next_id,
            position: neighbors.position,
            total: neighbors.total,
            unread_position: neighbors.unread_position,
            unread_total: neighbors.unread_total,
        },
    })
    .into_response())
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

    let root = thread_root_for(&state, &target).await?;
    // Only the conversation's candidates are read, and the References match is
    // a substring, so membership is re-checked before anything is returned.
    let members: Vec<message::Model> = match target.mailbox_id {
        Some(mailbox_id) => repo::thread_candidates(&state.db, mailbox_id, &root)
            .await
            .map_err(db_error)?
            .into_iter()
            .filter(|row| in_thread(row, &root))
            .collect(),
        None => vec![target.clone()],
    };

    let ids: Vec<Uuid> = members.iter().map(|row| row.id).collect();
    let flags = load_flags(&state, &ids).await?;
    let mut extras = message_extras(&state, &ids).await?;

    let mut messages = Vec::with_capacity(members.len());
    for row in members {
        let id = row.id;
        let flag = flags.iter().find(|flag| flag.message_id == row.id);
        messages.push(MessageResponse::with_flags(
            row,
            flag,
            extras.remove(&id).unwrap_or_default(),
        ));
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

    let filters = query.filters(&state, principal.mailbox_scope()).await?;
    let ids = query::message_ids(&state.db, &filters, query::NAVIGATION_CAP)
        .await
        .map_err(db_error)?;
    let mut deleted = 0usize;
    for id in ids {
        let Some(row) = repo::get_message(&state.db, id).await.map_err(db_error)? else {
            continue;
        };
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
        .map(|(index, attachment)| AttachmentResponse::from_part(index, attachment))
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
            // The sequence hands the new row its uid.
            uid: NotSet,
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
            spam_score: Set(None),
            spam_tags: Set(None),
            spam_action: Set(None),
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
        spam_threshold: Set(None),
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

/// The settings another mail app needs to send as this mailbox and, when the
/// readers are configured, to read it over IMAP or POP3.
#[derive(Debug, Serialize, ToSchema)]
pub struct SpamSettingResponse {
    /// This mailbox's junk threshold; null means the global default applies.
    pub threshold: Option<f64>,
    /// The global threshold from the server configuration.
    pub default_threshold: f64,
    /// Senders never filed as junk.
    pub allow: Vec<String>,
    /// Senders always filed as junk.
    pub block: Vec<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SpamSettingRequest {
    /// New junk threshold for this mailbox, or null to follow the default.
    #[serde(default)]
    pub threshold: Option<f64>,
}

#[utoipa::path(
    get,
    path = "/api/settings/spam",
    tag = "send",
    responses((status = 200, description = "Junk threshold", body = DataResponse<SpamSettingResponse>))
)]
pub async fn get_spam_setting(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Response, ApiError> {
    let row = settings_row(&state, settings_scope(&principal)).await?;
    let (allow, block) = spam_lists(&state, principal.mailbox_scope()).await?;
    Ok(Json(DataResponse {
        data: SpamSettingResponse {
            threshold: row.spam_threshold,
            default_threshold: default_junk_threshold(&state),
            allow,
            block,
        },
    })
    .into_response())
}

#[utoipa::path(
    patch,
    path = "/api/settings/spam",
    tag = "send",
    request_body = SpamSettingRequest,
    responses((status = 200, description = "Updated junk threshold", body = DataResponse<SpamSettingResponse>))
)]
pub async fn update_spam_setting(
    State(state): State<AppState>,
    principal: Principal,
    Json(body): Json<SpamSettingRequest>,
) -> Result<Response, ApiError> {
    let row = settings_row(&state, settings_scope(&principal)).await?;
    let mut active: setting::ActiveModel = row.into();
    active.spam_threshold = Set(body.threshold);
    active.updated_at = Set(chrono::Utc::now());
    let row = active.update(&state.db).await.map_err(db_error)?;
    let (allow, block) = spam_lists(&state, principal.mailbox_scope()).await?;
    Ok(Json(DataResponse {
        data: SpamSettingResponse {
            threshold: row.spam_threshold,
            default_threshold: default_junk_threshold(&state),
            allow,
            block,
        },
    })
    .into_response())
}

/// The global junk threshold, or the built-in default when the filter is off.
fn default_junk_threshold(state: &AppState) -> f64 {
    state
        .spam
        .as_ref()
        .map(|engine| engine.junk_threshold() as f64)
        .unwrap_or(5.0)
}

/// The mailbox's sender allow and block lists.
async fn spam_lists(
    state: &AppState,
    mailbox_id: Option<Uuid>,
) -> Result<(Vec<String>, Vec<String>), ApiError> {
    let Some(mailbox_id) = mailbox_id else {
        return Ok((Vec::new(), Vec::new()));
    };
    let rows = contact::Entity::find()
        .filter(contact::Column::MailboxId.eq(mailbox_id))
        .all(&state.db)
        .await
        .map_err(db_error)?;
    let mut allow = Vec::new();
    let mut block = Vec::new();
    for row in rows {
        if row.allowed {
            allow.push(row.address.clone());
        } else if row.blocked {
            block.push(row.address.clone());
        }
    }
    Ok((allow, block))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ClientConnectionResponse {
    /// Submission host a mail app connects to.
    pub smtp_host: String,
    /// Submission port on that host.
    pub smtp_port: u16,
    /// "starttls" when the listener offers TLS, otherwise "none".
    pub smtp_security: String,
    /// Address to authenticate as; absent for the operator, who manages every mailbox.
    pub username: Option<String>,
    /// True when this instance answers IMAP.
    pub imap_supported: bool,
    /// IMAP host a mail app connects to.
    pub imap_host: String,
    /// IMAP port on that host.
    pub imap_port: u16,
    /// "tls" when the IMAP listener is encrypted, otherwise "none".
    pub imap_security: String,
    /// True when this instance answers POP3.
    pub pop3_supported: bool,
    /// POP3 host a mail app connects to.
    pub pop3_host: String,
    /// POP3 port on that host.
    pub pop3_port: u16,
    /// "tls" when the POP3 listener is encrypted, otherwise "none".
    pub pop3_security: String,
}

/// The port the submission listener is bound to, read from its configured
/// address, falling back to the registered submission port.
fn submission_port(listen: &str) -> u16 {
    listen
        .rsplit(':')
        .next()
        .and_then(|port| port.parse().ok())
        .unwrap_or(587)
}

#[utoipa::path(
    get,
    path = "/api/settings/clients",
    tag = "send",
    responses((status = 200, description = "Settings for connecting another mail app", body = DataResponse<ClientConnectionResponse>))
)]
pub async fn get_client_connection(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Response, ApiError> {
    let smtp_security = if state.config.starttls_enabled() {
        "starttls"
    } else {
        "none"
    };
    let reader_security = if state.config.starttls_enabled() {
        "tls"
    } else {
        "none"
    };
    let imap_port = state
        .config
        .imap_listen
        .as_deref()
        .map(|listen| mailcore::config::listen_port(listen, 993))
        .unwrap_or(993);
    let pop3_port = state
        .config
        .pop3_listen
        .as_deref()
        .map(|listen| mailcore::config::listen_port(listen, 995))
        .unwrap_or(995);
    Ok(Json(DataResponse {
        data: ClientConnectionResponse {
            smtp_host: state.config.hostname.clone(),
            smtp_port: submission_port(&state.config.submission_listen),
            smtp_security: smtp_security.to_string(),
            username: principal.scope().map(|scope| scope.address.clone()),
            imap_supported: state.config.imap_enabled(),
            imap_host: state.config.hostname.clone(),
            imap_port,
            imap_security: reader_security.to_string(),
            pop3_supported: state.config.pop3_enabled(),
            pop3_host: state.config.hostname.clone(),
            pop3_port,
            pop3_security: reader_security.to_string(),
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

#[derive(Debug, Deserialize, ToSchema)]
pub struct SpamListRequest {
    pub mailbox: Option<String>,
    pub mailbox_id: Option<Uuid>,
    pub address: String,
    /// allow (never junk), block (always junk) or none (clear both).
    pub kind: String,
}

#[utoipa::path(
    post,
    path = "/api/contacts/spam-list",
    tag = "mailboxes",
    request_body = SpamListRequest,
    responses((status = 200, description = "Updated spam list", body = DataResponse<ContactResponse>))
)]
pub async fn update_contact_spam_list(
    State(state): State<AppState>,
    principal: Principal,
    Json(body): Json<SpamListRequest>,
) -> Result<Response, ApiError> {
    let mailbox_id = contact_mailbox(
        &state,
        &body.mailbox,
        body.mailbox_id,
        principal.mailbox_scope(),
    )
    .await?;
    let (blocked, allowed) = match body.kind.trim().to_ascii_lowercase().as_str() {
        "allow" => (false, true),
        "block" => (true, false),
        "none" | "clear" => (false, false),
        _ => return Err(bad_request("kind must be allow, block or none")),
    };
    let row = set_spam_list(&state, mailbox_id, &body.address, blocked, allowed).await?;
    Ok(Json(DataResponse {
        data: contact_response(&row),
    })
    .into_response())
}

async fn set_spam_list(
    state: &AppState,
    mailbox_id: Uuid,
    address: &str,
    blocked: bool,
    allowed: bool,
) -> Result<contact::Model, ApiError> {
    let address = address.trim().to_ascii_lowercase();
    if address::parse(&address).is_none() {
        return Err(bad_request("Invalid email address"));
    }
    let now = chrono::Utc::now();
    match find_contact(state, mailbox_id, &address).await? {
        Some(row) => {
            let mut active: contact::ActiveModel = row.into();
            active.blocked = Set(blocked);
            active.allowed = Set(allowed);
            active.updated_at = Set(now);
            active.update(&state.db).await.map_err(db_error)
        }
        None => contact::ActiveModel {
            id: Set(Uuid::now_v7()),
            mailbox_id: Set(mailbox_id),
            address: Set(address),
            display_name: Set(None),
            avatar_key: Set(None),
            preview_key: Set(None),
            avatar_content_type: Set(None),
            blocked: Set(blocked),
            allowed: Set(allowed),
            source: Set("manual".to_string()),
            last_seen_at: Set(now),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(&state.db)
        .await
        .map_err(db_error),
    }
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
            allowed: Set(false),
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
    let raw = read_stored(&state, &message.storage_key).await?;
    let parsed = body::extract(&raw);
    let attachments = parsed
        .attachments
        .iter()
        .enumerate()
        .map(|(index, attachment)| AttachmentResponse::from_part(index, attachment))
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
        data: MessageResponse::with_flags(
            message,
            Some(&flag),
            message_extras_for(&state, id).await?,
        ),
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
    // The running total kept on each mailbox row answers this without a query.
    let payload = mailboxes
        .into_iter()
        .map(|mailbox| {
            let used_bytes = mailbox.used_bytes;
            MailboxResponse::new(mailbox, used_bytes)
        })
        .collect();
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

    let response = MailboxResponse::load(mailbox);
    Ok((StatusCode::CREATED, Json(DataResponse { data: response })).into_response())
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
    let Some(mailbox) = repo::find_mailbox(&state.db, &address)
        .await
        .map_err(db_error)?
    else {
        return Err(not_found("Unknown mailbox"));
    };
    // A mailbox takes its messages and its queued copies with it: a message row's
    // mailbox is nullable, so leaving them behind would hide them from every
    // reader while their objects stay in the bucket.
    let messages = message::Entity::find()
        .filter(message::Column::MailboxId.eq(mailbox.id))
        .all(&state.db)
        .await
        .map_err(db_error)?;
    for row in &messages {
        purge_object(&state, &row.storage_key).await;
        repo::delete_message(&state.db, row.id)
            .await
            .map_err(db_error)?;
    }
    let jobs = outbound_job::Entity::find()
        .filter(outbound_job::Column::MailboxId.eq(mailbox.id))
        .all(&state.db)
        .await
        .map_err(db_error)?;
    for job in &jobs {
        purge_object(&state, &job.storage_key).await;
        outbound_job::Entity::delete_by_id(job.id)
            .exec(&state.db)
            .await
            .map_err(db_error)?;
    }
    repo::delete_mailbox(&state.db, &address)
        .await
        .map_err(db_error)?;
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
    if let Some(quota_bytes) = body.quota_bytes
        && quota_bytes < 0
    {
        return Err(bad_request("quota_bytes must not be negative"));
    }
    if let Some(scope) = principal.scope() {
        // A session may only edit its own mailbox, and only its sender identity.
        if address != scope.address {
            return Err(not_found("Unknown mailbox"));
        }
        if body.password.is_some() || body.quota_bytes.is_some() || body.active.is_some() {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "Forbidden",
                "A session may only change its own signature, display name and forwarding address",
            )
            .code("forbidden"));
        }
    }
    // A forwarding address is stored normalised; an empty one turns it off.
    let forward_to = match body.forward_to {
        None => None,
        Some(None) => Some(None),
        Some(Some(value)) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                Some(None)
            } else {
                let parsed = address::parse(trimmed)
                    .ok_or_else(|| bad_request("forward_to must be an email address"))?;
                Some(Some(parsed.normalized()))
            }
        }
    };
    let updated = repo::update_mailbox(
        &state.db,
        &address,
        repo::MailboxUpdate {
            password: body.password,
            signature: body.signature,
            display_name: body.display_name,
            forward_to,
            quota_bytes: body.quota_bytes,
            active: body.active,
        },
    )
    .await
    .map_err(db_error)?
    .ok_or_else(|| not_found("Unknown mailbox"))?;
    let response = MailboxResponse::load(updated);
    Ok(Json(DataResponse { data: response }).into_response())
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

    // A blind recipient is told by their own copy naming them; anyone the sender
    // named in To or Cc was addressed openly, whoever else was blind copied. The
    // addresses are compared normalized, the way the mailbox lookup just did.
    let normalized = |values: &[String]| -> Vec<String> {
        values
            .iter()
            .filter_map(|value| address::parse(&bare_address(value)))
            .map(|parsed| parsed.normalized())
            .collect()
    };
    let mut named = normalized(&body.to);
    named.extend(normalized(&body.cc));
    let blind: Vec<String> = normalized(&body.bcc)
        .into_iter()
        .filter(|address| !named.contains(address))
        .collect();
    let is_blind = |address: &str| blind.iter().any(|blind| blind == address);

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

    // The Message-ID is ours to build: mail-builder would fall back to the local
    // hostname, which in a container is the pod name.
    let composed_message_id = compose::message_id(&state.config.hostname);
    let raw = compose::build(&compose::Compose {
        from: &body.from,
        to: &body.to,
        cc: &body.cc,
        bcc: &body.bcc,
        subject: &body.subject,
        text: body.text.as_deref(),
        html: body.html.as_deref(),
        attachments: &attachment_refs,
        message_id: Some(&composed_message_id),
        // A reply keeps its conversation headers, which is what threads it at the
        // recipient and in our own conversation view: the request carries them,
        // and a draft already stored with them is the fallback.
        in_reply_to: body.in_reply_to.as_deref().or_else(|| {
            draft_row
                .as_ref()
                .and_then(|row| row.in_reply_to.as_deref())
        }),
        references: body.references_header.as_deref().or_else(|| {
            draft_row
                .as_ref()
                .and_then(|row| row.references_header.as_deref())
        }),
    })
    .map_err(internal)?;

    let outcome = AuthOutcome::skipped();
    let scheduled_at = body.scheduled_at.filter(|at| *at > chrono::Utc::now());
    let mut stored = 0usize;
    let mut scheduled_copy = None;
    // The copy the sender owns, so the queued job can report its state back.
    let mut copy_message_id = None;

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
            &recipients.join(", "),
            &raw,
            &outcome,
        )
        .await
        .map_err(internal)?;
        copy_message_id = Some(message_id);
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
        // Their own copy: a blind recipient is named by it, nobody else is told.
        let copy = delivery::recipient_copy(&raw, &mailbox.address, is_blind(&mailbox.address));
        delivery::store_local(
            &state,
            mailbox,
            message::Direction::Inbound,
            &sender,
            &mailbox.address,
            &copy,
            &outcome,
        )
        .await
        .map_err(internal)?;
        stored += 1;
    }

    // The copies that leave the cluster: the openly addressed recipients travel
    // together on bytes that name no blind recipient, and each blind recipient
    // travels on a copy that names them alone, which is how they learn they were
    // blind copied without learning about the others.
    let (blind_remote, visible_remote): (Vec<String>, Vec<String>) =
        remote.into_iter().partition(|address| is_blind(address));
    let queued = delivery::enqueue_send(
        &state,
        None,
        delivery::Send {
            envelope_from: &sender,
            raw: &raw,
            visible: &visible_remote,
            blind: &blind_remote,
            not_before: scheduled_at,
            copies: delivery::SendCopies {
                copy_message_id,
                scheduled_message_id: scheduled_copy,
            },
        },
    )
    .await
    .map_err(internal)?;

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
    fn only_renderable_types_preview_inline() {
        assert!(inline_previewable("application/pdf"));
        assert!(inline_previewable("image/png"));
        assert!(inline_previewable("image/jpeg; charset=binary"));
        assert!(inline_previewable("video/mp4"));
        assert!(inline_previewable("audio/mpeg"));
        assert!(inline_previewable("text/plain"));
        // Script-bearing types stay downloads even on a preview request.
        assert!(!inline_previewable("text/html"));
        assert!(!inline_previewable("image/svg+xml"));
        assert!(!inline_previewable("application/xml"));
        assert!(!inline_previewable("application/octet-stream"));
    }

    #[test]
    fn a_list_omits_the_total_when_the_page_was_not_counted() {
        // with_total=false: the page alone, no count and no next page.
        let page = ListResponse::page(vec![1u8], None, 25, None);
        let json = serde_json::to_value(&page).unwrap();
        assert_eq!(json["meta"]["limit"], 25);
        assert!(
            json["meta"].get("total").is_none(),
            "the total is absent when it was not counted: {json}"
        );
        assert!(
            json["meta"].get("next_cursor").is_none(),
            "the last page carries no cursor: {json}"
        );

        let counted = ListResponse::page(vec![1u8], Some(8), 25, None);
        let json = serde_json::to_value(&counted).unwrap();
        assert_eq!(json["meta"]["total"], 8);

        let full = ListResponse::new(vec![1u8, 2]);
        let json = serde_json::to_value(&full).unwrap();
        assert_eq!(json["meta"]["total"], 2, "an unpaginated list still counts");
    }

    #[test]
    fn a_disposition_quotes_the_name_and_encodes_non_ascii() {
        assert_eq!(
            content_disposition("attachment", "note.txt"),
            "attachment; filename=\"note.txt\""
        );
        // A quote in the name cannot break out of the quoted string.
        assert_eq!(
            content_disposition("inline", "a\"b.txt"),
            "inline; filename=\"a\\\"b.txt\""
        );
        assert_eq!(
            content_disposition("inline", "ring\u{f8}.pdf"),
            "inline; filename=\"ring_.pdf\"; filename*=UTF-8''ring%C3%B8.pdf"
        );
    }

    #[test]
    fn the_submission_port_falls_back_when_unparsable() {
        assert_eq!(submission_port("0.0.0.0:2587"), 2587);
        assert_eq!(submission_port("0.0.0.0:587"), 587);
        assert_eq!(submission_port("[::]:587"), 587);
        assert_eq!(submission_port("nonsense"), 587);
    }

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
