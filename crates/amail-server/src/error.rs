use amail_proto::{ErrorBody, ErrorDetail, Rejected};
use amail_store::StoreError;
use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

/// An API error, rendered as `{"error":{"code":…,"message":…}}`.
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
    pub retry_after_ms: Option<u64>,
    pub rejected: Vec<Rejected>,
}

pub type ApiResult<T> = Result<T, ApiError>;

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        ApiError {
            status,
            code,
            message: message.into(),
            retry_after_ms: None,
            rejected: Vec::new(),
        }
    }
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid", msg)
    }
    pub fn unauthorized(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "unauthorized", msg)
    }
    pub fn forbidden(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden", msg)
    }
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", msg)
    }
    pub fn unprocessable(code: &'static str, msg: impl Into<String>) -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, code, msg)
    }
    /// 429 with a retry time.
    pub fn too_many(code: &'static str, msg: impl Into<String>, retry_after_ms: u64) -> Self {
        let mut e = Self::new(StatusCode::TOO_MANY_REQUESTS, code, msg);
        e.retry_after_ms = Some(retry_after_ms);
        e
    }
    /// 507: the message would take the org over its storage quota.
    pub fn storage_quota(stored: i64, limit: Option<i64>) -> Self {
        let limit = match limit {
            Some(l) => format!(" of {}", crate::render::mib(l)),
            None => String::new(),
        };
        Self::new(
            StatusCode::INSUFFICIENT_STORAGE,
            "storage_quota",
            format!(
                "this message does not fit in the org's storage quota{limit} on this server \
                 ({} stored); ask the server's operator",
                crate::render::mib(stored)
            ),
        )
    }
    pub fn rate_limited(retry_after_ms: u64) -> Self {
        let mut e = Self::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            format!(
                "send budget exhausted; retry in {:.1}s",
                retry_after_ms as f64 / 1000.0
            ),
        );
        e.retry_after_ms = Some(retry_after_ms);
        e
    }
}

impl From<StoreError> for ApiError {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::NotFound(m) => Self::not_found(m),
            StoreError::Conflict(m) => Self::new(StatusCode::CONFLICT, "conflict", m),
            StoreError::Invalid(m) => Self::bad_request(m),
            StoreError::StorageQuota(stored) => Self::storage_quota(stored, None),
            other => {
                tracing::error!("store error: {other}");
                Self::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal",
                    "internal error",
                )
            }
        }
    }
}

impl From<JsonRejection> for ApiError {
    fn from(e: JsonRejection) -> Self {
        Self::new(e.status(), "invalid", e.body_text())
    }
}

impl From<QueryRejection> for ApiError {
    fn from(e: QueryRejection) -> Self {
        Self::bad_request(e.body_text())
    }
}

impl From<PathRejection> for ApiError {
    fn from(e: PathRejection) -> Self {
        Self::bad_request(e.body_text())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let retry = self.retry_after_ms;
        let body = ErrorBody {
            error: ErrorDetail {
                code: self.code.to_string(),
                message: self.message,
                retry_after_ms: self.retry_after_ms,
                rejected: self.rejected,
            },
        };
        let mut resp = (self.status, axum::Json(body)).into_response();
        if let Some(ms) = retry {
            let secs = ms.div_ceil(1000).max(1);
            if let Ok(v) = HeaderValue::from_str(&secs.to_string()) {
                resp.headers_mut().insert(header::RETRY_AFTER, v);
            }
        }
        resp
    }
}

/// `axum::Json` with errors in the API's JSON format.
#[derive(axum::extract::FromRequest)]
#[from_request(via(axum::Json), rejection(ApiError))]
pub struct Json<T>(pub T);

impl<T: serde::Serialize> IntoResponse for Json<T> {
    fn into_response(self) -> Response {
        axum::Json(self.0).into_response()
    }
}

/// `axum::extract::Query` with errors in the API's JSON format.
#[derive(axum::extract::FromRequestParts)]
#[from_request(via(axum::extract::Query), rejection(ApiError))]
pub struct Query<T>(pub T);

/// `axum::extract::Path` with errors in the API's JSON format.
#[derive(axum::extract::FromRequestParts)]
#[from_request(via(axum::extract::Path), rejection(ApiError))]
pub struct Path<T>(pub T);

/// An optional JSON body: an empty body yields `None`.
pub struct OptJson<T>(pub Option<T>);

impl<S: Send + Sync, T: serde::de::DeserializeOwned> axum::extract::FromRequest<S> for OptJson<T> {
    type Rejection = ApiError;

    async fn from_request(req: axum::extract::Request, state: &S) -> Result<Self, ApiError> {
        let bytes = axum::body::Bytes::from_request(req, state)
            .await
            .map_err(|e| ApiError::bad_request(e.body_text()))?;
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(OptJson(None));
        }
        serde_json::from_slice(&bytes)
            .map(|v| OptJson(Some(v)))
            .map_err(|e| ApiError::bad_request(format!("invalid JSON body: {e}")))
    }
}
