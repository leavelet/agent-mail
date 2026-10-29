//! HTTP client for the amail server, authenticated with a node token.

use amail_proto::limits::{NODE_MAX_EVENT_BYTES, NODE_MAX_RESPONSE_BYTES};
use amail_proto::{
    ClaimRequest, ClaimResponse, DeliveryClass, InboxResponse, LaunchInfo, PendingResponse,
    PresenceReport, Priority,
};
use anyhow::{Context, Result, bail};
use futures::StreamExt;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// The least total time the node spends on server calls for one hook. The
/// budget grows with the latency the node measures (see `Node::hook_budget`),
/// and stays well below the hook client's wait (see [`crate::hook_client`]).
pub const HOOK_SERVER_TIMEOUT: Duration = Duration::from_millis(150);

/// How long a measured round trip counts for the hook budget.
pub const LATENCY_WINDOW: Duration = Duration::from_secs(300);

/// Round trips of answered requests, kept for [`LATENCY_WINDOW`].
#[derive(Default)]
struct Latency {
    samples: VecDeque<(Instant, Duration)>,
}

impl Latency {
    fn record(&mut self, now: Instant, took: Duration) {
        self.samples.push_back((now, took));
        self.trim(now);
        // Bound memory under a flood of requests.
        while self.samples.len() > 4096 {
            self.samples.pop_front();
        }
    }

    fn trim(&mut self, now: Instant) {
        while self
            .samples
            .front()
            .is_some_and(|(t, _)| now.duration_since(*t) > LATENCY_WINDOW)
        {
            self.samples.pop_front();
        }
    }
}

/// What the node measured about the server's answers lately.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LatencyStats {
    /// The slowest answered request in the window.
    pub max: Option<Duration>,
    /// The most recent one.
    pub last: Option<Duration>,
    pub samples: usize,
}

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
    /// For the event stream only: a client (and connection pool) of its
    /// own. The HTTP/1 client can put a connection back into its pool when
    /// the answer's head arrives although the answer's body is still being
    /// read (its "ready for another request" signal can be set while a
    /// request is on the way). For a short answer the next request then
    /// waits until that body ends; behind the event stream, whose body does
    /// not end, it would wait until its timeout.
    stream_http: reqwest::Client,
    /// Timeout of requests made while a hook waits, when the caller gives
    /// none.
    fast: Duration,
    latency: std::sync::Arc<std::sync::Mutex<Latency>>,
}

