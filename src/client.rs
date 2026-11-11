//! The CLI's side of the agent API. It holds no state and no privileges: it
//! calls `/api/agent/ops/<name>`, so agents meet the same gates as any caller.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::StatusCode;
use serde_json::Value;
use url::Url;

use crate::http::AGENT_HEADER;

pub struct Client {
    base: Url,
    agent: String,
    http: reqwest::Client,
}

/// A request hora answered with an error.
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub body: Value,
}

impl ApiError {
    pub fn code(&self) -> &str {
        self.body["error"].as_str().unwrap_or("")
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = self.body["message"].as_str().unwrap_or("request failed");
        write!(f, "{message} ({})", self.status)
    }
}
impl std::error::Error for ApiError {}

/// The server must be a plain loopback `http://` origin.
pub fn server_url(raw: &str) -> Result<Url> {
    let url = Url::parse(raw).context("invalid hora server URL")?;
    let loopback = match url.host() {
        Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    };
    if url.scheme() != "http"
        || !loopback
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        bail!(
            "the hora server must be a plain http:// loopback origin, such as http://127.0.0.1:4530"
        );
    }
    Ok(url)
}

impl Client {
    pub fn new(base: Url, agent: String) -> Result<Self> {
        if !crate::http::valid_agent_name(&agent) {
            bail!("the agent name is 1 to 64 letters, digits, dots, dashes or underscores");
        }
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self { base, agent, http })
    }

    pub async fn ops(&self) -> Result<Value> {
        let url = self.base.join("api/agent/ops")?;
        let response = self
            .http
            .get(url)
            .header(AGENT_HEADER, &self.agent)
            .send()
            .await
            .with_context(|| self.unreachable())?;
        Self::read(response).await
    }

    pub async fn call(&self, op: &str, args: Value) -> Result<Value> {
        let url = self.base.join(&format!("api/agent/ops/{op}"))?;
        let response = self
            .http
            .post(url)
            .header(AGENT_HEADER, &self.agent)
            .json(&args)
            .send()
            .await
            .with_context(|| self.unreachable())?;
        Self::read(response).await
    }

    fn unreachable(&self) -> String {
        format!(
            "could not reach hora at {}; is `hora serve` running?",
            self.base
        )
    }

    async fn read(response: reqwest::Response) -> Result<Value> {
        let status = response.status();
        let body: Value = response.json().await.unwrap_or(Value::Null);
        if status.is_success() {
            Ok(body)
        } else {
            Err(ApiError { status, body }.into())
        }
    }
}
