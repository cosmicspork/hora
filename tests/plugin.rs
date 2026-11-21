//! `hora serve --socket --data`, run as a Tracon node runs it: the built
//! binary, an empty environment but for the contract's variables, requests on
//! the Unix socket carrying a context signed with a test node key, and tool
//! calls carrying one in `params._meta.tracon`.

mod support;

use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

use hora::contract;
use serde_json::{Value, json};
use support::{Plugin, header, key, signed_header, ui_context};

fn minutes(n: i64) -> Value {
    json!({"project": "acme", "date": "2026-11-02", "minutes": n, "description": "Client work",
           "refs": ["pr:acme-web#1"]})
}

#[tokio::test]
async fn a_signed_operator_request_is_served_as_the_operator() {
    let plugin = Plugin::start(|_| {}).await;
    plugin.add_project("acme").await;
    let (status, entry) = plugin.op("time_log", minutes(30)).await;
    assert_eq!(status, 200, "{entry}");
    assert_eq!(entry["state"], "confirmed");
    assert_eq!(entry["author"], "operator");

    let (status, state) = plugin.operator("GET", "/api/state", &Value::Null).await;
    assert_eq!(status, 200, "{state}");
    assert_eq!(state["projects"][0]["slug"], "acme");

    // The database is in the data directory by default, private.
    let db = plugin.data.join("hora.sqlite3");
    assert_eq!(
        std::fs::metadata(&db).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let socket = std::fs::metadata(&plugin.socket).unwrap();
    assert_eq!(socket.permissions().mode() & 0o777, 0o600);
}

#[tokio::test]
async fn refused_contexts_are_401_and_change_nothing() {
    let plugin = Plugin::start(|_| {}).await;
    plugin.add_project("acme").await;
    let target = "/api/ops/time_log";
    let body = minutes(30).to_string().into_bytes();
    let json = ("Content-Type", "application/json".to_string());

    // No context.
    let answer = plugin
        .raw("POST", target, std::slice::from_ref(&json), &body)
        .await;
    assert_eq!(answer.status, 401);
    assert_eq!(answer.json()["error"], "unauthorized");

    // Unreadable.
    let garbage = ("tracon-context", "!!".to_string());
    let answer = plugin
        .raw("POST", target, &[json.clone(), garbage], &body)
        .await;
    assert_eq!(answer.status, 401);

    // Signed by another key.
    let other = ed25519_dalek::SigningKey::from_bytes(&[42; 32]);
    let forged = contract::sign(&ui_context("POST", target, &body), &other);
    let answer = plugin
        .raw(
            "POST",
            target,
            &[json.clone(), ("tracon-context", header(&forged))],
            &body,
        )
        .await;
    assert_eq!(answer.status, 401);

    // For another request: the body, the path and the method are all bound.
    let for_other_body = signed_header("POST", target, b"{}");
    let answer = plugin
        .raw(
            "POST",
            target,
            &[json.clone(), ("tracon-context", for_other_body)],
            &body,
        )
        .await;
    assert_eq!(answer.status, 401);
    let for_confirm = signed_header("POST", "/api/ops/time_confirm", &body);
    let answer = plugin
        .raw(
            "POST",
            target,
            &[json.clone(), ("tracon-context", for_confirm)],
            &body,
        )
        .await;
    assert_eq!(answer.status, 401);

    // An agent's context on the operator's API.
    let mut agent = ui_context("POST", target, &body);
    agent["caller"] = json!({"kind": "agent", "session": "s", "harness": "claude"});
    let agent = contract::sign(&agent, &key());
    let answer = plugin
        .raw(
            "POST",
            target,
            &[json.clone(), ("tracon-context", header(&agent))],
            &body,
        )
        .await;
    assert_eq!(answer.status, 401);

    // Expired.
    let mut old = ui_context("POST", target, &body);
    old["issued_ms"] = json!(support::now_ms() - 120_000);
    old["expires_ms"] = json!(support::now_ms() - 90_000);
    let old = contract::sign(&old, &key());
    let answer = plugin
        .raw(
            "POST",
            target,
            &[json.clone(), ("tracon-context", header(&old))],
            &body,
        )
        .await;
    assert_eq!(answer.status, 401);

    // A replay of a context that was accepted once.
    let good = signed_header("POST", target, &body);
    let headers = [json.clone(), ("tracon-context", good)];
    assert_eq!(
        plugin.raw("POST", target, &headers, &body).await.status,
        200
    );
    assert_eq!(
        plugin.raw("POST", target, &headers, &body).await.status,
        401
    );

    let (_, entries) = plugin
        .op(
            "time_list",
            json!({"from": "2026-11-01", "to": "2026-11-30"}),
        )
        .await;
    assert_eq!(entries.as_array().unwrap().len(), 1, "{entries}");
}

#[tokio::test]
async fn the_socket_serves_no_agent_api_and_no_page() {
    let plugin = Plugin::start(|_| {}).await;
    let body = minutes(30).to_string().into_bytes();
    let target = "/api/agent/ops/time_log";
    let answer = plugin
        .raw(
            "POST",
            target,
            &[
                ("Content-Type", "application/json".into()),
                ("X-Hora-Agent", "claude".into()),
                ("tracon-context", signed_header("POST", target, &body)),
            ],
            &body,
        )
        .await;
    assert_eq!(answer.status, 403);
    for path in ["/index.html", "/assets/x.js", "/week"] {
        assert_eq!(
            plugin.raw("GET", path, &[], b"").await.status,
            404,
            "{path}"
        );
    }
}

#[tokio::test]
async fn mcp_offers_the_agent_operations_as_tools() {
    let plugin = Plugin::start(|_| {}).await;
    let (status, init) = plugin
        .mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2025-03-26"}}))
        .await;
    assert_eq!(status, 200);
    assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(init["result"]["serverInfo"]["name"], "hora");
    assert_eq!(init["result"]["capabilities"], json!({"tools": {}}));

    let (status, _) = plugin
        .mcp(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
        .await;
    assert_eq!(status, 202);
    let (_, ping) = plugin
        .mcp(&json!({"jsonrpc": "2.0", "id": 2, "method": "ping"}))
        .await;
    assert_eq!(ping["result"], json!({}));

    let (_, list) = plugin
        .mcp(&json!({"jsonrpc": "2.0", "id": 3, "method": "tools/list"}))
        .await;
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    let expected: Vec<&str> = hora::plugin::tool_ops().map(|op| op.name).collect();
    assert_eq!(names, expected);
    assert!(!names.contains(&"time_confirm"));
}

#[tokio::test]
async fn an_agent_tool_call_is_a_draft_by_the_harness_the_node_names() {
    let plugin = Plugin::start(|_| {}).await;
    plugin.add_project("acme").await;

    let draft = plugin.call_ok("time_log", minutes(45), "claude").await;
    assert_eq!(draft["state"], "draft");
    assert_eq!(draft["author"], "agent:claude");
    let history = plugin
        .call_ok("time_history", json!({"id": draft["id"]}), "claude")
        .await;
    assert_eq!(history[0]["actor"], "agent:claude");
    assert_eq!(history[0]["via"], "agent");

    // The operator confirms it on the page; the agent can then only propose.
    let (status, confirmed) = plugin
        .op(
            "time_confirm",
            json!({"entries": [{"id": draft["id"], "version": draft["version"]}]}),
        )
        .await;
    assert_eq!(status, 200, "{confirmed}");
    let version = confirmed[0]["version"].clone();
    let edit = plugin
        .call(
            "time_edit",
            json!({"id": draft["id"], "version": version, "minutes": 50}),
            "claude",
        )
        .await;
    assert_eq!(edit["result"]["isError"], true, "{edit}");
    let error: Value =
        serde_json::from_str(edit["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(error["error"], "forbidden");
    let proposal = plugin
        .call_ok(
            "time_propose",
            json!({"id": draft["id"], "version": version, "minutes": 50,
                   "reason": "Commits end at 18:30"}),
            "claude",
        )
        .await;
    assert_eq!(proposal["state"], "pending");
    assert_eq!(proposal["author"], "agent:claude");

    // A harness id that is not a usable name is recorded as a plain agent.
    let unnamed = plugin.call_ok("time_log", minutes(5), "not a name").await;
    assert_eq!(
        unnamed["author"],
        format!("agent:{}", contract::UNNAMED_AGENT)
    );
}

#[tokio::test]
async fn tool_calls_without_a_valid_context_are_refused() {
    let plugin = Plugin::start(|_| {}).await;
    plugin.add_project("acme").await;
    let refused = |answer: &Value| answer["error"]["code"] == plugin_refused();

    // No context.
    let (_, answer) = plugin
        .mcp(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "time_log", "arguments": minutes(5)}}))
        .await;
    assert!(refused(&answer), "{answer}");

    // Signed for other arguments than the ones sent.
    let context = contract::sign(
        &support::call_context("time_log", &minutes(5), "claude"),
        &key(),
    );
    let (_, answer) = plugin
        .mcp(
            &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
            "name": "time_log", "arguments": minutes(500), "_meta": {"tracon": context}}}),
        )
        .await;
    assert!(refused(&answer), "{answer}");
    assert_eq!(answer["error"]["data"]["check"], 3);

    // An operator-only operation is not a tool, whoever signs for it.
    let args = json!({"entries": []});
    let context = contract::sign(
        &support::call_context("time_confirm", &args, "claude"),
        &key(),
    );
    let (_, answer) = plugin
        .mcp(
            &json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
            "name": "time_confirm", "arguments": args, "_meta": {"tracon": context}}}),
        )
        .await;
    assert_eq!(answer["error"]["code"], -32602, "{answer}");

    let (_, entries) = plugin
        .op(
            "time_list",
            json!({"from": "2026-11-01", "to": "2026-11-30"}),
        )
        .await;
    assert_eq!(entries, json!([]));
}

