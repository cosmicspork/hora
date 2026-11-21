//! The agent's tools and hora's HTTP API offer the same operations. Both are
//! built from `ops::OPS` (tools by `plugin::tools`, routes by
//! `http::ROUTES`), and these tests fail on anything one offers that the
//! other doesn't, unless it is listed below with the reason.

use std::collections::BTreeSet;
use std::path::Path;

use hora::http::{ROUTES, Serves};
use hora::ops::{self, OPS};
use hora::plugin;

const ROOT: &str = env!("CARGO_MANIFEST_DIR");

/// Operations that are deliberately not MCP tools. Each is a decision the
/// operator makes about agents' work, made in hora's page: an agent's entry
/// is a draft until the operator confirms it, and its change to a confirmed
/// entry is a proposal until the operator accepts it. A tool would let the
/// agent make that decision itself, since the node attests the tool's caller
/// as the agent and hora refuses these to agents anyway.
const NOT_TOOLS: &[(&str, &str)] = &[
    (
        "time_confirm",
        "Confirming drafts is the operator's review of an agent's time.",
    ),
    (
        "proposal_accept",
        "Accepting a proposal is the operator's decision on an agent's change to a confirmed entry.",
    ),
    (
        "proposal_reject",
        "Rejecting a proposal is the operator's decision, the other half of accepting one.",
    ),
    (
        "project_create",
        "Projects are what time is billed against; the operator decides which exist.",
    ),
    (
        "project_update",
        "Renaming, archiving or changing a project's billable default changes how time is billed.",
    ),
];

/// Routes with no single operation behind them, each with what serves the
/// same purpose for a tool caller.
const ROUTES_WITHOUT_TOOL: &[(&str, &str)] = &[
    (
        "/api/ops",
        "The registry; an MCP client reads the tools from tools/list.",
    ),
    (
        "/api/agent/ops",
        "The registry; an MCP client reads the tools from tools/list.",
    ),
];

/// Routes in the source that are not the HTTP API.
const NOT_API: &[(&str, &str)] = &[
    (
        "/",
        "The page standalone, and the MCP endpoint on the plugin socket.",
    ),
    ("/assets/{*path}", "The page's assets."),
];

fn tool_names() -> Vec<String> {
    plugin::tools()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn every_operation_is_a_tool_or_an_explained_exclusion() {
    for (name, why) in NOT_TOOLS {
        assert!(!why.is_empty(), "{name} needs a reason");
    }
    let excluded: BTreeSet<&str> = NOT_TOOLS.iter().map(|(name, _)| *name).collect();
    let operator_only: BTreeSet<&str> = OPS
        .iter()
        .filter(|op| op.operator_only)
        .map(|op| op.name)
        .collect();
    // Marking an operation operator-only takes it off the tools, so it needs
    // a reason here; a reason left for an operation that is a tool, or gone,
    // is stale.
    assert_eq!(
        operator_only, excluded,
        "NOT_TOOLS must name exactly the operator-only operations"
    );
    let expected: Vec<String> = OPS
        .iter()
        .filter(|op| !excluded.contains(op.name))
        .map(|op| op.name.to_string())
        .collect();
    assert_eq!(tool_names(), expected);
}

#[test]
fn every_route_is_an_operation_or_an_explained_exclusion() {
    let tools = tool_names();
    let mut paths = BTreeSet::new();
    for route in ROUTES {
        assert!(paths.insert(route.path), "{} is listed twice", route.path);
        match route.serves {
            // Every operation, and so every tool, by name.
            Serves::AnyOp => assert!(route.path.ends_with("/ops/{name}"), "{}", route.path),
            Serves::Registry => assert!(
                ROUTES_WITHOUT_TOOL.iter().any(|(p, _)| *p == route.path),
                "{} serves no operation and has no reason in ROUTES_WITHOUT_TOOL",
                route.path
            ),
            Serves::Op(name) | Serves::Download(name) => {
                let op = ops::find(name)
                    .unwrap_or_else(|| panic!("{} serves no operation {name}", route.path));
                // A GET route skips the Origin check a mutation gets.
                assert!(!op.writes, "{} serves {name}, which writes", route.path);
                assert!(
                    op.operator_only || tools.iter().any(|t| t == name),
                    "{} serves {name}, which is not a tool",
                    route.path
                );
            }
        }
    }
    for (path, why) in ROUTES_WITHOUT_TOOL {
        assert!(!why.is_empty(), "{path} needs a reason");
        assert!(
            ROUTES
                .iter()
                .any(|r| r.path == *path && r.serves == Serves::Registry),
            "ROUTES_WITHOUT_TOOL names {path}, which is not a registry route"
        );
    }
    // The agent API and the operator's offer the same operations.
    for prefix in ["/api", "/api/agent"] {
        for tail in ["/ops", "/ops/{name}"] {
            let path = format!("{prefix}{tail}");
            assert!(paths.contains(path.as_str()), "{path} is missing");
        }
    }
}

/// Every string literal that follows `marker` in `text`.
fn literals_after<'a>(text: &'a str, marker: &str, quote: char) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find(marker) {
        rest = &rest[at + marker.len()..];
        if let Some(end) = rest.find(quote) {
            out.push(&rest[..end]);
        }
    }
    out
}

