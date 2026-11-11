// Handlers return a ready `Response` as their error, which is how axum expects it.
#![allow(clippy::result_large_err)]

use std::sync::Arc;

use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Path, Query, Request, State, rejection::BytesRejection},
    http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use rust_embed::Embed;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::model::{Actor, Entry, ListEntries};
use crate::ops::{self, OpError};
use crate::store::{Store, StoreError};

/// Names the agent behind a CLI or API request. A browser can't send it
/// cross-origin without a preflight hora never answers.
pub const AGENT_HEADER: &str = "x-hora-agent";
const AGENT_PREFIX: &str = "/api/agent";

#[derive(Clone)]
pub struct App {
    pub store: Arc<Store>,
    pub origin: String,
}

#[derive(Embed)]
#[folder = "spa/dist"]
struct Assets;

fn error(status: StatusCode, code: &str, message: &str) -> Response {
    (status, Json(json!({"error": code, "message": message}))).into_response()
}

fn op_error(err: OpError) -> Response {
    match err {
        OpError::Unknown(_) => error(StatusCode::NOT_FOUND, "unknown_operation", &err.to_string()),
        OpError::BadInput(message) => error(StatusCode::BAD_REQUEST, "invalid_request", &message),
        OpError::Store(err) => store_error(err),
    }
}

fn store_error(err: StoreError) -> Response {
    let message = err.to_string();
    match err {
        StoreError::Invalid(_) => error(StatusCode::BAD_REQUEST, "invalid_request", &message),
        StoreError::NotFound => error(StatusCode::NOT_FOUND, "not_found", "Not found."),
        StoreError::Conflict => error(
            StatusCode::CONFLICT,
            "version_conflict",
            "This changed since it was read. Reload it before saving.",
        ),
        StoreError::Forbidden(_) => error(StatusCode::FORBIDDEN, "forbidden", &message),
        StoreError::Duplicate(id) => (
            StatusCode::CONFLICT,
            Json(json!({"error": "duplicate", "message": message, "id": id})),
        )
            .into_response(),
        StoreError::Stale(_) => error(StatusCode::CONFLICT, "stale_proposal", &message),
        StoreError::PendingProposal(id) => (
            StatusCode::CONFLICT,
            Json(json!({"error": "pending_proposal", "message": message, "id": id})),
        )
            .into_response(),
        StoreError::TimerRunning(id) => (
            StatusCode::CONFLICT,
            Json(json!({"error": "timer_running", "message": message, "id": id})),
        )
            .into_response(),
        StoreError::Internal(detail) => {
            eprintln!("hora: internal error: {detail}");
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                "Local data is unavailable.",
            )
        }
    }
}

async fn blocking<T: Send + 'static>(
    job: impl FnOnce() -> T + Send + 'static,
) -> Result<T, Response> {
    tokio::task::spawn_blocking(job).await.map_err(|_| {
        error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "Local data is unavailable.",
        )
    })
}

/// The one place hora decides who is calling. Operator routes are reachable
/// only from the page's own origin (enforced by `guard`); agent routes name
/// their agent in a header. Wave 2 replaces this with Tracon's signed caller.
fn actor_for(path: &str, headers: &HeaderMap) -> Result<Actor, Response> {
    if !is_agent_path(path) {
        return Ok(Actor::Operator);
    }
    let name = headers
        .get(AGENT_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if valid_agent_name(name) {
        Ok(Actor::Agent(name.to_string()))
    } else {
        Err(error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "Agent requests need an X-Hora-Agent header: 1 to 64 letters, digits, dots, dashes or underscores.",
        ))
    }
}

pub fn valid_agent_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn is_agent_path(path: &str) -> bool {
    path == AGENT_PREFIX || path.starts_with("/api/agent/")
}

async fn list_ops() -> Json<Value> {
    Json(Value::Array(
        ops::OPS.iter().map(ops::Op::describe).collect(),
    ))
}

async fn call(
    State(app): State<App>,
    Path(name): Path<String>,
    uri: Uri,
    headers: HeaderMap,
    bytes: Result<Bytes, BytesRejection>,
) -> Response {
    let actor = match actor_for(uri.path(), &headers) {
        Ok(actor) => actor,
        Err(response) => return response,
    };
    let bytes = match bytes {
        Ok(bytes) => bytes,
        Err(rejection) => {
            return error(
                rejection.status(),
                "invalid_request",
                "Request body could not be read or exceeds 64 KiB.",
            );
        }
    };
    let args: Value = if bytes.is_empty() {
        Value::Null
    } else {
        match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(_) => {
                return error(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "Invalid JSON request.",
                );
            }
        }
    };
    let store = app.store.clone();
    match blocking(move || ops::dispatch(&store, &actor, &name, args)).await {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(err)) => op_error(err),
        Err(response) => response,
    }
}

async fn state(State(app): State<App>) -> Response {
    let store = app.store.clone();
    match blocking(move || store.state()).await {
        Ok(Ok(state)) => Json(state).into_response(),
        Ok(Err(err)) => store_error(err),
        Err(response) => response,
    }
}

#[derive(Deserialize)]
struct ExportQuery {
    format: Option<String>,
    from: Option<String>,
    to: Option<String>,
    project: Option<String>,
}

