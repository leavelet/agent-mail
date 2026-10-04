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
use crate::peer::Peer;
use amail_proto::{SeatKind, TokenScope};
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
    /// The hash of the browser session the request was made with, if it
    /// was authenticated by the session cookie.
    pub session: Option<String>,
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
/// custom header, and `Origin` (when sent) naming this host. `proxied`: the
/// request came through a trusted proxy, so `X-Forwarded-Host` names the
/// host too.
pub fn check_csrf(
    method: &axum::http::Method,
    headers: &axum::http::HeaderMap,
    proxied: bool,
) -> ApiResult<()> {
    let safe = matches!(*method, axum::http::Method::GET | axum::http::Method::HEAD);
    if !safe && headers.get(CSRF_HEADER).is_none() {
        return Err(ApiError::new(
            axum::http::StatusCode::FORBIDDEN,
            "csrf",
            "requests authenticated by the session cookie must send X-Amail-Csrf: 1",
        ));
    }
    if let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        let origin_host = origin.split_once("://").map(|(_, h)| h).unwrap_or(origin);
        // Behind a trusted reverse proxy the original host may arrive as
        // X-Forwarded-Host.
        let names: &[&str] = if proxied {
            &[header::HOST.as_str(), "x-forwarded-host"]
        } else {
            &[header::HOST.as_str()]
        };
        let matches = names.iter().any(|h| {
            headers
                .get(*h)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.eq_ignore_ascii_case(origin_host))
        });
        if !matches {
            return Err(ApiError::new(
                axum::http::StatusCode::FORBIDDEN,
                "csrf",
                "cross-origin request refused",
            ));
        }
    }
    Ok(())
}

/// How long a request over its org's request limit waits before it is
/// refused: a client that sends again as soon as it is refused thus sends
/// about one request per second per connection, and refusals cost the
/// server little.
pub const ORG_REFUSAL_DELAY: std::time::Duration = std::time::Duration::from_secs(1);

async fn org_rate_limited(ms: u64) -> ApiError {
    tokio::time::sleep(ORG_REFUSAL_DELAY).await;
    ApiError::too_many(
        "rate_limited",
        "too many requests for this org (all its tokens together)",
        ms,
    )
}

/// The failed-authentication limit
/// ([`crate::config::Limits::auth_failures_per_min`]), before a credential
/// is looked up in the database. A credential that was not valid lately
/// takes a token from the client address's bucket (given back if it turns
/// out valid), and is refused when the bucket is empty, so that at most the
/// bucket's tokens of such lookups reach the database, also when many
/// arrive at once. Pass the answer to [`auth_outcome`].
pub fn auth_gate(state: &AppState, peer: &Peer, hash: &str) -> ApiResult<AuthTicket> {
    let l = &state.limiters;
    let mut charged = false;
    if let Some(b) = &l.auth_failures
        && !l.known_credentials.contains(hash)
    {
        if let Err(ms) = b.take(&peer.key()) {
            return Err(ApiError::too_many(
                "rate_limited",
                "too many failed authentications from this address",
                ms,
            ));
        }
        charged = true;
    }
    l.auth_lookups
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Ok(AuthTicket { charged })
}

/// A credential lookup let through by [`auth_gate`].
#[derive(Debug, Clone, Copy)]
pub struct AuthTicket {
    /// It took a token from the client address's bucket.
    charged: bool,
}

/// Record the outcome of a credential lookup for the failed-authentication
/// limit.
pub fn auth_outcome(state: &AppState, peer: &Peer, hash: &str, t: AuthTicket, valid: bool) {
    let l = &state.limiters;
    let Some(b) = &l.auth_failures else {
        return;
    };
    let key = peer.key();
    if valid {
        l.known_credentials.insert(hash);
        if t.charged {
            b.refund(&key);
        }
    } else {
        l.known_credentials.remove(hash);
        if !t.charged {
            let _ = b.take(&key);
        }
    }
}

