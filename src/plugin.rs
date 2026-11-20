//! `hora serve --socket <path> --data <dir>`: hora as a Tracon plugin
//! (contract v1). The node starts it, serves hora's page from the plugin
//! archive itself, and talks to it over the Unix socket in two ways:
//!
//! - `POST /` is MCP. hora offers every operation an agent may run as a tool
//!   (`ops::OPS` without the operator-only ones). The node signs who made
//!   each call into `params._meta.tracon`, so the author hora records is the
//!   harness the node names, not a name the agent picked. An agent's entry is
//!   still a draft and its change to a confirmed entry still a proposal: the
//!   operator confirms them in hora, so a tool call needs no approval of the
//!   node's own.
//! - `/api/*` is the operator's API, for the page. The node forwards the
//!   page's requests with a context it signed for the operator; that context
//!   is what makes a request the operator's, and it replaces the standalone
//!   server's Origin and Host checks. The agent HTTP API is never served
//!   here: a request on this path is attested as the operator, so an agent
//!   route would turn an agent's write into the operator's.
//!
//! Agents that use the hora CLI rather than the tools keep working against a
//! separate loopback port, `agent_listen`, when `plugin.toml` names one.
//!
//! The node starts hora with an empty environment (no `HOME`, no `PATH`), so
//! everything else comes from `<data>/plugin.toml`.

use std::net::{Ipv4Addr, SocketAddr};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, Request, State},
    http::{HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::post,
};
use base64::Engine;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::watch;

use crate::contract::{self, ContractEnv, NonceCache, UiRequest};
use crate::http::{self, App};
use crate::ops;
use crate::store::Store;

/// The contract version this build serves.
pub const CONTRACT: &str = "1";
/// The header the node puts its signed context in.
pub const CONTEXT_HEADER: &str = "tracon-context";
/// The most the node forwards; the handlers themselves take far less.
const MAX_FORWARDED_BODY: usize = 1024 * 1024;
/// The MCP revision hora answers with when a client names none.
const PROTOCOL_VERSION: &str = "2025-06-18";
/// JSON-RPC error code for a call its caller context refuses.
pub const REFUSED: i64 = -32001;

/// `<data>/plugin.toml`, the channel's own configuration. Optional.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginConfig {
    /// The directory holding `hora.sqlite3`. Default: the data directory
    /// itself. A relative path is from the data directory.
    pub data_dir: Option<PathBuf>,
    /// `127.0.0.1:<port>` to serve the agent API on, for the hora CLI.
    /// Default: none, so agents reach this instance only through its tools.
    pub agent_listen: Option<SocketAddr>,
}

impl PluginConfig {
    pub fn load(data: &Path) -> Result<Self> {
        let path = data.join("plugin.toml");
        if !path.exists() {
            return Ok(Self::default());
        }
        let body = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let config: Self =
            toml::from_str(&body).with_context(|| format!("parsing {}", path.display()))?;
        // The agent API's own checks expect the standalone origin,
        // http://127.0.0.1:<port>.
        if let Some(addr) = config.agent_listen
            && addr.ip() != Ipv4Addr::LOCALHOST
        {
            bail!("agent_listen must be 127.0.0.1:<port>, not {addr}");
        }
        Ok(config)
    }

    /// Where the database lives.
    pub fn store_dir(&self, data: &Path) -> PathBuf {
        match &self.data_dir {
            Some(path) => data.join(path),
            None => data.to_path_buf(),
        }
    }
}

/// What every request on the socket is checked against.
pub struct Auth {
    pub env: ContractEnv,
    pub nonces: NonceCache,
}

