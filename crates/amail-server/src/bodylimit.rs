//! Request bodies declared larger than [`crate::MAX_REQUEST_BYTES`] are
//! answered with 413 `too_large` before any handler runs. A body of at most
//! [`DRAIN_MAX_BYTES`] is read and dropped first, for at most
//! [`DRAIN_TIME`]: when a server closes a connection while request bytes it
//! has not read are still waiting in it, the operating system resets the
//! connection, and the client then sees "connection reset" or "broken pipe"
//! instead of the answer. Larger bodies are not read; the answer asks to
//! close the connection.

use crate::error::ApiError;
use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use futures::StreamExt;
use std::time::Duration;

/// Largest body that is read before it is refused.
pub const DRAIN_MAX_BYTES: u64 = 16 * 1024 * 1024;
/// Longest time spent reading a body that is refused.
pub const DRAIN_TIME: Duration = Duration::from_secs(5);

pub async fn layer(req: Request, next: Next) -> Response {
    let declared = req
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok());
    let Some(n) = declared.filter(|n| *n > crate::MAX_REQUEST_BYTES as u64) else {
        return next.run(req).await;
    };
    if n <= DRAIN_MAX_BYTES {
        drain(req.into_body()).await;
    }
    let mut r = ApiError::new(
        StatusCode::PAYLOAD_TOO_LARGE,
        "too_large",
        format!(
            "a request body is limited to {} bytes",
            crate::MAX_REQUEST_BYTES
        ),
    )
    .into_response();
    r.headers_mut()
        .insert(header::CONNECTION, HeaderValue::from_static("close"));
    r
}

async fn drain(body: Body) {
    let mut s = body.into_data_stream();
    let _ = tokio::time::timeout(DRAIN_TIME, async {
        while let Some(Ok(_)) = s.next().await {}
    })
    .await;
}
