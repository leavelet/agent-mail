//! The web interface: the embedded single-page app, browser sign-in
//! (`/v1/session`), and the security headers every response carries.

use crate::AppState;
use crate::auth::{CSRF_HEADER, SESSION_COOKIE, check_csrf, cookie, hash_token, new_token};
use crate::error::{ApiError, ApiResult, Json};
use crate::peer::Peer;
use amail_proto::{SessionRequest, TokenScope, WhoAmI};
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

mod assets {
    include!(concat!(env!("OUT_DIR"), "/web_assets.rs"));
}

/// Whether this build carries the real interface (not the placeholder).
pub const UI_BUILT: bool = assets::BUILT;

/// How long a browser session lasts.
pub const SESSION_DAYS: i64 = 30;

const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
    img-src 'self' data:; font-src 'self'; connect-src 'self'; object-src 'none'; \
    frame-ancestors 'none'; base-uri 'none'; form-action 'self'";

fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "webmanifest" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// `GET /` and the app's static files. Hashed files under `assets/` are
/// cached for a year; everything else is revalidated.
pub async fn static_file(method: Method, uri: axum::http::Uri) -> Response {
    if method != Method::GET && method != Method::HEAD {
        return ApiError::not_found("no such endpoint").into_response();
    }
    let path = uri.path().trim_start_matches('/');
    // The app's own pages: sign-in, registration, the operator's view.
    let path = match path {
        "" | "register" | "admin" => "index.html",
        p => p,
    };
    let Some((name, bytes)) = assets::ASSETS.iter().find(|(p, _)| *p == path) else {
        return ApiError::not_found("no such endpoint").into_response();
    };
    let cache = if name.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    let mut resp = Response::new(Body::from(*bytes));
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(content_type(name)),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    resp
}

/// Security headers on every response; API responses are not cached.
pub async fn headers(req: Request, next: Next) -> Response {
    let api = req.uri().path().starts_with("/v1/");
    let mut resp = next.run(req).await;
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    if api && !h.contains_key(header::CACHE_CONTROL) {
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    resp
}

/// Whether the session cookie gets `Secure`: always in public mode,
/// otherwise when the request came over HTTPS (through a trusted proxy that
/// says so, or as seen from the page's origin).
pub(crate) fn https(st: &AppState, peer: Peer, headers: &HeaderMap) -> bool {
    if st.config.secure_cookies {
        return true;
    }
    let fwd = peer.proxied
        && headers
            .get("x-forwarded-proto")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.eq_ignore_ascii_case("https"));
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("https://"));
    fwd || origin
}

pub(crate) fn session_cookie(value: &str, max_age: i64, secure: bool) -> HeaderValue {
    let mut c =
        format!("{SESSION_COOKIE}={value}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age}");
    if secure {
        c.push_str("; Secure");
    }
    HeaderValue::from_str(&c).expect("cookie is ASCII")
}

/// `POST /v1/session`: sign a browser in with a user token (or an operator
/// token: an operator session).
pub async fn login(
    State(st): State<AppState>,
    peer: Peer,
    headers: HeaderMap,
    Json(req): Json<SessionRequest>,
) -> ApiResult<Response> {
    if headers.get(CSRF_HEADER).is_none() {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "csrf",
            "sign-in requests must send X-Amail-Csrf: 1",
        ));
    }
    check_csrf(&Method::POST, &headers, peer.proxied)?;
    let hash = hash_token(req.token.trim());
    let Some(auth) = st.store.token_auth(&hash).await? else {
        if let Some(op) = st.store.operator_auth(&hash).await? {
            return operator_login(&st, peer, &headers, op).await;
        }
        return Err(ApiError::unauthorized("invalid or revoked token"));
    };
    if auth.org.suspended_at.is_some() {
        return Err(crate::auth::suspended());
    }
    // Each sign-in stores a session: it counts against the token's rate.
    if let Some(b) = &st.limiters.token_requests
        && let Err(ms) = b.take(&(0, auth.token_id))
    {
        return Err(ApiError::too_many(
            "rate_limited",
            "too many requests for this token",
            ms,
        ));
    }
    if auth.scope != TokenScope::User {
        return Err(ApiError::forbidden(
            "only user tokens can sign in to the web interface",
        ));
    }
    let secret = new_token();
    let max_age = SESSION_DAYS * 86_400;
    st.store
        .create_session(
            &hash_token(&secret),
            auth.token_id,
            amail_store::now_ms() + max_age * 1000,
        )
        .await?;
    let body = WhoAmI {
        org: auth.org.name.clone(),
        scope: auth.scope,
        seat: auth.seat.map(|s| s.name),
    };
    let mut resp = Json(body).into_response();
    resp.headers_mut().insert(
        header::SET_COOKIE,
        session_cookie(&secret, max_age, https(&st, peer, &headers)),
    );
    Ok(resp)
}

/// Operator sessions last a day.
const OPERATOR_SESSION_S: i64 = 86_400;

async fn operator_login(
    st: &AppState,
    peer: Peer,
    headers: &HeaderMap,
    token_id: i64,
) -> ApiResult<Response> {
    let secret = new_token();
    st.store
        .create_operator_session(
            &hash_token(&secret),
            token_id,
            amail_store::now_ms() + OPERATOR_SESSION_S * 1000,
        )
        .await?;
    let mut resp = Json(WhoAmI {
        org: String::new(),
        scope: TokenScope::Operator,
        seat: None,
    })
    .into_response();
    resp.headers_mut().insert(
        header::SET_COOKIE,
        session_cookie(&secret, OPERATOR_SESSION_S, https(st, peer, headers)),
    );
    Ok(resp)
}

/// `DELETE /v1/session`: sign out (always clears the cookie).
pub async fn logout(
    State(st): State<AppState>,
    peer: Peer,
    method: Method,
    headers: HeaderMap,
) -> ApiResult<Response> {
    if let Some(secret) = cookie(&headers, SESSION_COOKIE) {
        check_csrf(&method, &headers, peer.proxied)?;
        let hash = hash_token(secret);
        if st.store.delete_session(&hash).await? {
            st.credentials_ended(Vec::new(), vec![hash]);
        } else {
            st.store.delete_operator_session(&hash).await?;
        }
    }
    let mut resp = StatusCode::NO_CONTENT.into_response();
    resp.headers_mut().insert(
        header::SET_COOKIE,
        session_cookie("", 0, https(&st, peer, &headers)),
    );
    Ok(resp)
}
