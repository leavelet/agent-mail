//! Notification webhook: when an org has `webhook_url` set, every new
//! message delivered to the org's user seat is announced with a POST of its
//! header (never the body). Delivery runs in the background with a few
//! retries and never delays or fails the send.

use amail_proto::MessageHeader;
use serde::Serialize;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct WebhookConfig {
    /// Delays before the second, third, ... attempt.
    pub backoff: Vec<Duration>,
    pub timeout: Duration,
}

impl Default for WebhookConfig {
    fn default() -> Self {
        WebhookConfig {
            backoff: vec![Duration::from_secs(1), Duration::from_secs(5)],
            timeout: Duration::from_secs(10),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Notice {
    pub event: &'static str,
    pub org: String,
    /// The user seat that received the message.
    pub seat: String,
    pub message: MessageHeader,
}

/// Only `http` and `https` URLs without whitespace are accepted.
pub fn valid_url(url: &str) -> bool {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"));
    matches!(rest, Some(r) if !r.is_empty() && !r.starts_with('/'))
        && url.len() <= 2000
        && !url.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// Post the notice in the background. With `private_ok == false` a URL
/// with a literal non-public address is not tried at all (names are checked
/// by the client's resolver when they are looked up).
pub fn spawn(
    http: reqwest::Client,
    cfg: WebhookConfig,
    url: String,
    notice: Notice,
    private_ok: bool,
) {
    tokio::spawn(async move {
        let shown = crate::netguard::redact(&url);
        if let Err(e) = crate::netguard::check_literal(&url, private_ok) {
            tracing::warn!("webhook {shown}: not sent: {e}");
            return;
        }
        let attempts = cfg.backoff.len() + 1;
        for attempt in 0..attempts {
            if attempt > 0 {
                tokio::time::sleep(cfg.backoff[attempt - 1]).await;
            }
            let r = http
                .post(&url)
                .timeout(cfg.timeout)
                .header(reqwest::header::USER_AGENT, "amail")
                .json(&notice)
                .send()
                .await;
            match r {
                Ok(resp) if resp.status().is_success() => return,
                Ok(resp) => tracing::debug!(
                    "webhook {shown}: HTTP {} (attempt {})",
                    resp.status(),
                    attempt + 1
                ),
                Err(e) => tracing::debug!(
                    "webhook {shown}: {} (attempt {})",
                    error_chain(&e),
                    attempt + 1
                ),
            }
        }
        tracing::warn!(
            "webhook {shown}: giving up on message {} after {attempts} attempts",
            notice.message.id
        );
    });
}

/// An error with its causes, without the URL (reqwest puts the full URL in
/// its errors).
fn error_chain(e: &reqwest::Error) -> String {
    let mut out = String::new();
    let mut src: Option<&dyn std::error::Error> = std::error::Error::source(e);
    while let Some(s) = src {
        if !out.is_empty() {
            out.push_str(": ");
        }
        out.push_str(&s.to_string());
        src = s.source();
    }
    if out.is_empty() {
        "request failed".into()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn urls() {
        assert!(super::valid_url("https://example.com/hook"));
        assert!(super::valid_url("http://127.0.0.1:9/x"));
        assert!(!super::valid_url("ftp://example.com"));
        assert!(!super::valid_url("https://"));
        assert!(!super::valid_url("https://a b"));
        assert!(!super::valid_url("javascript:alert(1)"));
    }
}