fn plugin_refused() -> i64 {
    hora::plugin::REFUSED
}

#[tokio::test]
async fn plugin_toml_moves_the_database_and_agent_listen_serves_the_cli() {
    let port = {
        let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        probe.local_addr().unwrap().port()
    };
    let elsewhere = tempfile::tempdir().unwrap();
    let store_dir = elsewhere.path().join("hora");
    let config = format!(
        "data_dir = {:?}\nagent_listen = \"127.0.0.1:{port}\"\n",
        store_dir.display().to_string()
    );
    let plugin = Plugin::start(|data| {
        std::fs::write(data.join("plugin.toml"), &config).unwrap();
    })
    .await;
    plugin.add_project("acme").await;
    assert!(store_dir.join("hora.sqlite3").is_file());
    assert!(!plugin.data.join("hora.sqlite3").exists());

    // The CLI, pointed at agent_listen, logs a draft under its own name.
    let out = tokio::process::Command::new(support::BIN)
        .env_clear()
        .env("HORA_URL", format!("http://127.0.0.1:{port}"))
        .env("HORA_AGENT", "claude-code")
        .args([
            "log",
            "-p",
            "acme",
            "--date",
            "2026-11-03",
            "--minutes",
            "20",
            "-m",
            "From the CLI",
        ])
        .output()
        .await
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let entry: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(entry["author"], "agent:claude-code");
    assert_eq!(entry["state"], "draft");

    // agent_listen serves the agent API only: not the operator's, not the page.
    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");
    for path in ["/api/state", "/", "/index.html"] {
        let status = client
            .get(format!("{base}{path}"))
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(status, 404, "{path}");
    }
    // Even a request that passes the standalone origin check finds no
    // operator route there.
    let confirm = client
        .post(format!("{base}/api/ops/time_confirm"))
        .header("content-type", "application/json")
        .header("origin", &base)
        .body(json!({"entries": []}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(confirm.status(), 404);
}

#[tokio::test]
async fn sigterm_removes_the_socket() {
    let mut plugin = Plugin::start(|_| {}).await;
    let pid = plugin.child.id().unwrap();
    let killed = std::process::Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    let status = tokio::time::timeout(Duration::from_secs(10), plugin.child.wait())
        .await
        .expect("hora exits on SIGTERM")
        .unwrap();
    assert!(status.success(), "{status:?}");
    assert!(!plugin.socket.exists());
}

#[tokio::test]
async fn plugin_mode_needs_the_contract_environment_and_refuses_a_non_socket() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("hora.sock");
    let data = dir.path().join("data");

    let out = tokio::process::Command::new(support::BIN)
        .env_clear()
        .args(["serve", "--socket"])
        .arg(&socket)
        .arg("--data")
        .arg(&data)
        .output()
        .await
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("TRACON_PLUGIN_TOKEN"));

    std::fs::write(&socket, "not a socket").unwrap();
    let started = Instant::now();
    let out = support::command(std::path::Path::new(support::BIN), &socket, &data)
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("not a socket"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(std::fs::read_to_string(&socket).unwrap(), "not a socket");
}

#[tokio::test]
async fn the_overview_and_export_tools_answer_as_their_routes_do() {
    let plugin = Plugin::start(|_| {}).await;
    plugin.add_project("acme").await;
    plugin.call_ok("time_log", minutes(45), "claude").await;

    let overview = plugin.call_ok("time_overview", json!({}), "claude").await;
    let (status, state) = plugin.operator("GET", "/api/state", &Value::Null).await;
    assert_eq!(status, 200, "{state}");
    assert_eq!(overview, state);
    assert_eq!(overview["drafts"], 1);
    assert_eq!(overview["projects"][0]["slug"], "acme");
    assert!(overview["today"].is_string() && overview["utc_offset"].is_string());

    let export = plugin
        .call_ok(
            "time_export",
            json!({"format": "jsonl", "from": "2026-11-01", "to": "2026-11-30"}),
            "claude",
        )
        .await;
    assert_eq!(export["filename"], "hora-2026-11-02-2026-11-02.jsonl");
    assert_eq!(export["entries"], 1);
    let line: Value = serde_json::from_str(export["content"].as_str().unwrap().trim_end()).unwrap();
    assert_eq!(line["author"], "agent:claude");
    assert!(line.get("warnings").is_none());
}
