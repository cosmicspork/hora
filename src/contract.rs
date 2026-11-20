//! The caller context a Tracon node signs onto everything it sends hora as a
//! plugin, and its check. Wire format v1, shared with Tracon's plugin host
//! and the other plugins: an object holding `v`, `plugin`, `instance`,
//! `tool`, `arguments_sha256`, `caller`, `channel`, `approval_id`,
//! `issued_ms`, `expires_ms`, `nonce`, `credentials` and `signature`. The
//! signature is ed25519 by the node key over the ASCII label
//! `tracon/v1/plugin-call`, a big-endian u32 length, and the canonical JSON of
//! the object without `signature`.
//!
//! It arrives two ways. A `tools/call` carries it as `params._meta.tracon`,
//! with `arguments_sha256` the hash of the call's arguments. A request from
//! hora's page in the node's UI carries it base64url-encoded in the
//! `tracon-context` header, as the tool `tracon/ui`, whose "arguments" are the
//! HTTP request itself: method, request target and a hash of the body.

use std::collections::HashMap;
use std::sync::Mutex;

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::model::Actor;

/// What the signature is over, ahead of the context.
pub const LABEL: &str = "tracon/v1/plugin-call";
/// The manifest name a context must be addressed to.
pub const PLUGIN: &str = "hora";
/// The tool a UI request's context names; no real tool can have a `/`.
pub const UI_TOOL: &str = "tracon/ui";
/// Clock skew allowed either side of a context's window.
pub const SKEW_MS: i64 = 5_000;
/// The longest window a context may claim.
pub const MAX_WINDOW_MS: i64 = 60_000;
/// The author recorded for an agent whose harness gives no usable name.
pub const UNNAMED_AGENT: &str = "agent";

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// JSON with object keys sorted by byte order and no whitespace, strings and
/// keys escaped as serde_json escapes them: the bytes Tracon hashes and signs.
pub fn canonical(value: &Value) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, key) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String((*key).clone()).to_string());
                out.push(':');
                write_canonical(&map[*key], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

fn has_float(v: &Value) -> bool {
    match v {
        Value::Number(n) => n.is_f64(),
        Value::Array(a) => a.iter().any(has_float),
        Value::Object(o) => o.values().any(has_float),
        _ => false,
    }
}

/// What a context carries as `instance`: the hash of the plugin token, so the
/// token itself never travels.
pub fn instance_of(token: &str) -> String {
    sha256_hex(token.as_bytes())
}

/// The `arguments_sha256` of a tool call; missing arguments count as `{}`.
pub fn arguments_sha256(arguments: Option<&Value>) -> String {
    let empty = Value::Object(Map::new());
    sha256_hex(canonical(arguments.unwrap_or(&empty)).as_bytes())
}

/// The request a UI context is about, as the plugin received it.
pub struct UiRequest<'a> {
    /// The method, as sent (the node sends it uppercase).
    pub method: &'a str,
    /// The request target: the path, plus `?` and the raw query when there is one.
    pub target: &'a str,
    pub body: &'a [u8],
}

/// The `arguments_sha256` of a UI request: the hash of the canonical JSON of
/// `{"body_sha256", "method", "path"}`.
pub fn ui_arguments_sha256(request: &UiRequest) -> String {
    let arguments = json!({
        "body_sha256": sha256_hex(request.body),
        "method": request.method,
        "path": request.target,
    });
    sha256_hex(canonical(&arguments).as_bytes())
}

/// The bytes a context is signed over: the label, then the length-prefixed
/// canonical JSON of the context with any `signature` removed.
pub fn signing_bytes(context: &Value) -> Vec<u8> {
    let mut unsigned = context.clone();
    if let Some(o) = unsigned.as_object_mut() {
        o.remove("signature");
    }
    let json = canonical(&unsigned);
    let mut out = LABEL.as_bytes().to_vec();
    out.extend_from_slice(&(json.len() as u32).to_be_bytes());
    out.extend_from_slice(json.as_bytes());
    out
}

/// `context` signed by `key`, as the node signs it. For tests.
pub fn sign(context: &Value, key: &SigningKey) -> Value {
    let signature = key.sign(&signing_bytes(context));
    let mut signed = context.clone();
    signed["signature"] = Value::String(hex::encode(signature.to_bytes()));
    signed
}

