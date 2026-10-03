//! Minimal HTTP client for the `/v1` API.

use amail_proto::ErrorBody;
use anyhow::{Context, Result, anyhow, bail};
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
        Ok(())
    }
}

impl std::error::Error for ApiError {}

impl Client {
    /// Configuration: the `--profile` file (TOML with `url` and `token`) if
    /// given, with `AMAIL_URL` / `AMAIL_TOKEN` filling in missing values.
    pub fn from_config(profile: Option<&Path>) -> Result<Client> {
        let p: Profile = match profile {
            Some(path) => {
                let text = std::fs::read_to_string(path)
                    .with_context(|| format!("reading profile {}", path.display()))?;
                toml::from_str(&text)
                    .with_context(|| format!("parsing profile {}", path.display()))?
            }
            None => Profile::default(),
        };
        let url = p
            .url
            .or_else(|| std::env::var("AMAIL_URL").ok())
            .unwrap_or_else(|| DEFAULT_URL.to_string());
        let token = p
            .token
            .or_else(|| std::env::var("AMAIL_TOKEN").ok())
            .ok_or_else(|| anyhow!("no token: set AMAIL_TOKEN or use --profile"))?;
        Ok(Client {
            base: url.trim_end_matches('/').to_string(),
            token,
            http: reqwest::Client::new(),
        })
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
