//! HTTP client for the amail server, authenticated with a node token.

use amail_proto::limits::{NODE_MAX_EVENT_BYTES, NODE_MAX_RESPONSE_BYTES};
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

/// An HTTP error answer from the server.
#[derive(Debug)]
pub struct HttpError {
    pub status: reqwest::StatusCode,
    pub body: String,
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HTTP {}: {}", self.status, self.body)
    }
}

impl std::error::Error for HttpError {}

/// Whether retrying cannot help: the server refused the request itself
/// (4xx other than timeouts and rate limits).
pub fn is_permanent(e: &anyhow::Error) -> bool {
    e.downcast_ref::<HttpError>().is_some_and(|h| {
        h.status.is_client_error()
            && h.status != reqwest::StatusCode::REQUEST_TIMEOUT
            && h.status != reqwest::StatusCode::TOO_MANY_REQUESTS
    })
}

/// What a claim asks for (kept to retry it with the same key).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ClaimSpec {
    pub classes: Vec<DeliveryClass>,
    pub urgent_only: bool,
    /// Digest deliveries (shown as a digest: previews, not bodies).
    #[serde(default)]
    pub digest: bool,
    pub via: String,
    pub limit: u32,
}

#[derive(Clone)]
pub struct ServerClient {
    base: String,
    token: String,
    http: reqwest::Client,
    /// Timeout of requests made while a hook waits.
    fast: Duration,
}

/// Bytes of an error answer kept for the message.
const ERROR_BODY_MAX: usize = 4096;

