//! The release archive, built here as `release.yml` builds it:
//! `tracon-plugin.toml`, `bin/hora` and `ui/` (from `spa/dist`). The manifest
//! is what the node reads, the binary starts in plugin mode from `exec`, and
//! the page works behind a stand-in for the node's proxy, which serves `ui/`
//! under `/t/<token>/` and forwards `api/...` with a signed context.

mod support;

use std::path::{Path, PathBuf};

use axum::{
    Router,
    body::Bytes,
    extract::{Path as UrlPath, State},
    http::{HeaderMap, Method, StatusCode, Uri, header},
    response::{IntoResponse, Response},
    routing::any,
};
use serde_json::{Value, json};
use support::{Plugin, signed_header};

const ROOT: &str = env!("CARGO_MANIFEST_DIR");

fn manifest() -> toml::Table {
    let text = std::fs::read_to_string(Path::new(ROOT).join("tracon-plugin.toml")).unwrap();
    toml::from_str(&text).unwrap()
}

#[test]
fn the_manifest_is_contract_1_with_a_ui_and_the_tools_hora_lists() {
    let m = manifest();
    let mut keys: Vec<&str> = m.keys().map(String::as_str).collect();
    keys.sort();
    // A key written after a table would land inside it, and the node refuses
    // unknown keys, so the set is exact: no credentials, no egress.
    assert_eq!(keys, ["contract", "exec", "name", "tool", "ui", "version"]);
    assert_eq!(m["name"].as_str(), Some(hora::contract::PLUGIN));
    assert_eq!(m["version"].as_str(), Some(env!("CARGO_PKG_VERSION")));
    assert_eq!(m["contract"].as_integer(), Some(1));
    assert_eq!(
        m["exec"].as_array().unwrap(),
        &[toml::Value::from("bin/hora"), toml::Value::from("serve")]
    );
    let ui = m["ui"].as_table().unwrap();
    let mut ui_keys: Vec<&str> = ui.keys().map(String::as_str).collect();
    ui_keys.sort();
    assert_eq!(ui_keys, ["entry", "title"]);
    assert_eq!(ui["entry"].as_str(), Some("ui/index.html"));
    assert_eq!(ui["title"].as_str(), Some("hora"));

    // [[tool]] names exactly what tools/list returns, a write hint for each
    // operation that writes, and nothing consequential.
    let declared: Vec<(String, String)> = m["tool"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            let t = t.as_table().unwrap();
            let mut keys: Vec<&str> = t.keys().map(String::as_str).collect();
            keys.sort();
            assert_eq!(keys, ["hint", "name"]);
            (
                t["name"].as_str().unwrap().to_string(),
                t["hint"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let listed: Vec<(String, String)> = hora::plugin::tool_ops()
        .map(|op| {
            let hint = if op.writes { "write" } else { "read" };
            (op.name.to_string(), hint.to_string())
        })
        .collect();
    assert_eq!(declared, listed);
    let from_list: Vec<String> = hora::plugin::tools()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        declared.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>(),
        from_list
    );
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// Every `src="..."` and `href="..."` in `html`.
fn references(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    for attr in ["src=\"", "href=\""] {
        let mut rest = html;
        while let Some(at) = rest.find(attr) {
            rest = &rest[at + attr.len()..];
            let end = rest.find('"').unwrap();
            out.push(rest[..end].to_string());
            rest = &rest[end..];
        }
    }
    out
}

const NODE_TOKEN: &str = "cap-token";

/// The stand-in node: what Tracon's UI listener does for one plugin.
#[derive(Clone)]
struct Node {
    ui: PathBuf,
    socket: PathBuf,
}

async fn node(
    State(node): State<Node>,
    UrlPath((token, rest)): UrlPath<(String, String)>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if token != NODE_TOKEN || rest.split('/').any(|part| part == "..") {
        return StatusCode::NOT_FOUND.into_response();
    }
    if rest == "api" || rest.starts_with("api/") {
        let target = match uri.query() {
            Some(query) => format!("/{rest}?{query}"),
            None => format!("/{rest}"),
        };
        // No Origin, Cookie or Referer goes on; a signed context does.
        let mut forward = vec![(
            "tracon-context",
            signed_header(method.as_str(), &target, &body),
        )];
        for name in ["content-type", "accept"] {
            if let Some(value) = headers.get(name) {
                forward.push((name, value.to_str().unwrap().to_string()));
            }
        }
        let answer = support::http(&node.socket, method.as_str(), &target, &forward, &body)
            .await
            .unwrap();
        let mut response = Response::builder().status(answer.status);
        if let Some(v) = answer.content_type {
            response = response.header(header::CONTENT_TYPE, v);
        }
        if let Some(v) = answer.content_disposition {
            response = response.header(header::CONTENT_DISPOSITION, v);
        }
        return response.body(answer.body.into()).unwrap();
    }
    let file = node.ui.join(&rest);
    match std::fs::read(&file) {
        Ok(bytes) if file.is_file() => {
            let mime = mime_guess::from_path(&file).first_or_octet_stream();
            ([(header::CONTENT_TYPE, mime.to_string())], bytes).into_response()
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

#[tokio::test]
async fn the_packaged_archive_runs_behind_a_stand_in_node_proxy() {
    // The archive's layout.
    let stage = tempfile::tempdir().unwrap();
    let root = stage.path();
    std::fs::copy(
        Path::new(ROOT).join("tracon-plugin.toml"),
        root.join("tracon-plugin.toml"),
    )
    .unwrap();
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::copy(support::BIN, root.join("bin/hora")).unwrap();
    copy_dir(&Path::new(ROOT).join("spa/dist"), &root.join("ui"));

    let m = manifest();
    let entry = root.join(m["ui"]["entry"].as_str().unwrap());
    let html = std::fs::read_to_string(&entry).unwrap();
    assert!(
        !html.contains("<script>"),
        "no inline scripts under the CSP"
    );
    let refs = references(&html);
    assert!(refs.iter().any(|r| r.contains("assets/")), "{refs:?}");
    for r in &refs {
        assert!(r.starts_with("./"), "{r} is not relative");
        assert!(
            entry.parent().unwrap().join(r).is_file(),
            "{r} is not in ui/"
        );
    }
    for js in std::fs::read_dir(root.join("ui/assets")).unwrap() {
        let path = js.unwrap().path();
        if path.extension().is_some_and(|e| e == "js") {
            let code = std::fs::read_to_string(&path).unwrap();
            for absolute in ["\"/api", "'/api", "`/api"] {
                assert!(!code.contains(absolute), "{absolute} in {}", path.display());
            }
        }
    }

    // Started from `exec`, as the node starts it.
    let exec = root.join(m["exec"][0].as_str().unwrap());
    let plugin = Plugin::start_exec(&exec, |_| {}).await;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = Router::new()
        .route("/t/{token}/{*rest}", any(node))
        .with_state(Node {
            ui: root.join("ui"),
            socket: plugin.socket.clone(),
        });
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    // The page, and every file it names, relative to where the node serves it.
    let page = format!("http://127.0.0.1:{port}/t/{NODE_TOKEN}/index.html");
    let base = reqwest::Url::parse(&page).unwrap();
    let client = reqwest::Client::new();
    let index = client.get(page.as_str()).send().await.unwrap();
    assert_eq!(index.status(), 200);
    for r in references(&index.text().await.unwrap()) {
        let url = base.join(&r).unwrap();
        assert!(
            url.path().starts_with(&format!("/t/{NODE_TOKEN}/")),
            "{url}"
        );
        let status = client.get(url.clone()).send().await.unwrap().status();
        assert_eq!(status, 200, "{url}");
    }

    // The API the page calls, as relative URLs resolve against it. The
    // browser's Origin is the node's, which hora never sees.
    let api = |path: &str| base.join(path).unwrap();
    let origin = format!("http://127.0.0.1:{port}");
    let post = |path: &str, body: Value| {
        client
            .post(api(path))
            .header("content-type", "application/json")
            .header("origin", &origin)
            .body(body.to_string())
            .send()
    };
    let created = post(
        "api/ops/project_create",
        json!({"slug": "acme", "name": "Acme"}),
    )
    .await
    .unwrap();
    assert_eq!(created.status(), 200);
    let logged = post(
        "api/ops/time_log",
        json!({"project": "acme", "date": "2026-11-02", "minutes": 90,
               "description": "Release, with a comma"}),
    )
    .await
    .unwrap();
    assert_eq!(logged.status(), 200);
    let entry: Value = logged.json().await.unwrap();
    assert_eq!(entry["author"], "operator");

    let state: Value = client
        .get(api("api/state"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(state["projects"][0]["slug"], "acme");

    // Exports download through the proxy with their filename.
    let export = client
        .get(api(
            "api/export?format=csv&from=2026-11-01&to=2026-11-30&project=acme",
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(export.status(), 200);
    let disposition = export.headers()[header::CONTENT_DISPOSITION]
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        disposition.contains("hora-2026-11-02-2026-11-02.csv"),
        "{disposition}"
    );
    let csv = export.text().await.unwrap();
    assert!(csv.contains("\"Release, with a comma\""), "{csv}");

    // No SPA fallback, and nothing outside the token.
    let missing = client.get(api("week")).send().await.unwrap();
    assert_eq!(missing.status(), 404);
}
