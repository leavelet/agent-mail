//! HTTP client for the amail server, authenticated with a node token.

use amail_proto::{
    ClaimRequest, ClaimResponse, DeliveryClass, InboxResponse, LaunchInfo, PendingResponse,
    PresenceReport, Priority,
};
use anyhow::{Context, Result, bail};
use futures::StreamExt;
use std::time::Duration;

/// Total time the node spends on server calls for one hook; well below the
/// hook client's 300 ms wait (see [`crate::hook_client`]).
pub const HOOK_SERVER_TIMEOUT: Duration = Duration::from_millis(150);

#[derive(Clone)]
pub struct ServerClient {
    base: String,
    token: String,
    http: reqwest::Client,
    /// Timeout of requests made while a hook waits.
    fast: Duration,
}

fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

impl ServerClient {
    pub fn new(base: &str, token: &str, fast: Duration) -> Result<Self> {
        let http = reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_secs(5))
            .pool_idle_timeout(Duration::from_secs(60))
            .build()?;
        Ok(ServerClient {
            base: base.trim_end_matches('/').to_string(),
            token: token.to_string(),
            http,
            fast,
        })
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("{}/v1{}", self.base, path))
            .bearer_auth(&self.token)
    }

    async fn json<T: serde::de::DeserializeOwned>(rb: reqwest::RequestBuilder) -> Result<T> {
        let resp = rb.send().await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            bail!("HTTP {status}: {text}");
        }
        Ok(resp.json().await?)
    }

    pub async fn pending(&self, seat: Option<&str>) -> Result<PendingResponse> {
        let path = match seat {
            Some(s) => format!("/pending?seat={}", enc(s)),
            None => "/pending".into(),
        };
        Self::json(
            self.req(reqwest::Method::GET, &path)
                .timeout(Duration::from_secs(10)),
        )
        .await
    }

    /// Claim up to `limit` pending deliveries of `seat` in `classes` (only
    /// urgent ones if `urgent_only`). Bodies up to
    /// [`crate::inject::INLINE_BODY_MAX`] characters come inline and those
    /// deliveries are marked read: the node shows them in full.
    pub async fn claim(
        &self,
        seat: &str,
        classes: &[DeliveryClass],
        urgent_only: bool,
        via: &str,
        limit: u32,
    ) -> Result<ClaimResponse> {
        let body = ClaimRequest {
            classes: classes.to_vec(),
            min_priority: urgent_only.then_some(Priority::Urgent),
            via: via.to_string(),
            limit: Some(limit),
            inline_body_max: Some(crate::inject::INLINE_BODY_MAX),
            include_digest: false,
            mark_inlined_read: true,
        };
        Self::json(
            self.req(reqwest::Method::POST, &format!("/claim?seat={}", enc(seat)))
                .timeout(self.fast)
                .json(&body),
        )
        .await
    }

    pub async fn unread(&self, seat: &str, limit: u32) -> Result<InboxResponse> {
        Self::json(
            self.req(
                reqwest::Method::GET,
                &format!("/inbox?seat={}&unread=1&limit={limit}", enc(seat)),
            )
            .timeout(self.fast),
        )
        .await
    }

    pub async fn presence(&self, report: &PresenceReport) -> Result<()> {
        let resp = self
            .req(reqwest::Method::POST, "/presence")
            .timeout(Duration::from_secs(10))
            .json(report)
            .send()
            .await?;
        if !resp.status().is_success() {
            bail!(
                "HTTP {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            );
        }
        Ok(())
    }

    pub async fn launch(&self, seat: &str) -> Result<LaunchInfo> {
        Self::json(
            self.req(
                reqwest::Method::GET,
                &format!("/seats/{}/launch", enc(seat)),
            )
            .timeout(Duration::from_secs(10)),
        )
        .await
        .with_context(|| format!("fetching launch info for seat {seat:?}"))
    }

    /// Open the org's event stream. Yields `(event, data, id)` triples.
    pub async fn events(
        &self,
        last_event_id: Option<&str>,
    ) -> Result<impl futures::Stream<Item = Result<SseEvent>> + use<>> {
        let mut rb = self.req(reqwest::Method::GET, "/events");
        if let Some(id) = last_event_id {
            rb = rb.header("Last-Event-ID", id);
        }
        let resp = rb.send().await?;
        if !resp.status().is_success() {
            bail!(
                "HTTP {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            );
        }
        let mut parser = SseParser::default();
        Ok(resp
            .bytes_stream()
            .map(move |chunk| -> Result<Vec<SseEvent>> {
                let chunk = chunk?;
                Ok(parser.feed(&chunk))
            })
            .flat_map(|r| {
                futures::stream::iter(match r {
                    Ok(v) => v.into_iter().map(Ok).collect::<Vec<_>>(),
                    Err(e) => vec![Err(e)],
                })
            }))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SseEvent {
    pub event: String,
    pub data: String,
    pub id: Option<String>,
}

/// Incremental `text/event-stream` parser.
#[derive(Default)]
pub struct SseParser {
    buf: Vec<u8>,
    cur: SseEvent,
}

impl SseParser {
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<SseEvent> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some(nl) = self.buf.iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = self.buf.drain(..=nl).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            let line = String::from_utf8_lossy(&line);
            if line.is_empty() {
                let ev = std::mem::take(&mut self.cur);
                if !ev.data.is_empty() || !ev.event.is_empty() {
                    out.push(ev);
                }
                continue;
            }
            if line.starts_with(':') {
                continue;
            }
            let (field, value) = match line.split_once(':') {
                Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
                None => (line.as_ref(), ""),
            };
            match field {
                "event" => self.cur.event = value.to_string(),
                "data" => {
                    if !self.cur.data.is_empty() {
                        self.cur.data.push('\n');
                    }
                    self.cur.data.push_str(value);
                }
                "id" => self.cur.id = Some(value.to_string()),
                _ => {}
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse() {
        let mut p = SseParser::default();
        assert!(p.feed(b"event: mailbox\ndata: {\"a\"").is_empty());
        let v = p.feed(b":1}\nid: 5\n\n: keepalive\n\nevent: presence\r\ndata: x\r\n\r\n");
        assert_eq!(
            v,
            vec![
                SseEvent {
                    event: "mailbox".into(),
                    data: "{\"a\":1}".into(),
                    id: Some("5".into())
                },
                SseEvent {
                    event: "presence".into(),
                    data: "x".into(),
                    id: None
                }
            ]
        );
    }
}
