//! Shared by the HTTP test files.

#![allow(dead_code)]

use std::future::Future;
use std::time::Duration;

/// Longest time one test may take. A test that waits on an event stream,
/// or on a request that never gets an answer, fails with this message
/// instead of hanging the run.
pub const TEST_DEADLINE: Duration = Duration::from_secs(240);

/// Run the test `f`, failing it if it is not done within [`TEST_DEADLINE`].
pub async fn deadline(name: &str, f: impl Future<Output = ()>) {
    if tokio::time::timeout(TEST_DEADLINE, f).await.is_err() {
        panic!("test {name} did not finish within {TEST_DEADLINE:?}");
    }
}

/// A client (with a connection pool) of its own for one event stream. The
/// HTTP/1 client can put a connection back into its pool when an answer's
/// head arrives although the answer's body is still being read; with a
/// stream's connection, the next request through that pool would wait
/// behind the stream for ever (about one run in a few hundred of the tests
/// that open streams hung that way). See `amail_node::server::ServerClient`.
pub fn stream_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .build()
        .unwrap()
}

/// How long a stream's request waits for the answer's head.
pub const STREAM_HEAD_TIMEOUT: Duration = Duration::from_secs(30);

/// Send the request that opens a stream (made with [`stream_client`]); its
/// answer's head must come within [`STREAM_HEAD_TIMEOUT`].
pub async fn open_stream(req: reqwest::RequestBuilder) -> reqwest::Response {
    match tokio::time::timeout(STREAM_HEAD_TIMEOUT, req.send()).await {
        Ok(r) => r.expect("stream request failed"),
        Err(_) => panic!("no answer to a stream request within {STREAM_HEAD_TIMEOUT:?}"),
    }
}
