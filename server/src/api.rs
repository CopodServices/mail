use crate::delivery;
use crate::state::AppState;
use axum::Json;
use axum::body::Body;
use axum::extract::{FromRequestParts, Path, Query, State};
use axum::http::request::Parts;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use entity::message;
use mailcore::policy::AuthOutcome;
use mailcore::{address, dkim, repo};
use serde::{Deserialize, Serialize};
use utoipa::{OpenApi, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_scalar::{Scalar, Servable};
use uuid::Uuid;

#[derive(OpenApi)]
#[openapi(
    tags(
        (name = "messages", description = "Stored mail"),
        (name = "mailboxes", description = "Mailboxes"),
        (name = "domains", description = "Domains and DKIM records"),
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
        .merge(messages_router())
        .merge(mailboxes_router())
        .merge(domains_router())
        .merge(send_router())
        .with_state(state)
        .split_for_parts();

    router
        .route("/healthz", get(health))
        .route("/readyz", get(ready))
        .merge(Scalar::with_url("/scalar", api.clone()))
        .route("/scalar/openapi.json", get(Json(api)))
}

fn messages_router() -> OpenApiRouter<AppState> {
    use utoipa_axum::routes;
    OpenApiRouter::new()
        .routes(routes!(list_messages))
        .routes(routes!(get_message))
        .routes(routes!(get_message_raw))
        .routes(routes!(delete_message))
}

fn mailboxes_router() -> OpenApiRouter<AppState> {
    use utoipa_axum::routes;
    OpenApiRouter::new()
        .routes(routes!(list_mailboxes))
        .routes(routes!(create_mailbox))
        .routes(routes!(delete_mailbox))
}

fn domains_router() -> OpenApiRouter<AppState> {
    use utoipa_axum::routes;
    OpenApiRouter::new()
        .routes(routes!(list_domains))
        .routes(routes!(create_domain))
        .routes(routes!(domain_dkim))
}

fn send_router() -> OpenApiRouter<AppState> {
    use utoipa_axum::routes;
    OpenApiRouter::new().routes(routes!(send))
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

/// Bearer API key required on every /api route (skipped when API_KEY is unset).
pub struct ApiKey;

impl FromRequestParts<AppState> for ApiKey {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let Some(expected) = state.config.api_key.as_deref() else {
            return Ok(ApiKey);
        };
        let provided = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .map(str::trim);

        match provided {
            Some(value) if constant_time_eq(value.as_bytes(), expected.as_bytes()) => Ok(ApiKey),
            _ => Err(ApiError::new(
                StatusCode::UNAUTHORIZED,
                "Unauthorized",
                "Missing or invalid API key",
            )
            .code("unauthorized")),
        }
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
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
fn db_error(err: anyhow::Error) -> ApiError {
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
            meta: Meta { total },
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
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct MailboxResponse {
    pub id: Uuid,
    pub address: String,
    pub active: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl From<entity::mailbox::Model> for MailboxResponse {
    fn from(model: entity::mailbox::Model) -> Self {
        Self {
            id: model.id,
            address: model.address,
            active: model.active,
            created_at: model.created_at,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DomainResponse {
    pub id: Uuid,
    pub name: String,
    pub dkim_selector: String,
    pub active: bool,
}

impl From<entity::domain::Model> for DomainResponse {
    fn from(model: entity::domain::Model) -> Self {
        Self {
            id: model.id,
            name: model.name,
            dkim_selector: model.dkim_selector,
            active: model.active,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DkimResponse {
    pub name: String,
    pub selector: String,
    pub record_name: String,
    pub record_value: String,
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
    pub subject: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub html: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SendResponse {
    pub stored: usize,
    pub queued: usize,
}

#[derive(Debug, Deserialize)]
pub struct MessageQuery {
    pub mailbox: Option<String>,
    pub limit: Option<u64>,
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
        ("limit" = Option<u64>, Query, description = "Maximum rows (default 50)")
    ),
    responses((status = 200, description = "Stored messages", body = ListResponse<MessageResponse>))
)]
pub async fn list_messages(
    State(state): State<AppState>,
    _key: ApiKey,
    Query(query): Query<MessageQuery>,
) -> Result<Response, ApiError> {
    let limit = query.limit.unwrap_or(50);
    let messages = repo::list_messages(&state.db, query.mailbox.as_deref(), limit)
        .await
        .map_err(internal)?;

    let payload = messages.into_iter().map(MessageResponse::from).collect();
    Ok(ListResponse::new(payload).into_response())
}

#[utoipa::path(
    get,
    path = "/api/messages/{id}",
    tag = "messages",
    responses(
        (status = 200, description = "Message metadata", body = MessageResponse),
        (status = 404, description = "Unknown message")
    )
)]
pub async fn get_message(
    State(state): State<AppState>,
    _key: ApiKey,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let message = repo::get_message(&state.db, id)
        .await
        .map_err(db_error)?
        .ok_or_else(|| not_found("Unknown message"))?;
    Ok((
        StatusCode::OK,
        Json(DataResponse {
            data: MessageResponse::from(message),
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
    _key: ApiKey,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let message = repo::get_message(&state.db, id)
        .await
        .map_err(db_error)?
        .ok_or_else(|| not_found("Unknown message"))?;
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
    _key: ApiKey,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let message = repo::delete_message(&state.db, id)
        .await
        .map_err(db_error)?
        .ok_or_else(|| not_found("Unknown message"))?;

    if let Err(err) = state.store.delete(&message.storage_key).await {
        tracing::warn!(error = %err, key = %message.storage_key, "failed to remove the stored object");
    }

    Ok(StatusCode::NO_CONTENT.into_response())
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
    _key: ApiKey,
) -> Result<Response, ApiError> {
    let mailboxes = repo::list_mailboxes(&state.db).await.map_err(db_error)?;
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
    _key: ApiKey,
    Json(body): Json<CreateMailbox>,
) -> Result<Response, ApiError> {
    if address::parse(&body.address).is_none() {
        return Err(bad_request("Invalid email address"));
    }
    if body.password.is_empty() {
        return Err(bad_request("Password must not be empty"));
    }

    let mailbox = repo::create_mailbox(&state.db, &body.address, &body.password, &state.secrets)
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
    _key: ApiKey,
    Path(address): Path<String>,
) -> Result<Response, ApiError> {
    let deleted = repo::delete_mailbox(&state.db, &address)
        .await
        .map_err(db_error)?;
    if !deleted {
        return Err(not_found("Unknown mailbox"));
    }
    Ok(StatusCode::NO_CONTENT.into_response())
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
    _key: ApiKey,
) -> Result<Response, ApiError> {
    let domains = repo::list_domains(&state.db).await.map_err(db_error)?;
    let payload = domains.into_iter().map(DomainResponse::from).collect();
    Ok(ListResponse::new(payload).into_response())
}

#[utoipa::path(
    post,
    path = "/api/domains",
    tag = "domains",
    request_body = CreateDomain,
    responses((status = 201, description = "Domain created with a DKIM key", body = DomainResponse))
)]
pub async fn create_domain(
    State(state): State<AppState>,
    _key: ApiKey,
    Json(body): Json<CreateDomain>,
) -> Result<Response, ApiError> {
    let name = body.name.trim().to_ascii_lowercase();
    if name.is_empty() || !name.contains('.') {
        return Err(bad_request("Invalid domain name"));
    }

    let domain = repo::ensure_domain(&state.db, &name, &state.secrets)
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
    path = "/api/domains/{name}/dkim",
    tag = "domains",
    responses(
        (status = 200, description = "DKIM record to publish in DNS", body = DkimResponse),
        (status = 404, description = "Unknown domain")
    )
)]
pub async fn domain_dkim(
    State(state): State<AppState>,
    _key: ApiKey,
    Path(name): Path<String>,
) -> Result<Response, ApiError> {
    let domain = repo::find_domain(&state.db, &name)
        .await
        .map_err(db_error)?
        .ok_or_else(|| not_found("Unknown domain"))?;

    let payload = DkimResponse {
        name: domain.name.clone(),
        selector: domain.dkim_selector.clone(),
        record_name: dkim::txt_record_name(&domain.dkim_selector, &domain.name),
        record_value: dkim::txt_record_value(&domain.dkim_public_key),
    };
    Ok((StatusCode::OK, Json(DataResponse { data: payload })).into_response())
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
    _key: ApiKey,
    Json(body): Json<SendRequest>,
) -> Result<Response, ApiError> {
    if address::parse(&body.from).is_none() {
        return Err(bad_request("Invalid from address"));
    }
    if body.to.is_empty() {
        return Err(bad_request("At least one recipient is required"));
    }

    let mut local = Vec::new();
    let mut remote = Vec::new();
    for recipient in &body.to {
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

    let raw = mailcore::compose::simple(
        &body.from,
        &body.to,
        &body.subject,
        body.text.as_deref(),
        body.html.as_deref(),
    )
    .map_err(internal)?;

    let outcome = AuthOutcome::skipped();
    let mut stored = 0usize;
    for mailbox in &local {
        delivery::store_local(
            &state,
            mailbox,
            message::Direction::Outbound,
            &body.from,
            &raw,
            &outcome,
        )
        .await
        .map_err(internal)?;
        stored += 1;
    }

    let mut queued = 0usize;
    if !remote.is_empty() {
        delivery::enqueue_remote(&state, None, &body.from, &remote, &raw)
            .await
            .map_err(internal)?;
        queued = remote.len();
    }

    Ok((
        StatusCode::ACCEPTED,
        Json(DataResponse {
            data: SendResponse { stored, queued },
        }),
    )
        .into_response())
}