impl FromRequestParts<AppState> for Auth {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let mut session = None;
        let peer = parts.extensions.get::<Peer>().copied().unwrap_or_default();
        // The credential: a bearer token, or the session cookie (then the
        // request must pass the cross-site checks).
        let (hash, is_session) = match parts.headers.get(header::AUTHORIZATION) {
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
                (hash_token(token), false)
            }
            None => {
                let secret = cookie(&parts.headers, SESSION_COOKIE)
                    .ok_or_else(|| ApiError::unauthorized("missing bearer token"))?;
                check_csrf(&parts.method, &parts.headers, peer.proxied)?;
                (hash_token(secret), true)
            }
        };
        let ticket = auth_gate(state, &peer, &hash)?;
        // An org over its request limit is refused before the database is
        // asked, when this credential's org is known from earlier requests.
        let org_limit = state.limiters.org_requests.as_ref();
        let known_org = org_limit.and_then(|_| state.limiters.credential_orgs.get(&hash));
        if let (Some(b), Some(org)) = (org_limit, known_org)
            && let Err(ms) = b.take(&org)
        {
            return Err(org_rate_limited(ms).await);
        }
        let found = if is_session {
            state.store.session_auth(&hash).await?
        } else {
            state.store.token_auth(&hash).await?
        };
        auth_outcome(state, &peer, &hash, ticket, found.is_some());
        let auth = match found {
            Some(a) => a,
            None if is_session => {
                return Err(ApiError::unauthorized("session expired or signed out"));
            }
            None => return Err(ApiError::unauthorized("invalid or revoked token")),
        };
        if is_session {
            session = Some(hash.clone());
        }
        if let Some(b) = org_limit
            && known_org != Some(auth.org.id)
        {
            state.limiters.credential_orgs.insert(&hash, auth.org.id);
            if let Err(ms) = b.take(&auth.org.id) {
                return Err(org_rate_limited(ms).await);
            }
        }
        if auth.org.suspended_at.is_some() {
            return Err(suspended());
        }
        if let Some(b) = &state.limiters.token_requests
            && let Err(ms) = b.take(&(0, auth.token_id))
        {
            return Err(ApiError::too_many(
                "rate_limited",
                "too many requests for this token",
                ms,
            ));
        }
        Ok(Auth {
            token_id: auth.token_id,
            scope: auth.scope,
            org: auth.org,
            seat: auth.seat,
            session,
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
            TokenScope::Operator => Err(ApiError::forbidden("operator tokens have no mailbox")),
            TokenScope::User | TokenScope::Node => match seat {
                Some(name) => {
                    let seat = state
                        .store
                        .seat_by_name(self.org.id, name)
                        .await?
                        .ok_or_else(|| ApiError::not_found(format!("no seat {name:?}")))?;
                    // Nodes host agent sessions only. The user's mail is read
                    // by the user (CLI or web), never claimed by a node.
                    if self.scope == TokenScope::Node && seat.kind != SeatKind::Agent {
                        return Err(ApiError::forbidden(
                            "node tokens can only access agent seats' mailboxes",
                        ));
                    }
                    Ok(seat)
                }
                None if self.is_user() => Ok(self.mail_seat()?.clone()),
                None => Err(ApiError::bad_request("node tokens must pass seat=<name>")),
            },
        }
    }
}

/// The answer for tokens and sessions of a suspended org.
pub fn suspended() -> ApiError {
    ApiError::new(
        axum::http::StatusCode::UNAUTHORIZED,
        "org_suspended",
        "this org is suspended by the server's operator",
    )
}

/// An authenticated operator (an operator token, or a browser session made
/// with one). Operators run the server: they see counts, never mail.
#[derive(Debug, Clone, Copy)]
pub struct Operator {
    pub token_id: i64,
}

impl FromRequestParts<AppState> for Operator {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let peer = parts.extensions.get::<Peer>().copied().unwrap_or_default();
        let token_id = match parts.headers.get(header::AUTHORIZATION) {
            Some(value) => {
                let token = value
                    .to_str()
                    .ok()
                    .and_then(|v| {
                        v.strip_prefix("Bearer ")
                            .or_else(|| v.strip_prefix("bearer "))
                    })
                    .map(str::trim)
                    .ok_or_else(|| {
                        ApiError::unauthorized("expected 'Authorization: Bearer <token>'")
                    })?;
                let hash = hash_token(token);
                let ticket = auth_gate(state, &peer, &hash)?;
                let id = state.store.operator_auth(&hash).await?;
                auth_outcome(state, &peer, &hash, ticket, id.is_some());
                id
            }
            None => {
                let secret = cookie(&parts.headers, SESSION_COOKIE)
                    .ok_or_else(|| ApiError::unauthorized("missing bearer token"))?;
                check_csrf(&parts.method, &parts.headers, peer.proxied)?;
                let hash = hash_token(secret);
                let ticket = auth_gate(state, &peer, &hash)?;
                let id = state.store.operator_session_auth(&hash).await?;
                auth_outcome(state, &peer, &hash, ticket, id.is_some());
                id
            }
        };
        let token_id = token_id.ok_or_else(|| ApiError::forbidden("requires an operator token"))?;
        if let Some(b) = &state.limiters.token_requests
            && let Err(ms) = b.take(&(1, token_id))
        {
            return Err(ApiError::too_many(
                "rate_limited",
                "too many requests for this token",
                ms,
            ));
        }
        Ok(Operator { token_id })
    }
}
