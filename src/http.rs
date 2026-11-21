// Handlers return a ready `Response` as their error, which is how axum expects it.
#![allow(clippy::result_large_err)]

use std::sync::Arc;

use axum::{
    Json, Router,
    body::Bytes,
    extract::{
        DefaultBodyLimit, Extension, Path, Query, Request, State, rejection::BytesRejection,
    },
    http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use rust_embed::Embed;
use serde_json::{Map, Value, json};

use crate::model::{Actor, Writer};
use crate::ops::{self, OpError};
use crate::store::{Store, StoreError};

/// Names the agent behind a CLI or API request. A browser can't send it
/// cross-origin without a preflight hora never answers.
pub const AGENT_HEADER: &str = "x-hora-agent";
const AGENT_PREFIX: &str = "/api/agent";
/// The largest JSON body a handler reads.
pub(crate) const BODY_LIMIT: usize = 64 * 1024;

#[derive(Clone)]
pub struct App {
    pub store: Arc<Store>,
    pub origin: String,
}

#[derive(Embed)]
#[folder = "spa/dist"]
struct Assets;

pub(crate) fn error(status: StatusCode, code: &str, message: &str) -> Response {
    (status, Json(json!({"error": code, "message": message}))).into_response()
}

fn body(code: &str, message: &str) -> Value {
    json!({"error": code, "message": message})
}

fn with_id(code: &str, message: &str, id: String) -> Value {
    json!({"error": code, "message": message, "id": id})
}

/// An operation's failure as its HTTP status and JSON body. The MCP tools
/// answer with the same body.
pub(crate) fn op_error_body(err: OpError) -> (StatusCode, Value) {
    match err {
        OpError::Unknown(_) => (
            StatusCode::NOT_FOUND,
            body("unknown_operation", &err.to_string()),
        ),
        OpError::BadInput(message) => (StatusCode::BAD_REQUEST, body("invalid_request", &message)),
        OpError::Store(err) => store_error_body(err),
    }
}

fn store_error_body(err: StoreError) -> (StatusCode, Value) {
    let message = err.to_string();
    match err {
        StoreError::Invalid(_) => (StatusCode::BAD_REQUEST, body("invalid_request", &message)),
        StoreError::NotFound => (StatusCode::NOT_FOUND, body("not_found", "Not found.")),
        StoreError::Conflict => (
            StatusCode::CONFLICT,
            body(
                "version_conflict",
                "This changed since it was read. Reload it before saving.",
            ),
        ),
        StoreError::Forbidden(_) => (StatusCode::FORBIDDEN, body("forbidden", &message)),
        StoreError::Duplicate(id) => (StatusCode::CONFLICT, with_id("duplicate", &message, id)),
        StoreError::Stale(_) => (StatusCode::CONFLICT, body("stale_proposal", &message)),
        StoreError::PendingProposal(id) => (
            StatusCode::CONFLICT,
            with_id("pending_proposal", &message, id),
        ),
        StoreError::TimerRunning(id) => {
            (StatusCode::CONFLICT, with_id("timer_running", &message, id))
        }
        StoreError::Internal(detail) => {
            eprintln!("hora: internal error: {detail}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                body("internal_error", "Local data is unavailable."),
            )
        }
    }
}

fn op_error(err: OpError) -> Response {
    let (status, body) = op_error_body(err);
    (status, Json(body)).into_response()
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

/// The one place hora's HTTP API decides who is calling. Operator routes are
/// reachable only from the page's own origin (enforced by `guard`), or, as a
/// Tracon plugin, only with the node's signed context for the operator
/// (`plugin::guard`); agent routes name their agent in a header. An MCP tool
/// call takes its caller from the node's signed context instead
/// (`contract::Caller::actor`).
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

/// The Tracon session behind an operator request on the plugin socket, put
/// there by `plugin::guard` from the node's signed context once it checks
/// out. Nothing else sets it, so standalone and agent-API writes record no
/// session.
#[derive(Clone, Debug)]
pub(crate) struct AttestedSession(pub String);

pub fn valid_agent_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

pub(crate) fn is_agent_path(path: &str) -> bool {
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
    session: Option<Extension<AttestedSession>>,
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
    let session = session.map(|Extension(AttestedSession(id))| id);
    let job = move || {
        let by = Writer {
            actor: &actor,
            session: session.as_deref(),
        };
        ops::dispatch(&store, by, &name, args)
    };
    match blocking(job).await {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(err)) => op_error(err),
        Err(response) => response,
    }
}

/// A read-only operation over `GET`, its arguments from the query string, as
/// the caller `actor_for` decides. `download` serves its result, an
/// `Export`, as the file it describes.
async fn get_op(
    app: App,
    name: &'static str,
    download: bool,
    uri: Uri,
    headers: HeaderMap,
    query: Map<String, Value>,
) -> Response {
    // GET skips the Origin check mutations get, so a writing operation
    // never answers one.
    if ops::find(name).is_none_or(|op| op.writes) {
        return error(
            StatusCode::METHOD_NOT_ALLOWED,
            "invalid_request",
            "Only read operations answer GET.",
        );
    }
    let actor = match actor_for(uri.path(), &headers) {
        Ok(actor) => actor,
        Err(response) => return response,
    };
    let store = app.store.clone();
    let value =
        match blocking(move || ops::dispatch(&store, &actor, name, Value::Object(query))).await {
            Ok(Ok(value)) => value,
            Ok(Err(err)) => return op_error(err),
            Err(response) => return response,
        };
    if !download {
        return Json(value).into_response();
    }
    let field = |key: &str| value[key].as_str().unwrap_or_default().to_string();
    (
        [
            (header::CONTENT_TYPE, field("content_type")),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{}\"", field("filename")),
            ),
        ],
        field("content"),
    )
        .into_response()
}