fn sources(dir: &Path, extensions: &[&str], out: &mut Vec<(String, String)>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            sources(&path, extensions, out);
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| extensions.contains(&e))
        {
            out.push((
                path.display().to_string(),
                std::fs::read_to_string(&path).unwrap(),
            ));
        }
    }
}

/// A route added by hand, outside `ROUTES`, would be neither checked here nor
/// offered as a tool.
#[test]
fn the_source_adds_no_route_outside_the_table() {
    let mut files = Vec::new();
    sources(&Path::new(ROOT).join("src"), &["rs"], &mut files);
    let mut seen = BTreeSet::new();
    for (file, text) in &files {
        for path in literals_after(text, ".route(\"", '"') {
            seen.insert(path);
            assert!(
                NOT_API.iter().any(|(p, _)| *p == path),
                "{file} routes {path} by hand; add it to http::ROUTES"
            );
        }
    }
    for (path, why) in NOT_API {
        assert!(!why.is_empty(), "{path} needs a reason");
        assert!(seen.contains(path), "NOT_API names {path}, which is gone");
    }
}

/// The page and the CLI call only routes and operations that exist, so what
/// they use is what the table, and the tools, cover.
#[test]
fn the_page_and_the_cli_use_only_listed_routes_and_operations() {
    let mut files = Vec::new();
    sources(
        &Path::new(ROOT).join("spa/src"),
        &["ts", "svelte"],
        &mut files,
    );
    let mut ops_used = BTreeSet::new();
    let mut paths_used = BTreeSet::new();
    for (file, text) in &files {
        // `op<Entry[]>('time_list', args)`
        let mut rest = text.as_str();
        while let Some(at) = rest.find("op<") {
            let standalone = !rest[..at]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_');
            rest = &rest[at + 3..];
            // Past the type argument, however nested.
            let mut depth = 1;
            let Some(close) = rest.find(|c| {
                depth += match c {
                    '<' => 1,
                    '>' => -1,
                    _ => 0,
                };
                depth == 0
            }) else {
                break;
            };
            let after = &rest[close + 1..];
            if let Some(name) = after.strip_prefix("('").filter(|_| standalone) {
                let name = &name[..name.find('\'').unwrap()];
                ops_used.insert((file.clone(), name.to_string()));
            }
        }
        // `'api/state'`, `` `api/ops/${name}` ``, `` `api/export?${params}` ``
        for quote in ['\'', '`'] {
            for path in literals_after(text, &format!("{quote}api/"), quote) {
                let path = path.split(['?', '$']).next().unwrap();
                let path = match path.strip_suffix('/') {
                    Some(prefix) => format!("/api/{prefix}/{{name}}"),
                    None => format!("/api/{path}"),
                };
                paths_used.insert((file.clone(), path));
            }
        }
    }
    let main = std::fs::read_to_string(Path::new(ROOT).join("src/main.rs")).unwrap();
    for prefix in ["time_", "project_", "proposal_"] {
        for name in literals_after(&main, &format!("\"{prefix}"), '"') {
            ops_used.insert(("src/main.rs".into(), format!("{prefix}{name}")));
        }
    }
    let client = std::fs::read_to_string(Path::new(ROOT).join("src/client.rs")).unwrap();
    for path in literals_after(&client, "\"api/", '"') {
        let path = match path.strip_suffix("/{op}") {
            Some(prefix) => format!("/api/{prefix}/{{name}}"),
            None => format!("/api/{path}"),
        };
        paths_used.insert(("src/client.rs".into(), path));
    }

    assert!(
        ops_used.len() > 10,
        "found too few operations: {ops_used:?}"
    );
    for (file, name) in &ops_used {
        assert!(ops::find(name).is_some(), "{file} calls unknown {name}");
    }
    assert!(
        paths_used.len() >= 4,
        "found too few routes: {paths_used:?}"
    );
    for (file, path) in &paths_used {
        assert!(
            ROUTES.iter().any(|r| r.path == path),
            "{file} calls {path}, which is not in http::ROUTES"
        );
    }
}