/// Check the start environment: the contract's variables, and nothing else.
pub fn auth(get: impl Fn(&str) -> Option<String>) -> Result<Auth, String> {
    let token = get("TRACON_PLUGIN_TOKEN")
        .filter(|t| !t.is_empty())
        .ok_or("TRACON_PLUGIN_TOKEN is not set; serve --socket is started by a Tracon node")?;
    let key = get("TRACON_NODE_KEY")
        .filter(|k| !k.is_empty())
        .ok_or("TRACON_NODE_KEY is not set; serve --socket is started by a Tracon node")?;
    if let Some(v) = get("TRACON_CONTRACT").filter(|v| v != CONTRACT) {
        return Err(format!(
            "TRACON_CONTRACT is {v}; this build serves contract {CONTRACT}"
        ));
    }
    Ok(Auth {
        env: ContractEnv::new(&token, &key)?,
        nonces: NonceCache::default(),
    })
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

fn context_of(request: &axum::http::request::Parts) -> Option<Value> {
    let header = request.headers.get(CONTEXT_HEADER)?.to_str().ok()?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(header.trim())
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// The socket's gate. `POST /` is MCP, whose tool calls carry their own
/// context. `/api/agent/*` is refused outright. Any other `/api` request runs
/// only with a valid operator context for exactly that request; nothing else
/// is served.
async fn guard(State(auth): State<Arc<Auth>>, request: Request, next: Next) -> Response {
    let path = request.uri().path().to_string();
    if path == "/" {
        return next.run(request).await;
    }
    if http::is_agent_path(&path) {
        return http::error(
            StatusCode::FORBIDDEN,
            "forbidden",
            "The agent API is not served on the plugin socket, where every request is the \
             operator's. Agents use hora's tools, or the hora CLI against `agent_listen`.",
        );
    }
    if path != "/api" && !path.starts_with("/api/") {
        return http::not_found().await;
    }
    let (parts, body) = request.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, MAX_FORWARDED_BODY).await else {
        return http::error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "invalid_request",
            "Request body could not be read or exceeds 1 MiB.",
        );
    };
    let Some(context) = context_of(&parts) else {
        return http::error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "The request carries no readable tracon-context.",
        );
    };
    let target = parts
        .uri
        .path_and_query()
        .map_or(path.as_str(), |pq| pq.as_str());
    let checked = contract::verify_ui(
        &context,
        &UiRequest {
            method: parts.method.as_str(),
            target,
            body: &bytes,
        },
        &auth.env,
        now_ms(),
        &auth.nonces,
    );
    if let Err(refusal) = checked {
        eprintln!(
            "hora: refused {} {path}: {} (check {})",
            parts.method, refusal.reason, refusal.check
        );
        return http::error(StatusCode::UNAUTHORIZED, "unauthorized", &refusal.reason);
    }
    let request = Request::from_parts(parts, Body::from(bytes));
    let mutation = !matches!(*request.method(), Method::GET | Method::HEAD);
    let mut response = if mutation && !http::json_content(&request) {
        http::error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "JSON Content-Type is required.",
        )
    } else {
        next.run(request).await
    };
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

/// The operations offered as tools: every one an agent may run.
pub fn tool_ops() -> impl Iterator<Item = &'static ops::Op> {
    ops::OPS.iter().filter(|op| !op.operator_only)
}

/// `tools/list`: each tool's name, description and input schema, from the
/// operation registry.
pub fn tools() -> Vec<Value> {
    tool_ops()
        .map(|op| {
            let mut schema = serde_json::to_value(op.input_schema()).unwrap_or_else(|_| json!({}));
            if let Some(o) = schema.as_object_mut() {
                o.remove("$schema");
                o.entry("type").or_insert_with(|| json!("object"));
            }
            json!({
                "name": op.name,
                "description": op.description,
                "inputSchema": schema,
                "annotations": { "readOnlyHint": !op.writes, "openWorldHint": false },
            })
        })
        .collect()
}

/// A tool's answer.
fn tool_ok(v: &Value) -> Value {
    let text = serde_json::to_string_pretty(v).unwrap_or_default();
    json!({ "content": [{ "type": "text", "text": text }], "isError": false })
}

/// A tool that ran and could not do what it was asked: hora's error body, as
/// the HTTP API gives it, for the caller to read and act on.
fn tool_error(body: &Value) -> Value {
    let text = serde_json::to_string_pretty(body).unwrap_or_default();
    json!({ "content": [{ "type": "text", "text": text }], "isError": true })
}

struct RpcError {
    code: i64,
    message: String,
    data: Option<Value>,
}

impl RpcError {
    fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }
}

fn rpc_error(id: Value, e: RpcError) -> Value {
    let mut error = json!({"code": e.code, "message": e.message});
    if let Some(data) = e.data {
        error["data"] = data;
    }
    json!({"jsonrpc": "2.0", "id": id, "error": error})
}

/// What MCP calls run against.
#[derive(Clone)]
struct Mcp {
    store: Arc<Store>,
    auth: Arc<Auth>,
}