/// The body of `resp`, refused when it is larger than `max` bytes.
pub async fn body_limited(mut resp: reqwest::Response, max: usize) -> Result<Vec<u8>> {
    if resp.content_length().is_some_and(|n| n > max as u64) {
        bail!("the server's answer is larger than {max} bytes");
    }
    let mut out = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        if out.len() + chunk.len() > max {
            bail!("the server's answer is larger than {max} bytes");
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

/// The start of an error answer's body (at most [`ERROR_BODY_MAX`] bytes).
async fn error_text(mut resp: reqwest::Response) -> String {
    let mut out = Vec::new();
    while out.len() < ERROR_BODY_MAX {
        match resp.chunk().await {
            Ok(Some(c)) => out.extend_from_slice(&c),
            _ => break,
        }
    }
    out.truncate(ERROR_BODY_MAX);
    String::from_utf8_lossy(&out).into_owned()
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
            let body = error_text(resp).await;
            return Err(HttpError { status, body }.into());
        }
        let body = body_limited(resp, NODE_MAX_RESPONSE_BYTES).await?;
        Ok(serde_json::from_slice(&body)?)
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

    /// Claim pending deliveries of `seat` as `spec` says, with idempotency
    /// key `key` (a retry with the same key gets the same deliveries). For
    /// immediate mail, bodies up to [`crate::inject::INLINE_BODY_MAX`]
    /// characters come inline and those deliveries are marked read: the
    /// node shows them in full. Digest mail comes with a one-line preview.
    /// `timeout`: `None` for the hook deadline.
    pub async fn claim(
        &self,
        seat: &str,
        spec: &ClaimSpec,
        key: &str,
        timeout: Option<Duration>,
    ) -> Result<ClaimResponse> {
        let body = ClaimRequest {
            classes: spec.classes.clone(),
            min_priority: spec.urgent_only.then_some(Priority::Urgent),
            via: spec.via.clone(),
            limit: Some(spec.limit),
            inline_body_max: (!spec.digest).then_some(crate::inject::INLINE_BODY_MAX),
            include_digest: false,
            mark_inlined_read: !spec.digest,
            digest_only: spec.digest,
            key: Some(key.to_string()),
            preview_chars: spec.digest.then_some(crate::inject::PREVIEW_CHARS),
        };
        Self::json(
            self.req(reqwest::Method::POST, &format!("/claim?seat={}", enc(seat)))
                .timeout(timeout.unwrap_or(self.fast))
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
            let status = resp.status();
            bail!("HTTP {}: {}", status, error_text(resp).await);
        }
        Ok(())
    }

    /// Forward transcript events of `seat` to the server's watchers.
    pub async fn post_transcript(
        &self,
        seat: &str,
        post: &amail_proto::TranscriptPost,
    ) -> Result<amail_proto::TranscriptPostResponse> {
        Self::json(
            self.req(
                reqwest::Method::POST,
                &format!("/seats/{}/transcript", enc(seat)),
            )
            .timeout(Duration::from_secs(15))
            .json(post),
        )
        .await
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
            let status = resp.status();
            bail!("HTTP {}: {}", status, error_text(resp).await);
        }
        let mut parser = SseParser::default();
        Ok(resp
            .bytes_stream()
            .map(move |chunk| -> Result<Vec<SseEvent>> {
                let chunk = chunk?;
                parser.feed(&chunk)
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

/// Incremental `text/event-stream` parser. Lines and events are limited to
/// [`NODE_MAX_EVENT_BYTES`] (by default); a longer one is an error, after
/// which the stream is dropped.
pub struct SseParser {
    buf: Vec<u8>,
    cur: SseEvent,
    max: usize,
}

impl Default for SseParser {
    fn default() -> Self {
        SseParser::with_limit(NODE_MAX_EVENT_BYTES)
    }
}

impl SseParser {
    pub fn with_limit(max: usize) -> Self {
        SseParser {
            buf: Vec::new(),
            cur: SseEvent::default(),
            max,
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<SseEvent>> {
        // Only the part not yet searched for a newline needs searching.
        let searched = self.buf.len();
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        let mut from = searched;
        while let Some(nl) = self.buf[from..]
            .iter()
            .position(|&b| b == b'\n')
            .map(|i| i + from)
        {
            from = 0;
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
                    if self.cur.data.len() + value.len() + 1 > self.max {
                        bail!(
                            "an event of the event stream is larger than {} bytes",
                            self.max
                        );
                    }
                    if !self.cur.data.is_empty() {
                        self.cur.data.push('\n');
                    }
                    self.cur.data.push_str(value);
                }
                "id" => self.cur.id = Some(value.to_string()),
                _ => {}
            }
        }
        if self.buf.len() > self.max {
            bail!(
                "a line of the event stream is longer than {} bytes",
                self.max
            );
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_limits() {
        // A line that never ends.
        let mut p = SseParser::with_limit(1000);
        assert!(p.feed(&[b'x'; 600]).unwrap().is_empty());
        assert!(p.feed(&[b'x'; 600]).is_err());
        // An event made of many short data lines.
        let mut p = SseParser::with_limit(1000);
        let mut r = Ok(Vec::new());
        for _ in 0..200 {
            r = p.feed(b"data: 0123456789\n");
            if r.is_err() {
                break;
            }
        }
        assert!(r.is_err());
        // Within the limit, after a long line was split over many chunks.
        let mut p = SseParser::with_limit(1000);
        for _ in 0..9 {
            assert!(p.feed(&[b'y'; 100]).unwrap().is_empty());
        }
        let v = p.feed(b"\ndata: ok\n\n").unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].data, "ok");
    }

    /// A raw HTTP server that answers every request with `head` and then
    /// `chunks` chunks of `size` bytes (chunked encoding unless `head` says
    /// otherwise).
    async fn raw_server(head: &'static str, chunks: usize, size: usize) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let _ = s.read(&mut buf).await;
                    if s.write_all(head.as_bytes()).await.is_err() {
                        return;
                    }
                    let data = vec![b'1'; size];
                    for _ in 0..chunks {
                        let mut c = format!("{size:x}\r\n").into_bytes();
                        c.extend_from_slice(&data);
                        c.extend_from_slice(b"\r\n");
                        if s.write_all(&c).await.is_err() {
                            return;
                        }
                    }
                    let _ = s.write_all(b"0\r\n\r\n").await;
                });
            }
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn bodies_are_limited() {
        let chunked = "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                       transfer-encoding: chunked\r\n\r\n";
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        // Larger than the limit, without a length: cut off while reading.
        let base = raw_server(chunked, 64, 64 * 1024).await;
        let resp = http.get(&base).send().await.unwrap();
        let e = body_limited(resp, 1024 * 1024).await.unwrap_err();
        assert!(e.to_string().contains("larger than"), "{e}");
        // Within the limit.
        let resp = http.get(&base).send().await.unwrap();
        assert_eq!(
            body_limited(resp, 8 * 1024 * 1024).await.unwrap().len(),
            64 * 64 * 1024
        );
        // A declared length above the limit is refused before reading.
        let big = "HTTP/1.1 200 OK\r\ncontent-length: 1000000000\r\n\r\n";
        let base = raw_server(big, 0, 0).await;
        let resp = http.get(&base).send().await.unwrap();
        assert!(body_limited(resp, 1024).await.is_err());
        // The node's own calls go through the limit too: an endless answer
        // to `/v1/pending` is an error, not an allocation of all of it.
        let base = raw_server(chunked, 4096, 64 * 1024).await;
        let c = ServerClient::new(&base, "t", Duration::from_secs(5)).unwrap();
        let e = c.pending(None).await.unwrap_err();
        assert!(e.to_string().contains("larger than"), "{e}");
    }

    #[test]
    fn sse() {
        let mut p = SseParser::default();
        assert!(p.feed(b"event: mailbox\ndata: {\"a\"").unwrap().is_empty());
        let v = p
            .feed(b":1}\nid: 5\n\n: keepalive\n\nevent: presence\r\ndata: x\r\n\r\n")
            .unwrap();
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
