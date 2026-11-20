//! Starting `hora serve --socket --data` as a Tracon node would, and speaking
//! to it over its socket with contexts signed by a test node key.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine;
use ed25519_dalek::SigningKey;
use hora::contract::{self, UiRequest};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::process::{Child, Command};

pub const BIN: &str = env!("CARGO_BIN_EXE_hora");
pub const TOKEN: &str = "hora-plugin-test-token";

pub fn key() -> SigningKey {
    SigningKey::from_bytes(&std::array::from_fn(|i| i as u8 + 1))
}

pub fn node_key() -> String {
    hex::encode(key().verifying_key().to_bytes())
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// 32 lowercase hex characters, never the same twice in a run.
pub fn nonce() -> String {
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;
    format!("{nanos:016x}{:016x}", COUNT.fetch_add(1, Ordering::Relaxed))
}

/// `exec` with the arguments and environment a node starts a plugin with.
pub fn command(exec: &Path, socket: &Path, data: &Path) -> Command {
    let mut cmd = Command::new(exec);
    cmd.env_clear()
        .env("TRACON_PLUGIN_TOKEN", TOKEN)
        .env("TRACON_NODE_KEY", node_key())
        .env("TRACON_CONTRACT", "1")
        .arg("serve")
        .arg("--socket")
        .arg(socket)
        .arg("--data")
        .arg(data)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    cmd
}

/// The unsigned context the node would send for a UI request.
pub fn ui_context(method: &str, target: &str, body: &[u8]) -> Value {
    let now = now_ms();
    json!({
        "v": 1,
        "plugin": "hora",
        "instance": contract::instance_of(TOKEN),
        "tool": "tracon/ui",
        "arguments_sha256": contract::ui_arguments_sha256(&UiRequest { method, target, body }),
        "caller": {"kind": "operator", "session": null, "harness": null},
        "channel": "personal",
        "approval_id": null,
        "issued_ms": now,
        "expires_ms": now + 30_000,
        "nonce": nonce(),
        "credentials": {},
    })
}

/// The unsigned context the node would send for a tool call by `harness`.
pub fn call_context(tool: &str, arguments: &Value, harness: &str) -> Value {
    let now = now_ms();
    json!({
        "v": 1,
        "plugin": "hora",
        "instance": contract::instance_of(TOKEN),
        "tool": tool,
        "arguments_sha256": contract::arguments_sha256(Some(arguments)),
        "caller": {"kind": "agent", "session": "session-1", "harness": harness},
        "channel": "personal",
        "approval_id": null,
        "issued_ms": now,
        "expires_ms": now + 30_000,
        "nonce": nonce(),
        "credentials": {},
    })
}

/// A context as the `tracon-context` header carries it.
pub fn header(context: &Value) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(context.to_string())
}

pub fn signed_header(method: &str, target: &str, body: &[u8]) -> String {
    header(&contract::sign(&ui_context(method, target, body), &key()))
}

/// A response: status, the headers the node passes on, and the body.
pub struct Answer {
    pub status: u16,
    pub content_type: Option<String>,
    pub content_disposition: Option<String>,
    pub body: Vec<u8>,
}

impl Answer {
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

fn dechunk(mut data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let Some(line_end) = data.windows(2).position(|w| w == b"\r\n") else {
            return out;
        };
        let size =
            usize::from_str_radix(std::str::from_utf8(&data[..line_end]).unwrap().trim(), 16)
                .unwrap();
        data = &data[line_end + 2..];
        if size == 0 {
            return out;
        }
        out.extend_from_slice(&data[..size]);
        data = &data[size + 2..];
    }
}