/// What a route of the HTTP API serves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Serves {
    /// `POST`: the operation the path's `{name}` names, its arguments the
    /// JSON body. Every operation in `ops::OPS`.
    AnyOp,
    /// `GET`: the operation registry, each operation with its schema.
    Registry,
    /// `GET`: one read-only operation, its arguments the query string.
    Op(&'static str),
    /// `GET`: one read-only operation whose result is a file, downloaded.
    Download(&'static str),
}

pub struct Route {
    pub path: &'static str,
    pub serves: Serves,
}

/// Every route of the HTTP API, and the operation behind it. The routers are
/// built from this table alone, so a route is either an operation (and so,
/// unless it is operator-only, an MCP tool) or listed here as what it is.
/// `tests/parity.rs` holds the table, the tools and the page to that.
/// Routes under `/api/agent` are the agent API; the rest are the operator's.
pub const ROUTES: &[Route] = &[
    Route {
        path: "/api/state",
        serves: Serves::Op("time_overview"),
    },
    Route {
        path: "/api/ops",
        serves: Serves::Registry,
    },
    Route {
        path: "/api/ops/{name}",
        serves: Serves::AnyOp,
    },
    Route {
        path: "/api/export",
        serves: Serves::Download("time_export"),
    },
    Route {
        path: "/api/agent/ops",
        serves: Serves::Registry,
    },
    Route {
        path: "/api/agent/ops/{name}",
        serves: Serves::AnyOp,
    },
];

/// The routes of `ROUTES` on the agent API (`agent`) or the operator's.
fn api(agent: bool) -> Router<App> {
    ROUTES
        .iter()
        .filter(|route| is_agent_path(route.path) == agent)
        .fold(Router::new(), |router, route| {
            let method = match route.serves {
                Serves::AnyOp => post(call),
                Serves::Registry => get(list_ops),
                Serves::Op(name) | Serves::Download(name) => {
                    let download = matches!(route.serves, Serves::Download(_));
                    get(
                        move |State(app): State<App>,
                              uri: Uri,
                              headers: HeaderMap,
                              Query(query): Query<Map<String, Value>>| {
                            get_op(app, name, download, uri, headers, query)
                        },
                    )
                }
            };
            router.route(route.path, method)
        })
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

pub(crate) fn json_content(request: &Request) -> bool {
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

/// The operator's API: the routes hora's page uses. Every change made here is
/// the operator's, so whatever serves it must first establish that the
/// request is the operator's (the page's own origin standalone, the node's
/// signed context as a plugin).
pub fn operator_api() -> Router<App> {
    api(false)
}

/// The agent API, for the hora CLI: the agent names itself in a header.
fn agent_api() -> Router<App> {
    api(true)
}

/// A JSON 404 for any path a router does not serve.
pub(crate) async fn not_found() -> Response {
    error(StatusCode::NOT_FOUND, "not_found", "Not found.")
}

/// Standalone `hora serve`: the page, the operator's API and the agent API on
/// one loopback port.
pub fn router(app: App) -> Router {
    operator_api()
        .merge(agent_api())
        .fallback(fallback)
        .route("/", get(asset))
        .route("/assets/{*path}", get(asset))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .layer(middleware::from_fn_with_state(app.clone(), guard))
        .with_state(app)
}

/// Only the agent API, with the same checks as standalone, for plugin mode's
/// `agent_listen`: neither the operator's routes nor the page are on it.
pub fn agent_router(app: App) -> Router {
    agent_api()
        .fallback(not_found)
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .layer(middleware::from_fn_with_state(app.clone(), guard))
        .with_state(app)
}