async fn export(State(app): State<App>, Query(query): Query<ExportQuery>) -> Response {
    let format = query.format.unwrap_or_else(|| "csv".into());
    if format != "csv" && format != "jsonl" {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "format is csv or jsonl",
        );
    }
    let filter = ListEntries {
        from: query.from.clone(),
        to: query.to.clone(),
        project: query.project.clone(),
        state: None,
    };
    let store = app.store.clone();
    let entries = match blocking(move || store.entries(&filter)).await {
        Ok(Ok(entries)) => entries,
        Ok(Err(err)) => return store_error(err),
        Err(response) => return response,
    };
    let entries: Vec<Entry> = entries.into_iter().filter(|e| !e.running).collect();
    let (body, content_type) = if format == "csv" {
        (to_csv(&entries), "text/csv; charset=utf-8")
    } else {
        let mut body = String::new();
        for entry in &entries {
            let mut value = serde_json::to_value(entry).unwrap_or(Value::Null);
            if let Value::Object(map) = &mut value {
                map.remove("warnings");
            }
            body.push_str(&value.to_string());
            body.push('\n');
        }
        (body, "application/x-ndjson; charset=utf-8")
    };
    let name = format!(
        "hora-{}-{}.{}",
        entries.first().map(|e| e.date.as_str()).unwrap_or("empty"),
        entries.last().map(|e| e.date.as_str()).unwrap_or("empty"),
        format
    );
    (
        [
            (header::CONTENT_TYPE, content_type.to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{name}\""),
            ),
        ],
        body,
    )
        .into_response()
}

pub fn to_csv(entries: &[Entry]) -> String {
    fn cell(value: &str) -> String {
        if value.contains([',', '"', '\n', '\r']) {
            format!("\"{}\"", value.replace('"', "\"\""))
        } else {
            value.to_string()
        }
    }
    let mut out = String::from(
        "id,date,project,started_at,ended_at,minutes,hours,billable,state,author,description,refs\n",
    );
    for e in entries {
        let minutes = e.minutes.unwrap_or(0);
        let row = [
            e.id.clone(),
            e.date.clone(),
            e.project.clone(),
            e.started_at.clone().unwrap_or_default(),
            e.ended_at.clone().unwrap_or_default(),
            minutes.to_string(),
            format!("{:.2}", minutes as f64 / 60.0),
            e.billable.to_string(),
            e.state.as_str().to_string(),
            e.author.clone(),
            e.description.clone(),
            e.refs.join(" "),
        ];
        out.push_str(&row.iter().map(|v| cell(v)).collect::<Vec<_>>().join(","));
        out.push('\n');
    }
    out
}

async fn asset(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    match Assets::get(path) {
        Some(file) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            (
                [(header::CONTENT_TYPE, mime.as_ref().to_string())],
                file.data,
            )
                .into_response()
        }
        None => error(StatusCode::NOT_FOUND, "not_found", "Not found."),
    }
}

async fn fallback(uri: Uri) -> Response {
    if uri.path() == "/api" || uri.path().starts_with("/api/") {
        error(StatusCode::NOT_FOUND, "not_found", "Not found.")
    } else if Assets::get(uri.path().trim_start_matches('/')).is_some() {
        asset(uri).await
    } else {
        // Client-side routes all load the app shell.
        asset(Uri::from_static("/")).await
    }
}

fn json_content(request: &Request) -> bool {
    request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.eq_ignore_ascii_case("application/json")
                || v.to_ascii_lowercase().starts_with("application/json;")
        })
}

/// Operator routes accept mutations only from the page's own origin. Agent
/// routes accept no browser at all: no Origin header, and a custom header a
/// cross-origin page can't send without a preflight.
async fn guard(State(app): State<App>, request: Request, next: Next) -> Response {
    let path = request.uri().path();
    let is_api = path.starts_with("/api");
    let is_agent = is_agent_path(path);
    let ipv4_host = app.origin.strip_prefix("http://").unwrap_or("");
    let port = ipv4_host.strip_prefix("127.0.0.1:").unwrap_or("");
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok());
    let ipv4 = host == Some(ipv4_host);
    let localhost = host.and_then(|value| value.strip_prefix("localhost:")) == Some(port);
    let allowed_host = ipv4 || localhost;
    let mutation = !matches!(*request.method(), Method::GET | Method::HEAD);
    let origin = request
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok());
    let allowed_origin = (ipv4 && origin == Some(app.origin.as_str()))
        || (localhost
            && origin.and_then(|value| value.strip_prefix("http://localhost:")) == Some(port));
    let has_origin = request.headers().contains_key(header::ORIGIN);
    let refused = if !allowed_host {
        Some("Use the local hora URL.")
    } else if is_agent && has_origin {
        Some("The agent API takes requests from the hora CLI or an agent, not from browsers.")
    } else if is_agent && !request.headers().contains_key(AGENT_HEADER) {
        Some("Agent requests need an X-Hora-Agent header.")
    } else if !is_agent && ((has_origin && !allowed_origin) || (mutation && !allowed_origin)) {
        Some("Use the local hora URL.")
    } else {
        None
    };
    let mut response = if let Some(message) = refused {
        error(StatusCode::FORBIDDEN, "forbidden", message)
    } else if mutation && is_api && !json_content(&request) {
        error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "JSON Content-Type is required.",
        )
    } else {
        next.run(request).await
    };
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    if is_api {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    response
}

pub fn router(app: App) -> Router {
    Router::new()
        .route("/api/state", get(state))
        .route("/api/ops", get(list_ops))
        .route("/api/ops/{name}", post(call))
        .route("/api/export", get(export))
        .route("/api/agent/ops", get(list_ops))
        .route("/api/agent/ops/{name}", post(call))
        .fallback(fallback)
        .route("/", get(asset))
        .route("/assets/{*path}", get(asset))
        .layer(DefaultBodyLimit::max(64 * 1024))
        .layer(middleware::from_fn_with_state(app.clone(), guard))
        .with_state(app)
}