impl Mcp {
    /// A `tools/call`, checked against its signed context and run as the
    /// caller the node names.
    async fn call(&self, params: &Value) -> Result<Value, RpcError> {
        let name = params["name"]
            .as_str()
            .ok_or_else(|| RpcError::new(-32602, "tools/call needs a tool name"))?;
        if !tool_ops().any(|op| op.name == name) {
            return Err(RpcError::new(-32602, format!("hora has no tool {name}")));
        }
        let arguments = params.get("arguments");
        let meta = params
            .get("_meta")
            .and_then(|m| m.get("tracon"))
            .unwrap_or(&Value::Null);
        let caller = contract::verify_call(
            meta,
            name,
            arguments,
            &self.auth.env,
            now_ms(),
            &self.auth.nonces,
        )
        .map_err(|r| {
            eprintln!(
                "hora: refused tool {name}: {} (check {})",
                r.reason, r.check
            );
            RpcError {
                code: REFUSED,
                message: format!("refused: {}", r.reason),
                data: Some(json!({"check": r.check})),
            }
        })?;
        let actor = caller.actor();
        let args = arguments.cloned().unwrap_or_else(|| json!({}));
        let store = self.store.clone();
        let name = name.to_string();
        let ran = tokio::task::spawn_blocking(move || ops::dispatch(&store, &actor, &name, args))
            .await
            .map_err(|_| RpcError::new(-32603, "the call failed"))?;
        Ok(match ran {
            Ok(value) => tool_ok(&value),
            Err(err) => tool_error(&http::op_error_body(err).1),
        })
    }

    /// Answer one JSON-RPC message; `None` for a notification or a response.
    async fn answer(&self, msg: &Value) -> Option<Value> {
        let Some(method) = msg.get("method").and_then(Value::as_str) else {
            if msg.get("result").is_some() || msg.get("error").is_some() {
                return None;
            }
            let id = msg.get("id").cloned().unwrap_or(Value::Null);
            return Some(rpc_error(id, RpcError::new(-32600, "invalid request")));
        };
        let id = msg.get("id").cloned()?;
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let result = match method {
            "initialize" => {
                let asked = params["protocolVersion"]
                    .as_str()
                    .unwrap_or(PROTOCOL_VERSION);
                Ok(json!({
                    "protocolVersion": asked,
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": contract::PLUGIN, "version": env!("CARGO_PKG_VERSION")},
                }))
            }
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tools()})),
            "tools/call" => self.call(&params).await,
            other => Err(RpcError::new(-32601, format!("no method {other}"))),
        };
        Some(match result {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err(e) => rpc_error(id, e),
        })
    }
}

/// MCP: one JSON-RPC message per POST, its answer as JSON, 202 for a
/// notification.
async fn mcp(State(mcp): State<Mcp>, body: Bytes) -> Response {
    let msg: Value = match serde_json::from_slice(&body) {
        Ok(msg) => msg,
        Err(e) => {
            let answer = rpc_error(
                Value::Null,
                RpcError::new(-32700, format!("parse error: {e}")),
            );
            return (StatusCode::BAD_REQUEST, Json(answer)).into_response();
        }
    };
    match mcp.answer(&msg).await {
        Some(answer) => Json(answer).into_response(),
        None => StatusCode::ACCEPTED.into_response(),
    }
}

/// The socket's router: MCP and the operator's API, nothing else.
pub fn router(app: App, auth: Arc<Auth>) -> Router {
    let state = Mcp {
        store: app.store.clone(),
        auth: auth.clone(),
    };
    http::operator_api()
        .with_state(app)
        .route("/", post(mcp).with_state(state))
        .fallback(http::not_found)
        .layer(DefaultBodyLimit::max(http::BODY_LIMIT))
        .layer(middleware::from_fn_with_state(auth, guard))
}

fn bind_socket(socket: &Path) -> Result<tokio::net::UnixListener> {
    match std::fs::symlink_metadata(socket) {
        Ok(m) if m.file_type().is_socket() => std::fs::remove_file(socket)
            .with_context(|| format!("removing the stale socket {}", socket.display()))?,
        Ok(_) => bail!(
            "{} exists and is not a socket; not replacing it",
            socket.display()
        ),
        Err(_) => {}
    }
    let listener = tokio::net::UnixListener::bind(socket)
        .with_context(|| format!("binding {}", socket.display()))?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("restricting {}", socket.display()))?;
    Ok(listener)
}