/// How long the event stream's request waits for the answer's head.
const EVENTS_HEAD_TIMEOUT: Duration = Duration::from_secs(30);

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
        let client = || {
            reqwest::Client::builder()
                .no_proxy()
                .connect_timeout(Duration::from_secs(5))
                .pool_idle_timeout(Duration::from_secs(60))
                .build()
        };
        Ok(ServerClient {
            base: base.trim_end_matches('/').to_string(),
            token: token.to_string(),
            http: client()?,
            stream_http: client()?,
            fast,
            latency: Default::default(),
        })
    }

    /// Round trips of the requests the server answered lately.
    pub fn latency(&self) -> LatencyStats {
        let mut l = self.latency.lock().unwrap();
        l.trim(Instant::now());
        LatencyStats {
            max: l.samples.iter().map(|(_, d)| *d).max(),
            last: l.samples.back().map(|(_, d)| *d),
            samples: l.samples.len(),
        }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("{}/v1{}", self.base, path))
            .bearer_auth(&self.token)
    }

    /// Send and decode; an answered request (success or an HTTP error) is a
    /// measured round trip.
    async fn json<T: serde::de::DeserializeOwned>(&self, rb: reqwest::RequestBuilder) -> Result<T> {
        let started = Instant::now();
        let r = Self::json_inner(rb).await;
        if r.is_ok()
            || r.as_ref()
                .err()
                .is_some_and(|e| e.downcast_ref::<HttpError>().is_some())
        {
            self.latency
                .lock()
                .unwrap()
                .record(Instant::now(), started.elapsed());
        }
        r
    }

    async fn json_inner<T: serde::de::DeserializeOwned>(rb: reqwest::RequestBuilder) -> Result<T> {
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
        self.json(
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
        self.json(
            self.req(reqwest::Method::POST, &format!("/claim?seat={}", enc(seat)))
                .timeout(timeout.unwrap_or(self.fast))
                .json(&body),
        )
        .await
    }

    /// Return the deliveries of `messages` that the claim with `key` took
    /// to "not notified, not read" (the turn that showed them failed).
    pub async fn release(
        &self,
        seat: &str,
        key: &str,
        messages: &[i64],
    ) -> Result<amail_proto::ReleaseResponse> {
        let body = amail_proto::ReleaseRequest {
            key: key.to_string(),
            messages: messages.to_vec(),
        };
        self.json(
            self.req(
                reqwest::Method::POST,
                &format!("/claim/release?seat={}", enc(seat)),
            )
            .timeout(Duration::from_secs(10))
            .json(&body),
        )
        .await
    }

    pub async fn unread(&self, seat: &str, limit: u32, timeout: Duration) -> Result<InboxResponse> {
        self.json(
            self.req(
                reqwest::Method::GET,
                &format!("/inbox?seat={}&unread=1&limit={limit}", enc(seat)),
            )
            .timeout(timeout),
        )
        .await
    }

    /// The unfinished tasks assigned to `seat` (`GET /tasks/mine`), for the
    /// count shown at session start.
    pub async fn tasks_mine(
        &self,
        seat: &str,
        limit: u32,
        timeout: Duration,
    ) -> Result<amail_proto::TaskList> {
        self.json(
            self.req(
                reqwest::Method::GET,
                &format!("/tasks/mine?seat={}&limit={limit}", enc(seat)),
            )
            .timeout(timeout),
        )
        .await
    }

    /// The document index `seat` is shown (`GET /docs/index`), cut by the
    /// server to `max_lines` lines and `max_chars` characters.
    pub async fn docs_index(
        &self,
        seat: &str,
        max_lines: u32,
        max_chars: usize,
        timeout: Duration,
    ) -> Result<amail_proto::DocIndex> {
        self.json(
            self.req(
                reqwest::Method::GET,
                &format!(
                    "/docs/index?seat={}&max_lines={max_lines}&max_chars={max_chars}",
                    enc(seat)
                ),
            )
            .timeout(timeout),
        )
        .await
    }

    /// The seat's handover note and its state (`GET /seats/{name}/handover`).
    pub async fn handover(
        &self,
        seat: &str,
        timeout: Duration,
    ) -> Result<amail_proto::HandoverResponse> {
        self.json(
            self.req(
                reqwest::Method::GET,
                &format!("/seats/{}/handover", enc(seat)),
            )
            .timeout(timeout),
        )
        .await
    }

    /// File a harness's compaction summary as a draft note revision.
    pub async fn handover_compaction(
        &self,
        seat: &str,
        summary: &str,
        session: Option<&str>,
    ) -> Result<amail_proto::HandoverResponse> {
        let body = amail_proto::HandoverWrite {
            text: Some(summary.to_string()),
            session: session.map(str::to_string),
            source: Some(amail_proto::HandoverSource::Compaction),
            ..Default::default()
        };
        self.json(
            self.req(
                reqwest::Method::PUT,
                &format!("/seats/{}/handover", enc(seat)),
            )
            .timeout(Duration::from_secs(10))
            .json(&body),
        )
        .await
    }

    /// Record that `session` was shown revision `rev` of the seat's note.
    pub async fn handover_read(
        &self,
        seat: &str,
        session: &str,
        rev: i64,
    ) -> Result<amail_proto::HandoverReadResponse> {
        let body = amail_proto::HandoverRead {
            session: session.to_string(),
            rev: Some(rev),
        };
        self.json(
            self.req(
                reqwest::Method::POST,
                &format!("/seats/{}/handover/read", enc(seat)),
            )
            .timeout(Duration::from_secs(10))
            .json(&body),
        )
        .await
    }

    /// Ask the seat for its note (system mail to the seat).
    pub async fn handover_request(&self, seat: &str) -> Result<amail_proto::HandoverRequested> {
        self.json(
            self.req(
                reqwest::Method::POST,
                &format!("/seats/{}/handover/request", enc(seat)),
            )
            .timeout(Duration::from_secs(10)),
        )
        .await
    }

    /// The seat's worklog entries of `kind` since `since` (RFC 3339),
    /// newest first.
    pub async fn worklog(
        &self,
        seat: &str,
        kind: &str,
        since: &str,
        limit: u32,
        timeout: Duration,
    ) -> Result<amail_proto::WorklogPage> {
        self.json(
            self.req(
                reqwest::Method::GET,
                &format!(
                    "/seats/{}/worklog?kind={}&since={}&limit={limit}",
                    enc(seat),
                    enc(kind),
                    enc(since)
                ),
            )
            .timeout(timeout),
        )
        .await
    }

    /// The open requests about `seat` (`GET /requests?seat=&state=open`).
    pub async fn open_requests(&self, seat: &str) -> Result<amail_proto::RequestList> {
        self.json(
            self.req(
                reqwest::Method::GET,
                &format!("/requests?seat={}&state=open", enc(seat)),
            )
            .timeout(Duration::from_secs(10)),
        )
        .await
    }

    /// Append worklog entries (`POST /worklog`).
    pub async fn post_worklog(
        &self,
        post: &amail_proto::WorklogPost,
    ) -> Result<amail_proto::WorklogPosted> {
        self.json(
            self.req(reqwest::Method::POST, "/worklog")
                .timeout(Duration::from_secs(15))
                .json(post),
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

    /// Report notices for the person (see [`amail_proto::NodeNotice`]).
    pub async fn notices(&self, report: &amail_proto::NodeNotices) -> Result<()> {
        let resp = self
            .req(reqwest::Method::POST, "/node/notices")
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
        self.json(
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
        self.json(
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
        let mut rb = self
            .stream_http
            .get(format!("{}/v1/events", self.base))
            .bearer_auth(&self.token);
        if let Some(id) = last_event_id {
            rb = rb.header("Last-Event-ID", id);
        }
        let resp = tokio::time::timeout(EVENTS_HEAD_TIMEOUT, rb.send())
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "the event stream did not answer within {} s",
                    EVENTS_HEAD_TIMEOUT.as_secs()
                )
            })??;
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

    /// A raw HTTP/1.1 server with keep-alive: `/v1/events` answers with an
    /// event stream that never ends, anything else with an empty pending
    /// list.
    async fn stream_server() -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    loop {
                        let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
                            match s.read(&mut chunk).await {
                                Ok(0) | Err(_) => return,
                                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                            }
                            continue;
                        };
                        let head = String::from_utf8_lossy(&buf[..end]).to_string();
                        buf.drain(..end + 4);
                        if head.contains("/v1/events") {
                            let _ = s
                                .write_all(
                                    b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                                      transfer-encoding: chunked\r\n\r\n",
                                )
                                .await;
                            // Never ends.
                            let _ = s.read(&mut chunk).await;
                            return;
                        }
                        let body = r#"{"seats":[]}"#;
                        let answer = format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                             content-length: {}\r\n\r\n{body}",
                            body.len()
                        );
                        if s.write_all(answer.as_bytes()).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });
        format!("http://{addr}")
    }

    /// The node asks for pending mail, opens its event stream, and asks
    /// again: the second request must get its answer. With one connection
    /// pool for both, about one round in a few thousand put the stream's
    /// connection back into the pool when its head arrived, and the second
    /// request waited behind the stream until its timeout. Several runtimes
    /// at once (as the node's tasks run) make that likely enough to see in
    /// a few thousand rounds; with a pool of its own for the stream it
    /// cannot happen.
    #[test]
    fn requests_after_the_event_stream_get_answers() {
        const THREADS: usize = 8;
        const ROUNDS: usize = 500;
        let stuck = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let threads: Vec<_> = (0..THREADS)
            .map(|_| {
                let stuck = stuck.clone();
                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_multi_thread()
                        .worker_threads(2)
                        .enable_all()
                        .build()
                        .unwrap();
                    rt.block_on(async move {
                        let base = stream_server().await;
                        for _ in 0..ROUNDS {
                            let c = ServerClient::new(&base, "t", Duration::from_secs(5)).unwrap();
                            c.pending(None).await.unwrap();
                            let events = c.events(None).await.unwrap();
                            let next =
                                tokio::time::timeout(Duration::from_secs(5), c.pending(None)).await;
                            if !matches!(next, Ok(Ok(_))) {
                                stuck.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            }
                            drop(events);
                        }
                    });
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(
            stuck.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "requests after the event stream went unanswered"
        );
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