/// What plugin mode was started with.
#[derive(Clone)]
pub struct ContractEnv {
    /// `hex(sha256(TRACON_PLUGIN_TOKEN))`.
    pub instance: String,
    pub node_key: VerifyingKey,
}

impl ContractEnv {
    pub fn new(plugin_token: &str, node_key_hex: &str) -> Result<Self, String> {
        if plugin_token.is_empty() {
            return Err("TRACON_PLUGIN_TOKEN is empty".into());
        }
        let bytes: [u8; 32] = hex::decode(node_key_hex.trim())
            .ok()
            .and_then(|b| b.try_into().ok())
            .ok_or("TRACON_NODE_KEY is not 32 bytes of hex")?;
        let node_key = VerifyingKey::from_bytes(&bytes)
            .map_err(|e| format!("TRACON_NODE_KEY is not an ed25519 key: {e}"))?;
        Ok(Self {
            instance: instance_of(plugin_token),
            node_key,
        })
    }
}

/// Nonces seen and the `expires_ms` of the context each came in, pruned once
/// a context that old could no longer pass the clock check.
#[derive(Default)]
pub struct NonceCache(Mutex<HashMap<String, i64>>);

impl NonceCache {
    /// Record `nonce`; false when it was already there.
    fn fresh(&self, nonce: &str, expires_ms: i64, now_ms: i64) -> bool {
        let mut seen = self.0.lock().unwrap_or_else(|e| e.into_inner());
        seen.retain(|_, expires| *expires + SKEW_MS >= now_ms);
        if seen.contains_key(nonce) {
            return false;
        }
        seen.insert(nonce.to_string(), expires_ms);
        true
    }
}

/// Who a verified context says is calling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    /// `operator` or `agent`; `plugin` never passes the check.
    pub kind: String,
    pub session: Option<String>,
    pub harness: Option<String>,
    pub channel: String,
}

impl Caller {
    /// Who hora records as the author: the operator, or the agent named by
    /// the harness the node says made the call. The name is the node's, not
    /// the agent's own claim.
    pub fn actor(&self) -> Actor {
        if self.kind == "operator" {
            return Actor::Operator;
        }
        let name = self
            .harness
            .as_deref()
            .filter(|h| crate::http::valid_agent_name(h))
            .unwrap_or(UNNAMED_AGENT);
        Actor::Agent(name.to_string())
    }
}

/// Why a request was refused, and the check (1–7) that refused it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub check: u8,
    pub reason: String,
}

fn refuse<T>(check: u8, reason: impl Into<String>) -> Result<T, Refusal> {
    Err(Refusal {
        check,
        reason: reason.into(),
    })
}

fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Checks 1 to 6, for a context that must be about `tool` with arguments
/// hashing to `args_hash`:
///
/// 1. `v` is 1 and `plugin` is hora (and the context is well formed)
/// 2. `instance` is the hash of this process's plugin token
/// 3. `tool` and `arguments_sha256` match what is being served
/// 4. the signature verifies under the node key
/// 5. now is within the window, give or take [`SKEW_MS`], and the window is
///    at most [`MAX_WINDOW_MS`]
/// 6. the nonce has not been seen
///
/// Check 7, who may call, is the caller's: [`verify_ui`] and [`verify_call`].
fn verify(
    meta: &Value,
    tool: &str,
    args_hash: &str,
    env: &ContractEnv,
    now_ms: i64,
    nonces: &NonceCache,
) -> Result<Caller, Refusal> {
    // 1
    let Some(ctx) = meta.as_object() else {
        return refuse(1, "the request carries no caller context");
    };
    if ctx.get("v").and_then(Value::as_i64) != Some(1) {
        return refuse(1, "caller context version is not 1");
    }
    if ctx.get("plugin").and_then(Value::as_str) != Some(PLUGIN) {
        return refuse(1, format!("caller context is not for {PLUGIN}"));
    }
    if has_float(meta) {
        return refuse(1, "caller context holds a non-integer number");
    }
    let str_of = |k: &str| ctx.get(k).and_then(Value::as_str);
    let (Some(instance), Some(ctx_tool), Some(ctx_args), Some(channel), Some(nonce), Some(sig)) = (
        str_of("instance"),
        str_of("tool"),
        str_of("arguments_sha256"),
        str_of("channel"),
        str_of("nonce"),
        str_of("signature"),
    ) else {
        return refuse(1, "caller context is missing a field");
    };
    let (Some(issued_ms), Some(expires_ms)) = (
        ctx.get("issued_ms").and_then(Value::as_i64),
        ctx.get("expires_ms").and_then(Value::as_i64),
    ) else {
        return refuse(1, "caller context has no integer issued_ms and expires_ms");
    };
    let Some(caller) = ctx.get("caller").and_then(Value::as_object) else {
        return refuse(1, "caller context has no caller");
    };
    let Some(kind) = caller.get("kind").and_then(Value::as_str) else {
        return refuse(1, "caller context has no caller kind");
    };
    let optional = |k: &str| caller.get(k).and_then(Value::as_str).map(str::to_string);

    // 2
    if instance != env.instance {
        return refuse(2, "caller context is for another instance of this plugin");
    }

    // 3
    if ctx_tool != tool {
        return refuse(3, format!("caller context is for {ctx_tool}, not {tool}"));
    }
    if ctx_args != args_hash {
        return refuse(3, "caller context is for other arguments");
    }

    // 4
    let signature = hex::decode(sig)
        .ok()
        .and_then(|b| <[u8; 64]>::try_from(b).ok())
        .map(|b| Signature::from_bytes(&b));
    let Some(signature) = signature else {
        return refuse(4, "caller context signature is not 64 bytes of hex");
    };
    if env
        .node_key
        .verify(&signing_bytes(meta), &signature)
        .is_err()
    {
        return refuse(
            4,
            "caller context signature does not verify under the node key",
        );
    }

    // 5
    if expires_ms < issued_ms || expires_ms - issued_ms > MAX_WINDOW_MS {
        return refuse(5, "caller context window is longer than 60 s");
    }
    if now_ms < issued_ms - SKEW_MS {
        return refuse(5, "caller context is issued in the future");
    }
    if now_ms > expires_ms + SKEW_MS {
        return refuse(5, "caller context has expired");
    }

    // 6
    if !is_hex(nonce, 32) {
        return refuse(6, "caller context nonce is not 32 lowercase hex characters");
    }
    if !nonces.fresh(nonce, expires_ms, now_ms) {
        return refuse(6, "caller context nonce was already used");
    }

    Ok(Caller {
        kind: kind.to_string(),
        session: optional("session"),
        harness: optional("harness"),
        channel: channel.to_string(),
    })
}

/// Check a UI request's context: checks 1 to 6 for `tracon/ui` and exactly
/// this request, then 7: the caller is the operator. An agent never reaches
/// the person's API, whatever it was lent. Nothing is done for a refused
/// request.
pub fn verify_ui(
    meta: &Value,
    request: &UiRequest,
    env: &ContractEnv,
    now_ms: i64,
    nonces: &NonceCache,
) -> Result<Caller, Refusal> {
    let caller = verify(
        meta,
        UI_TOOL,
        &ui_arguments_sha256(request),
        env,
        now_ms,
        nonces,
    )?;
    if caller.kind != "operator" {
        return refuse(
            7,
            format!("hora's page is the operator's, not the {}'s", caller.kind),
        );
    }
    Ok(caller)
}