async fn signalled() {
    let Ok(mut term) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
    else {
        let _ = tokio::signal::ctrl_c().await;
        return;
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
}

pub async fn serve(socket: &Path, data: &Path) -> Result<()> {
    let auth = Arc::new(auth(|k| std::env::var(k).ok()).map_err(anyhow::Error::msg)?);
    let config = PluginConfig::load(data)?;
    let store_dir = config.store_dir(data);
    let store = Arc::new(Store::open(&store_dir)?);
    // The socket has no origin: the node's signed context gates it.
    let app = App {
        store,
        origin: String::new(),
    };
    let agent = match config.agent_listen {
        Some(addr) => {
            let listener = tokio::net::TcpListener::bind(addr).await.with_context(|| {
                format!("binding agent_listen {addr} (is a standalone hora serve still on it?)")
            })?;
            let bound = listener.local_addr()?;
            let agent_app = App {
                origin: format!("http://{bound}"),
                ..app.clone()
            };
            eprintln!("hora: agent API on http://{bound}/api/agent");
            Some((listener, agent_app))
        }
        None => None,
    };
    let listener = bind_socket(socket)?;
    eprintln!(
        "hora: plugin on {} ({})",
        socket.display(),
        store_dir.display()
    );

    let (stop, stopped) = watch::channel(false);
    tokio::spawn(async move {
        signalled().await;
        let _ = stop.send(true);
    });
    let until_stopped = |mut rx: watch::Receiver<bool>| async move {
        let _ = rx.wait_for(|stop| *stop).await;
    };
    let agent_task = agent.map(|(listener, app)| {
        let serving = axum::serve(listener, http::agent_router(app))
            .with_graceful_shutdown(until_stopped(stopped.clone()));
        tokio::spawn(async move { serving.await })
    });
    let served = axum::serve(listener, router(app, auth))
        .with_graceful_shutdown(until_stopped(stopped))
        .await;
    let _ = std::fs::remove_file(socket);
    if let Some(task) = agent_task {
        if served.is_err() {
            task.abort();
        } else {
            task.await.context("the agent API task")??;
        }
    }
    served.context("serving the plugin socket")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_toml_is_optional_strict_and_loopback_only() {
        let dir = tempfile::tempdir().unwrap();
        let config = PluginConfig::load(dir.path()).unwrap();
        assert_eq!(config.store_dir(dir.path()), dir.path());
        assert!(config.agent_listen.is_none());

        let write = |body: &str| std::fs::write(dir.path().join("plugin.toml"), body).unwrap();
        write("data_dir = \"db\"\nagent_listen = \"127.0.0.1:4530\"\n");
        let config = PluginConfig::load(dir.path()).unwrap();
        assert_eq!(config.store_dir(dir.path()), dir.path().join("db"));
        assert_eq!(config.agent_listen.unwrap().port(), 4530);
        write("data_dir = \"/abs/hora\"\n");
        let config = PluginConfig::load(dir.path()).unwrap();
        assert_eq!(config.store_dir(dir.path()), Path::new("/abs/hora"));

        write("agent_listen = \"0.0.0.0:4530\"\n");
        assert!(PluginConfig::load(dir.path()).is_err());
        write("agent_listen = \"[::1]:4530\"\n");
        assert!(PluginConfig::load(dir.path()).is_err());
        write("port = 4530\n");
        assert!(PluginConfig::load(dir.path()).is_err());
    }

    #[test]
    fn the_start_environment_is_checked() {
        let key = hex::encode([9u8; 32]);
        let env = |vars: &'static [(&'static str, &'static str)], key: String| {
            move |k: &str| {
                if k == "TRACON_NODE_KEY" {
                    return Some(key.clone());
                }
                vars.iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| v.to_string())
            }
        };
        assert!(auth(env(&[("TRACON_PLUGIN_TOKEN", "t")], key.clone())).is_ok());
        assert!(auth(env(&[], key.clone())).is_err());
        assert!(
            auth(env(
                &[("TRACON_PLUGIN_TOKEN", "t"), ("TRACON_CONTRACT", "2")],
                key.clone()
            ))
            .is_err()
        );
    }

    #[test]
    fn the_tools_are_the_agent_operations_with_object_schemas() {
        let tools = tools();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"time_log"));
        assert!(names.contains(&"time_propose"));
        for operator_only in ["time_confirm", "proposal_accept", "project_create"] {
            assert!(!names.contains(&operator_only), "{operator_only}");
        }
        for tool in &tools {
            assert_eq!(tool["inputSchema"]["type"], "object", "{tool}");
            assert!(tool["inputSchema"].get("$schema").is_none());
            assert!(!tool["description"].as_str().unwrap().is_empty());
        }
        let log = tools.iter().find(|t| t["name"] == "time_log").unwrap();
        assert_eq!(log["annotations"]["readOnlyHint"], false);
        assert!(log["inputSchema"]["properties"].get("project").is_some());
    }
}
