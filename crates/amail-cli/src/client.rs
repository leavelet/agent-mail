//! Minimal HTTP client for the `/v1` API.

use amail_proto::ErrorBody;
use anyhow::{Context, Result, bail};
use reqwest::Method;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::path::Path;

pub const DEFAULT_URL: &str = "http://127.0.0.1:8025";

#[derive(Debug, Default, Deserialize)]
struct Profile {
    url: Option<String>,
    token: Option<String>,
}

pub struct Client {
    base: String,
    token: String,
    http: reqwest::Client,
}

/// An API error response.
#[derive(Debug)]
pub struct ApiError {
    pub status: u16,
    pub body: ErrorBody,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.body.error.code, self.body.error.message)?;
        for r in &self.body.error.rejected {
            write!(f, "\nrejected {}: {}", r.address, r.reason)?;
        }
        if self
            .body
            .error
            .rejected
            .iter()
            .any(|r| r.reason == "unreachable")
        {
            write!(f, "\n{}", crate::output::UNREACHABLE)?;
        }
        Ok(())
    }
}

impl std::error::Error for ApiError {}

impl Client {
    /// Configuration, first match wins:
    ///
    /// 1. `--profile` / `AMAIL_PROFILE`: a TOML file with `url` and `token`
    ///    (a path), or the name of a saved login; `AMAIL_URL` /
    ///    `AMAIL_TOKEN` fill in what a file leaves out;
    /// 2. `AMAIL_TOKEN` (with `AMAIL_URL`): how agents' sessions are set up;
    /// 3. inside a Codex session (`CODEX_THREAD_ID`), the seat's credentials
    ///    from the node;
    /// 4. the current saved login (`amail login`).
    pub fn from_config(profile: Option<&Path>) -> Result<Client> {
        let env_url = std::env::var("AMAIL_URL").ok().filter(|v| !v.is_empty());
        let env_token = std::env::var("AMAIL_TOKEN").ok().filter(|v| !v.is_empty());
        let (url, token) = match profile {
            Some(p) if is_saved_name(p) => {
                let name = p.to_string_lossy();
                let cfg = crate::profiles::Config::load()?;
                let (_, pr) = cfg
                    .get(Some(&name))
                    .with_context(|| format!("no saved login {name:?} (see `amail profile ls`)"))?;
                (pr.url.clone(), pr.token.clone())
            }
            Some(path) => {
                let text = std::fs::read_to_string(path)
                    .with_context(|| format!("reading profile {}", path.display()))?;
                let p: Profile = toml::from_str(&text)
                    .with_context(|| format!("parsing profile {}", path.display()))?;
                let url = p.url.or(env_url).unwrap_or_else(|| DEFAULT_URL.to_string());
                let token = p
                    .token
                    .or(env_token)
                    .context("no token: the profile has none and AMAIL_TOKEN is not set")?;
                (url, token)
            }
            None => match env_token {
                Some(t) => (env_url.unwrap_or_else(|| DEFAULT_URL.to_string()), t),
                None => match node_credentials() {
                    Some(c) => (c.url, c.token),
                    None => {
                        let cfg = crate::profiles::Config::load()?;
                        let (_, pr) = cfg.get(None).context(
                            "no token: run `amail login <url>`, set AMAIL_TOKEN, or use --profile",
                        )?;
                        (pr.url.clone(), pr.token.clone())
                    }
                },
            },
        };
        Ok(Client::new(&url, &token))
    }

    pub fn new(url: &str, token: &str) -> Client {
        Client {
            base: url.trim_end_matches('/').to_string(),
            token: token.to_string(),
            http: reqwest::Client::builder()
                .no_proxy()
                .build()
                .expect("building the HTTP client"),
        }
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    pub async fn call(&self, method: Method, path: &str, body: Option<&Value>) -> Result<Value> {
        let url = format!("{}/v1{}", self.base, path);
        let mut req = self.http.request(method, &url).bearer_auth(&self.token);
        if let Some(b) = body {
            req = req.json(b);
        }
        let resp = req
            .send()
            .await
            .with_context(|| format!("connecting to {}", self.base))?;
        let status = resp.status();
        let text = resp.text().await?;
        if status.is_success() {
            if text.is_empty() {
                return Ok(Value::Null);
            }
            return serde_json::from_str(&text).context("invalid JSON from server");
        }
        match serde_json::from_str::<ErrorBody>(&text) {
            Ok(body) => Err(ApiError {
                status: status.as_u16(),
                body,
            }
            .into()),
            Err(_) => bail!("HTTP {status}: {text}"),
        }
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        Ok(serde_json::from_value(
            self.call(Method::GET, path, None).await?,
        )?)
    }

    pub async fn raw(&self, method: Method, path: &str, body: Option<&Value>) -> Result<Value> {
        self.call(method, path, body).await
    }
}

/// `--profile x` names a saved login when it is not a path to a file.
fn is_saved_name(p: &Path) -> bool {
    let s = p.to_string_lossy();
    !p.exists() && !s.contains('/') && !s.ends_with(".toml")
}

/// Inside a Codex session, tool commands run in the shared Codex daemon and
/// do not see the session's environment; the node knows which seat owns the
/// thread (`CODEX_THREAD_ID`) and holds its token. It answers only requests
/// that carry the node's Codex pass, which the daemon's environment holds.
fn node_credentials() -> Option<amail_node::wire::Credentials> {
    let thread = std::env::var("CODEX_THREAD_ID")
        .ok()
        .filter(|t| !t.is_empty())?;
    let v = amail_node::request(
        &amail_node::paths::socket(),
        &amail_node::wire::Request::Credentials {
            codex_thread: thread,
            pass: std::env::var(amail_node::wire::CODEX_PASS_ENV).ok(),
        },
        std::time::Duration::from_secs(5),
    )
    .ok()?;
    serde_json::from_value(v).ok()
}

/// Percent-encode a path segment or query value.
pub fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'@') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn is_not_found(e: &anyhow::Error) -> bool {
    e.downcast_ref::<ApiError>()
        .is_some_and(|e| e.status == 404)
}