/// Check a tool call's context: checks 1 to 6 for `tool` and exactly these
/// arguments, then 7: the caller is an agent or the operator. No approval is
/// required: an agent's writes are drafts and proposals the operator confirms
/// in hora, which is the review. Nothing is done for a refused call.
pub fn verify_call(
    meta: &Value,
    tool: &str,
    arguments: Option<&Value>,
    env: &ContractEnv,
    now_ms: i64,
    nonces: &NonceCache,
) -> Result<Caller, Refusal> {
    let caller = verify(
        meta,
        tool,
        &arguments_sha256(arguments),
        env,
        now_ms,
        nonces,
    )?;
    if caller.kind != "agent" && caller.kind != "operator" {
        return refuse(
            7,
            format!("hora's tools are not offered to a {}", caller.kind),
        );
    }
    Ok(caller)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> SigningKey {
        SigningKey::from_bytes(&[7; 32])
    }

    fn env() -> ContractEnv {
        ContractEnv::new("token", &hex::encode(key().verifying_key().to_bytes())).unwrap()
    }

    const NONCE: &str = "00112233445566778899aabbccddeeff";

    fn context(tool: &str, args_hash: String, now: i64, nonce: &str, kind: &str) -> Value {
        sign(
            &json!({
                "v": 1, "plugin": PLUGIN, "instance": instance_of("token"), "tool": tool,
                "arguments_sha256": args_hash,
                "caller": { "kind": kind, "session": "s-1", "harness": "claude" },
                "channel": "personal", "approval_id": null,
                "issued_ms": now, "expires_ms": now + 30_000, "nonce": nonce,
                "credentials": {},
            }),
            &key(),
        )
    }

    fn ui(request: &UiRequest, now: i64, nonce: &str, kind: &str) -> Value {
        context(UI_TOOL, ui_arguments_sha256(request), now, nonce, kind)
    }

    fn request<'a>(target: &'a str, body: &'a [u8]) -> UiRequest<'a> {
        UiRequest {
            method: "POST",
            target,
            body,
        }
    }

    #[test]
    fn canonical_json_sorts_keys_and_drops_whitespace() {
        assert_eq!(
            canonical(&json!({ "b": [1, { "d": null, "c": "x\"y" }], "a": true })),
            r#"{"a":true,"b":[1,{"c":"x\"y","d":null}]}"#
        );
    }

    /// The valid context from the shared v1 vectors (jira-review's
    /// `tests/contract_vectors.json`): hora signs and checks the same bytes.
    #[test]
    fn signing_bytes_match_the_shared_vectors() {
        let context: Value = serde_json::from_str(
            r#"{"approval_id": "apv-0001", "arguments_sha256": "82b5bab1db2a4062874200d0b65f1cf461af93aea12cd658f881fad24f99f674", "caller": {"harness": "claude", "kind": "agent", "session": "01a1e94c-d876-7750-b2c0-2aa620c31655"}, "channel": "personal", "credentials": {"jira-work": {"JIRA_EMAIL": "me@example.com", "JIRA_TOKEN": "not-a-real-token", "JIRA_URL": "https://example.atlassian.net"}}, "expires_ms": 1794900029000, "instance": "00c1f614317a651050823f12608d02702eec50feae4627cd3192b56e7d5018cc", "issued_ms": 1794899999000, "nonce": "0000000000000000000000005eed0000", "plugin": "jira-review", "signature": "e167dba19431571b95e9cdbd9d4e640760bedbf1b134ccaa60bca0b15ef78babc7de6c6b306d090b0a254ccefeeaf4c0663a71171c92d457bd83b9f40e30dd01", "tool": "update", "v": 1}"#,
        )
        .unwrap();
        let env = ContractEnv::new(
            "jira-review-test-plugin-token",
            "79b5562e8fe654f94078b112e8a98ba7901f853ae695bed7e0e3910bad049664",
        )
        .unwrap();
        assert_eq!(env.instance, context["instance"]);
        let sig: [u8; 64] = hex::decode(context["signature"].as_str().unwrap())
            .unwrap()
            .try_into()
            .unwrap();
        env.node_key
            .verify(&signing_bytes(&context), &Signature::from_bytes(&sig))
            .unwrap();
    }

    #[test]
    fn a_good_ui_context_gives_the_operator_and_channel() {
        let req = request("/api/ops/time_log?x=1", b"{\"minutes\":5}");
        let ctx = ui(&req, 1_000_000, NONCE, "operator");
        let caller = verify_ui(&ctx, &req, &env(), 1_000_000, &NonceCache::default()).unwrap();
        assert_eq!(caller.channel, "personal");
        assert_eq!(caller.actor(), Actor::Operator);
    }

    #[test]
    fn each_ui_check_refuses_in_order() {
        let req = request("/api/ops/time_log", b"{}");
        let check = |ctx: &Value, req: &UiRequest, now: i64| {
            verify_ui(ctx, req, &env(), now, &NonceCache::default())
                .unwrap_err()
                .check
        };
        assert_eq!(check(&Value::Null, &req, 0), 1);

        let mut ctx = ui(&req, 0, NONCE, "operator");
        ctx["plugin"] = json!("hoja");
        assert_eq!(check(&ctx, &req, 0), 1);

        let mut ctx = ui(&req, 0, NONCE, "operator");
        ctx["instance"] = json!(instance_of("other"));
        assert_eq!(check(&sign(&ctx, &key()), &req, 0), 2);

        let mut ctx = ui(&req, 0, NONCE, "operator");
        ctx["tool"] = json!("time_log");
        assert_eq!(check(&sign(&ctx, &key()), &req, 0), 3);
        let ctx = ui(&req, 0, NONCE, "operator");
        assert_eq!(check(&ctx, &request("/api/ops/time_log", b"{ }"), 0), 3);
        assert_eq!(check(&ctx, &request("/api/ops/time_log?x", b"{}"), 0), 3);
        assert_eq!(check(&ctx, &request("/api/ops/time_confirm", b"{}"), 0), 3);
        let get = UiRequest {
            method: "GET",
            ..request("/api/ops/time_log", b"{}")
        };
        assert_eq!(check(&ctx, &get, 0), 3);

        let mut ctx = ui(&req, 0, NONCE, "operator");
        ctx["channel"] = json!("work");
        assert_eq!(check(&ctx, &req, 0), 4);

        let ctx = ui(&req, 0, NONCE, "operator");
        assert_eq!(check(&ctx, &req, 36_000), 5);
        assert_eq!(check(&ctx, &req, -6_000), 5);

        let ctx = ui(&req, 0, "not-hex", "operator");
        assert_eq!(check(&ctx, &req, 0), 6);

        let ctx = ui(&req, 0, NONCE, "agent");
        assert_eq!(check(&ctx, &req, 0), 7);
    }

    #[test]
    fn a_tool_call_is_checked_against_its_own_arguments() {
        let args = json!({"project": "acme", "minutes": 30, "description": "x"});
        let ctx = context("time_log", arguments_sha256(Some(&args)), 0, NONCE, "agent");
        let caller = verify_call(
            &ctx,
            "time_log",
            Some(&args),
            &env(),
            0,
            &NonceCache::default(),
        )
        .unwrap();
        assert_eq!(caller.actor(), Actor::Agent("claude".into()));
        assert_eq!(caller.session.as_deref(), Some("s-1"));

        let check = |tool: &str, args: Option<&Value>| {
            verify_call(&ctx, tool, args, &env(), 0, &NonceCache::default())
                .unwrap_err()
                .check
        };
        assert_eq!(check("time_edit", Some(&args)), 3);
        assert_eq!(check("time_log", Some(&json!({"minutes": 31}))), 3);
        assert_eq!(check("time_log", None), 3);

        // Missing arguments hash as `{}`.
        let ctx = context("time_status", arguments_sha256(None), 0, NONCE, "agent");
        verify_call(
            &ctx,
            "time_status",
            Some(&json!({})),
            &env(),
            0,
            &NonceCache::default(),
        )
        .unwrap();

        let ctx = context("time_status", arguments_sha256(None), 0, NONCE, "plugin");
        let refused = verify_call(&ctx, "time_status", None, &env(), 0, &NonceCache::default());
        assert_eq!(refused.unwrap_err().check, 7);
    }

    #[test]
    fn the_author_is_the_harness_or_a_placeholder() {
        let caller = |kind: &str, harness: Option<&str>| Caller {
            kind: kind.into(),
            session: None,
            harness: harness.map(str::to_string),
            channel: "personal".into(),
        };
        assert_eq!(
            caller("agent", Some("external")).actor(),
            Actor::Agent("external".into())
        );
        assert_eq!(
            caller("agent", Some("not a name")).actor(),
            Actor::Agent(UNNAMED_AGENT.into())
        );
        assert_eq!(
            caller("agent", None).actor(),
            Actor::Agent(UNNAMED_AGENT.into())
        );
        assert_eq!(caller("operator", Some("x")).actor(), Actor::Operator);
    }

    #[test]
    fn a_replayed_nonce_is_refused() {
        let req = request("/api/state", b"");
        let nonces = NonceCache::default();
        let ctx = ui(&req, 0, NONCE, "operator");
        verify_ui(&ctx, &req, &env(), 0, &nonces).unwrap();
        assert_eq!(
            verify_ui(&ctx, &req, &env(), 1, &nonces).unwrap_err().check,
            6
        );
    }

    #[test]
    fn a_missing_or_malformed_node_key_is_refused_at_start() {
        assert!(ContractEnv::new("t", "").is_err());
        assert!(ContractEnv::new("t", "zz").is_err());
        assert!(ContractEnv::new("", &hex::encode([1u8; 32])).is_err());
    }

    #[test]
    fn nonces_are_forgotten_once_their_context_could_not_pass() {
        let nonces = NonceCache::default();
        assert!(nonces.fresh("a", 100, 0));
        assert!(!nonces.fresh("a", 100, 0));
        assert!(nonces.fresh("b", 10_000, 5_101));
        assert!(nonces.fresh("a", 10_000, 5_101));
    }
}
