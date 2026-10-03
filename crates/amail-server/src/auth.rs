//! Authentication (bearer tokens, or the web interface's session cookie)
//! and the scope rules.
//!
//! A browser session (`POST /v1/session`) is a random secret in an
//! `HttpOnly`, `SameSite=Strict` cookie, stored hashed and tied to the user
//! token it was created with. Requests authenticated by the cookie that are
//! not `GET`/`HEAD` must carry `X-Amail-Csrf: 1` (a header a cross-site form
//! cannot set), and an `Origin` header, when present, must match `Host`.
//!
//! | scope  | may                                                               |
//! |--------|-------------------------------------------------------------------|
//! | `user` | everything in the org; acts as the org's user seat by default     |
//! | `seat` | send/receive as its seat; read its mailbox and threads; directory |
//! | `node` | `inbox`, `claim`, `changes`, `events` for any seat of the org     |

use crate::AppState;
use crate::error::{ApiError, ApiResult};
use amail_proto::TokenScope;
use amail_store::{Org, Seat};
use axum::extract::FromRequestParts;
use axum::http::header;
use axum::http::request::Parts;
use sha2::{Digest, Sha256};

/// The authenticated caller.
#[derive(Debug, Clone)]
pub struct Auth {
    /// The token's id (for a session: the token it was created with).
    pub token_id: i64,
    pub scope: TokenScope,
    pub org: Org,
    /// The token's seat (`seat` tokens) or the org's user seat (`user` tokens).
    pub seat: Option<Seat>,
}

/// A new random token (`amail_` + 64 hex chars).
pub fn new_token() -> String {
    let bytes: [u8; 32] = rand::random();
    format!("amail_{}", hex::encode(bytes))
}

/// Tokens are stored as SHA-256 hashes. They are high-entropy random strings,
/// so a fast hash is sufficient.
pub fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// Name of the session cookie.
pub const SESSION_COOKIE: &str = "amail_session";
/// Header that cookie-authenticated writes must carry.
pub const CSRF_HEADER: &str = "x-amail-csrf";

/// The value of cookie `name`, if the request has one.
pub fn cookie<'a>(headers: &'a axum::http::HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.trim())
}

/// Cross-site request protection for cookie-authenticated requests: the
/// custom header, and `Origin` (when sent) naming this host.
pub fn check_csrf(method: &axum::http::Method, headers: &axum::http::HeaderMap) -> ApiResult<()> {
    let safe = matches!(*method, axum::http::Method::GET | axum::http::Method::HEAD);
    if !safe && headers.get(CSRF_HEADER).is_none() {
        return Err(ApiError::new(
            axum::http::StatusCode::FORBIDDEN,
            "csrf",
            "requests authenticated by the session cookie must send X-Amail-Csrf: 1",
        ));
    }
    if let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        let host = headers
            .get(header::HOST)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        let origin_host = origin.split_once("://").map(|(_, h)| h).unwrap_or(origin);
        if origin != "null" && !origin_host.eq_ignore_ascii_case(host) {
            return Err(ApiError::new(
                axum::http::StatusCode::FORBIDDEN,
                "csrf",
                "cross-origin request refused",
            ));
        }
    }
    Ok(())
}

impl FromRequestParts<AppState> for Auth {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let auth = match parts.headers.get(header::AUTHORIZATION) {
            Some(value) => {
                let value = value
                    .to_str()
                    .map_err(|_| ApiError::unauthorized("invalid Authorization header"))?;
                let token = value
                    .strip_prefix("Bearer ")
                    .or_else(|| value.strip_prefix("bearer "))
                    .ok_or_else(|| {
                        ApiError::unauthorized("expected 'Authorization: Bearer <token>'")
                    })?
                    .trim();
                state
                    .store
                    .token_auth(&hash_token(token))
                    .await?
                    .ok_or_else(|| ApiError::unauthorized("invalid or revoked token"))?
            }
            None => {
                let secret = cookie(&parts.headers, SESSION_COOKIE)
                    .ok_or_else(|| ApiError::unauthorized("missing bearer token"))?;
                check_csrf(&parts.method, &parts.headers)?;
                state
                    .store
                    .session_auth(&hash_token(secret))
                    .await?
                    .ok_or_else(|| ApiError::unauthorized("session expired or signed out"))?
            }
        };
        Ok(Auth {
            token_id: auth.token_id,
            scope: auth.scope,
            org: auth.org,
            seat: auth.seat,
        })
    }
}

impl Auth {
    pub fn is_user(&self) -> bool {
        self.scope == TokenScope::User
    }

    pub fn require_user(&self) -> ApiResult<()> {
        if self.is_user() {
            Ok(())
        } else {
            Err(ApiError::forbidden("requires a user token"))
        }
    }

    /// The seat this caller sends and reads mail as (user and seat tokens).
    pub fn mail_seat(&self) -> ApiResult<&Seat> {
        match (self.scope, &self.seat) {
            (TokenScope::Node, _) => Err(ApiError::forbidden(
                "node tokens cannot send or read messages",
            )),
            (_, Some(seat)) => Ok(seat),
            (_, None) => Err(ApiError::forbidden("token has no seat")),
        }
    }

    /// Directory reads: user and seat tokens.
    pub fn require_directory(&self) -> ApiResult<()> {
        if self.scope == TokenScope::Node {
            Err(ApiError::forbidden("node tokens cannot read the directory"))
        } else {
            Ok(())
        }
    }

    /// Pick the mailbox for `inbox`, `claim`, `changes`: a seat token's own
    /// seat; for user and node tokens the seat named by `seat=` (user tokens
    /// default to the user seat).
    pub async fn mailbox(&self, state: &AppState, seat: Option<&str>) -> ApiResult<Seat> {
        match self.scope {
            TokenScope::Seat => {
                let own = self.mail_seat()?;
                match seat {
                    Some(name) if name != own.name => Err(ApiError::forbidden(
                        "seat tokens can only access their own mailbox",
                    )),
                    _ => Ok(own.clone()),
                }
            }
            TokenScope::User | TokenScope::Node => match seat {
                Some(name) => state
                    .store
                    .seat_by_name(self.org.id, name)
                    .await?
                    .ok_or_else(|| ApiError::not_found(format!("no seat {name:?}"))),
                None if self.is_user() => Ok(self.mail_seat()?.clone()),
                None => Err(ApiError::bad_request("node tokens must pass seat=<name>")),
            },
        }
    }
}