/// One HTTP/1.1 exchange on the socket, as the node's proxy makes it.
pub async fn http(
    socket: &Path,
    method: &str,
    target: &str,
    headers: &[(&str, String)],
    body: &[u8],
) -> std::io::Result<Answer> {
    let mut stream = UnixStream::connect(socket).await?;
    let mut request = format!(
        "{method} {target} HTTP/1.1\r\nHost: plugin\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).await?;
    stream.write_all(body).await?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await?;
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = String::from_utf8_lossy(&raw[..split]).to_string();
    let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    let field = |name: &str| {
        head.lines().skip(1).find_map(|line| {
            let (k, v) = line.split_once(':')?;
            k.trim()
                .eq_ignore_ascii_case(name)
                .then(|| v.trim().to_string())
        })
    };
    let mut data = raw[split + 4..].to_vec();
    if field("transfer-encoding").is_some_and(|v| v.eq_ignore_ascii_case("chunked")) {
        data = dechunk(&data);
    }
    Ok(Answer {
        status,
        content_type: field("content-type"),
        content_disposition: field("content-disposition"),
        body: data,
    })
}

/// A running plugin, killed when dropped.
pub struct Plugin {
    pub child: Child,
    pub socket: PathBuf,
    pub data: PathBuf,
    pub dir: tempfile::TempDir,
}

impl Plugin {
    /// Start the built binary with an empty data directory that `setup` may
    /// fill first.
    pub async fn start(setup: impl FnOnce(&Path)) -> Self {
        Self::start_exec(Path::new(BIN), setup).await
    }

    pub async fn start_exec(exec: &Path, setup: impl FnOnce(&Path)) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        setup(&data);
        let socket = dir.path().join("hora.sock");
        let child = command(exec, &socket, &data).spawn().unwrap();
        let plugin = Self {
            child,
            socket,
            data,
            dir,
        };
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let init = json!({"jsonrpc": "2.0", "id": 0, "method": "initialize",
                "params": {"protocolVersion": "2025-06-18"}});
            if let Ok(answer) = plugin.try_mcp(&init).await
                && answer.status == 200
            {
                return plugin;
            }
            assert!(
                Instant::now() < deadline,
                "hora never answered initialize on its socket"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn try_mcp(&self, msg: &Value) -> std::io::Result<Answer> {
        http(
            &self.socket,
            "POST",
            "/",
            &[("Content-Type", "application/json".into())],
            msg.to_string().as_bytes(),
        )
        .await
    }

    /// One JSON-RPC message; its HTTP status and answer.
    pub async fn mcp(&self, msg: &Value) -> (u16, Value) {
        let answer = self.try_mcp(msg).await.unwrap();
        (answer.status, answer.json())
    }

    /// `tools/call` with the node's signed context for `harness`; the result
    /// or the JSON-RPC error.
    pub async fn call(&self, tool: &str, arguments: Value, harness: &str) -> Value {
        let context = contract::sign(&call_context(tool, &arguments, harness), &key());
        let (status, answer) = self
            .mcp(
                &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
                    "name": tool, "arguments": arguments, "_meta": {"tracon": context},
                }}),
            )
            .await;
        assert_eq!(status, 200, "{answer}");
        answer
    }

    /// The JSON a successful tool call answered.
    pub async fn call_ok(&self, tool: &str, arguments: Value, harness: &str) -> Value {
        let answer = self.call(tool, arguments, harness).await;
        let result = &answer["result"];
        assert_eq!(result["isError"], false, "{answer}");
        serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap()
    }

    /// A request on the socket with no context at all.
    pub async fn raw(
        &self,
        method: &str,
        target: &str,
        headers: &[(&str, String)],
        body: &[u8],
    ) -> Answer {
        http(&self.socket, method, target, headers, body)
            .await
            .unwrap()
    }

    /// A request from the operator's page, as the node forwards it.
    pub async fn operator(&self, method: &str, target: &str, body: &Value) -> (u16, Value) {
        let body = if body.is_null() {
            Vec::new()
        } else {
            body.to_string().into_bytes()
        };
        let answer = self
            .raw(
                method,
                target,
                &[
                    ("Content-Type", "application/json".into()),
                    ("tracon-context", signed_header(method, target, &body)),
                ],
                &body,
            )
            .await;
        (answer.status, answer.json())
    }

    pub async fn op(&self, name: &str, args: Value) -> (u16, Value) {
        self.operator("POST", &format!("/api/ops/{name}"), &args)
            .await
    }

    pub async fn add_project(&self, slug: &str) {
        let (status, body) = self
            .op(
                "project_create",
                json!({"slug": slug, "name": slug.to_uppercase()}),
            )
            .await;
        assert_eq!(status, 200, "{body}");
    }
}
