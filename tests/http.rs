use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use hora::http::{App, router};
use hora::model::{Actor, CreateProject};
use hora::store::Store;
use serde_json::{Value, json};
use tower::ServiceExt;

const ORIGIN: &str = "http://127.0.0.1:4530";

fn app() -> (tempfile::TempDir, axum::Router) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    store
        .create_project(
            &Actor::Operator,
            CreateProject {
                slug: "acme".into(),
                name: "Acme".into(),
                client: None,
                billable_default: None,
            },
        )
        .unwrap();
    let app = App {
        store: Arc::new(store),
        origin: ORIGIN.into(),
    };
    (dir, router(app))
}

fn post(path: &str) -> axum::http::request::Builder {
    Request::post(path)
        .header("host", "127.0.0.1:4530")
        .header("content-type", "application/json")
}

async fn send(router: &axum::Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn log_body() -> Value {
    json!({"project": "acme", "minutes": 30, "description": "bugfixes"})
}

#[tokio::test]
async fn agent_calls_create_drafts() {
    let (_dir, router) = app();
    let request = post("/api/agent/ops/time_log")
        .header("x-hora-agent", "claude-code")
        .body(Body::from(log_body().to_string()))
        .unwrap();
    let (status, body) = send(&router, request).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], "draft");
    assert_eq!(body["author"], "agent:claude-code");
}

#[tokio::test]
async fn agent_routes_refuse_browsers_and_anonymous_callers() {
    let (_dir, router) = app();
    let with_origin = post("/api/agent/ops/time_log")
        .header("x-hora-agent", "claude-code")
        .header("origin", ORIGIN)
        .body(Body::from(log_body().to_string()))
        .unwrap();
    assert_eq!(send(&router, with_origin).await.0, StatusCode::FORBIDDEN);
    let anonymous = post("/api/agent/ops/time_log")
        .body(Body::from(log_body().to_string()))
        .unwrap();
    assert_eq!(send(&router, anonymous).await.0, StatusCode::FORBIDDEN);
    let bad_name = post("/api/agent/ops/time_log")
        .header("x-hora-agent", "has spaces")
        .body(Body::from(log_body().to_string()))
        .unwrap();
    assert_eq!(send(&router, bad_name).await.0, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn agents_cannot_run_operator_operations() {
    let (_dir, router) = app();
    let body = json!({"entries": []});
    let request = post("/api/agent/ops/time_confirm")
        .header("x-hora-agent", "claude-code")
        .body(Body::from(body.to_string()))
        .unwrap();
    let (status, body) = send(&router, request).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

#[tokio::test]
async fn operator_routes_need_the_pages_origin() {
    let (_dir, router) = app();
    let missing = post("/api/ops/time_log")
        .body(Body::from(log_body().to_string()))
        .unwrap();
    assert_eq!(send(&router, missing).await.0, StatusCode::FORBIDDEN);
    let foreign = post("/api/ops/time_log")
        .header("origin", "http://evil.example")
        .body(Body::from(log_body().to_string()))
        .unwrap();
    assert_eq!(send(&router, foreign).await.0, StatusCode::FORBIDDEN);
    let own = post("/api/ops/time_log")
        .header("origin", ORIGIN)
        .body(Body::from(log_body().to_string()))
        .unwrap();
    let (status, body) = send(&router, own).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], "confirmed");
    assert_eq!(body["author"], "operator");
}

#[tokio::test]
async fn foreign_hosts_are_refused() {
    let (_dir, router) = app();
    let request = Request::get("/api/state")
        .header("host", "attacker.example:4530")
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&router, request).await.0, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn the_registry_publishes_input_schemas() {
    let (_dir, router) = app();
    let request = Request::get("/api/agent/ops")
        .header("host", "127.0.0.1:4530")
        .header("x-hora-agent", "claude-code")
        .body(Body::empty())
        .unwrap();
    let (status, body) = send(&router, request).await;
    assert_eq!(status, StatusCode::OK);
    let log = body
        .as_array()
        .unwrap()
        .iter()
        .find(|op| op["name"] == "time_log")
        .unwrap();
    assert_eq!(log["writes"], true);
    assert!(log["input_schema"]["properties"]["project"].is_object());
}

#[tokio::test]
async fn errors_carry_codes() {
    let (_dir, router) = app();
    let body = json!({"project": "acme", "description": "x"});
    let request = post("/api/agent/ops/time_log")
        .header("x-hora-agent", "claude-code")
        .body(Body::from(body.to_string()))
        .unwrap();
    let (status, body) = send(&router, request).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "invalid_request");
    let unknown = post("/api/agent/ops/nope")
        .header("x-hora-agent", "claude-code")
        .body(Body::from("{}"))
        .unwrap();
    assert_eq!(send(&router, unknown).await.0, StatusCode::NOT_FOUND);
}

async fn call(
    router: &axum::Router,
    agent: Option<&str>,
    op: &str,
    body: Value,
) -> (StatusCode, Value) {
    let request = match agent {
        Some(name) => post(&format!("/api/agent/ops/{op}")).header("x-hora-agent", name),
        None => post(&format!("/api/ops/{op}")).header("origin", ORIGIN),
    };
    send(router, request.body(Body::from(body.to_string())).unwrap()).await
}

#[tokio::test]
async fn proposals_go_through_the_operator() {
    let (_dir, router) = app();
    let (_, entry) = call(&router, None, "time_log", log_body()).await;
    let id = entry["id"].as_str().unwrap().to_string();

    let (status, _) = call(
        &router,
        Some("claude-code"),
        "time_edit",
        json!({"id": id, "version": 1, "minutes": 45}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "agents still can't edit confirmed entries"
    );

    let propose =
        |minutes: i64| json!({"id": id, "version": 1, "minutes": minutes, "reason": "log says so"});
    let (status, proposal) = call(&router, Some("claude-code"), "time_propose", propose(45)).await;
    assert_eq!(status, StatusCode::OK, "{proposal}");
    assert_eq!(proposal["changes"], json!({"minutes": [30, 45]}));
    let first = proposal["id"].as_str().unwrap().to_string();

    let (status, body) = call(&router, Some("claude-code"), "time_propose", propose(40)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "pending_proposal");
    assert_eq!(body["id"], first.as_str());

    let (_, second) = call(&router, Some("codex"), "time_propose", propose(50)).await;
    let second = second["id"].as_str().unwrap().to_string();

    let (status, _) = call(
        &router,
        Some("claude-code"),
        "proposal_accept",
        json!({"id": first}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = call(
        &router,
        Some("claude-code"),
        "proposal_reject",
        json!({"id": first}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, accepted) = call(&router, None, "proposal_accept", json!({"id": first})).await;
    assert_eq!(status, StatusCode::OK, "{accepted}");
    assert_eq!(accepted["entry"]["minutes"], 45);
    assert_eq!(accepted["entry"]["state"], "confirmed");

    let (_, history) = call(
        &router,
        Some("claude-code"),
        "time_history",
        json!({"id": id}),
    )
    .await;
    let last = history.as_array().unwrap().last().unwrap();
    assert_eq!(last["actor"], "operator");
    assert_eq!(last["proposed_by"], "agent:claude-code");

    let (status, body) = call(&router, None, "proposal_accept", json!({"id": second})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "stale_proposal");
    let (status, _) = call(
        &router,
        None,
        "proposal_reject",
        json!({"id": second, "note": "stale"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (_, state) = send(
        &router,
        Request::get("/api/state")
            .header("host", "127.0.0.1:4530")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(state["proposals"], 0);
}
