//! Public mode and data lifecycle: server information, registration with
//! invite codes, token rotation, deleting one's own org, the retention of
//! message bodies, and ending the event streams of suspended or deleted
//! orgs.

use crate::AppState;
use crate::auth::{Auth, Operator, hash_token, new_token};
use crate::config::{RESERVED_ORG_NAMES, RESERVED_SEAT_NAMES};
use crate::error::{ApiError, ApiResult, Json, OptJson};
use crate::peer::Peer;
use amail_proto::{self as p, TokenScope, valid_name};
use amail_store::{InviteError, NewToken};
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use std::collections::HashSet;

/// A new invite code (`inv_` + 24 hex characters, 96 random bits).
pub fn new_invite() -> String {
    let bytes: [u8; 12] = rand::random();
    format!("inv_{}", hex::encode(bytes))
}

/// `GET /v1/info` (no authentication).
pub async fn info(State(st): State<AppState>) -> Json<p::ServerInfo> {
    Json(p::ServerInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        public: st.config.public,
        registration: st.config.registration,
        retention_days: st.config.retention_days,
    })
}

/// `GET /v1/whoami`: the caller's org, scope and seat; for an operator,
/// scope `operator` and no org.
pub async fn whoami(
    auth: Result<Auth, ApiError>,
    op: Result<Operator, ApiError>,
) -> ApiResult<Json<p::WhoAmI>> {
    match (auth, op) {
        (Ok(a), _) => Ok(Json(p::WhoAmI {
            org: a.org.name.clone(),
            scope: a.scope,
            seat: a.seat.map(|s| s.name),
        })),
        (Err(_), Ok(_)) => Ok(Json(p::WhoAmI {
            org: String::new(),
            scope: TokenScope::Operator,
            seat: None,
        })),
        (Err(e), Err(_)) => Err(e),
    }
}

/// A name for a new seat or list: valid, and in public mode not reserved.
pub fn check_new_name(st: &AppState, name: &str) -> ApiResult<()> {
    if !valid_name(name) {
        return Err(ApiError::bad_request(format!(
            "invalid name {name:?}: must match [a-z0-9][a-z0-9._-]{{0,62}}"
        )));
    }
    if st.config.public && RESERVED_SEAT_NAMES.contains(&name) {
        return Err(ApiError::unprocessable(
            "reserved_name",
            format!("the name {name:?} is reserved"),
        ));
    }
    Ok(())
}

/// Take a registration attempt from the client's address. An IPv6
/// client's /48 network is checked first, and a request that it refuses
/// adds no entry for the client's /64; a /64 out of attempts does not use up
/// its /48's.
fn registration_slot(st: &AppState, peer: &Peer) -> ApiResult<()> {
    let refused = |ms| {
        ApiError::too_many(
            "rate_limited",
            "too many registration attempts from this address",
            ms,
        )
    };
    let Some(b) = &st.limiters.registrations else {
        return Ok(());
    };
    let key = peer.key();
    b.check(&key).map_err(refused)?;
    if let (Some(nets), Some(net)) = (&st.limiters.registration_nets, peer.net48_key()) {
        nets.take(&net).map_err(refused)?;
    }
    b.take(&key).map_err(refused)
}

/// `POST /v1/register` (public mode): use an invite code to create an org
/// with its user seat; answers the user token, once.
pub async fn register(
    State(st): State<AppState>,
    peer: Peer,
    Json(req): Json<p::RegisterRequest>,
) -> ApiResult<(StatusCode, Json<p::RegisterResponse>)> {
    if !st.config.registration {
        return Err(ApiError::not_found(
            "registration is not open on this server",
        ));
    }
    registration_slot(&st, &peer)?;
    let org = req.org.trim();
    if !valid_name(org) {
        return Err(ApiError::unprocessable(
            "invalid_name",
            format!(
                "invalid org name {org:?}: use 1-63 of a-z, 0-9, '.', '_', '-', starting with a letter or digit"
            ),
        ));
    }
    if RESERVED_ORG_NAMES.contains(&org) {
        return Err(ApiError::unprocessable(
            "reserved_name",
            format!("the org name {org:?} is reserved"),
        ));
    }
    let user = req.user_name.as_deref().map(str::trim).unwrap_or("owner");
    if !valid_name(user) {
        return Err(ApiError::unprocessable(
            "invalid_name",
            format!("invalid user seat name {user:?}"),
        ));
    }
    if user != "owner" && RESERVED_SEAT_NAMES.contains(&user) {
        return Err(ApiError::unprocessable(
            "reserved_name",
            format!("the seat name {user:?} is reserved"),
        ));
    }
    let invite = req.invite.trim();
    let invalid = || {
        ApiError::new(
            StatusCode::FORBIDDEN,
            "invalid_invite",
            "this invite code is not valid (unknown, already used, revoked or expired)",
        )
    };
    if invite.is_empty() || invite.len() > 200 {
        return Err(invalid());
    }
    let token = new_token();
    match st
        .store
        .register(&hash_token(invite), org, user, &hash_token(&token))
        .await?
    {
        Ok((o, seat)) => {
            tracing::info!("registered org {}", o.name);
            Ok((
                StatusCode::CREATED,
                Json(p::RegisterResponse {
                    org: o.name,
                    user_seat: seat.name,
                    token,
                }),
            ))
        }
        Err(InviteError::Invalid) => Err(invalid()),
        Err(InviteError::OrgExists) => Err(ApiError::new(
            StatusCode::CONFLICT,
            "org_exists",
            format!("the org name {org:?} is taken"),
        )),
    }
}

/// `POST /v1/tokens/rotate` (user tokens): issue a new user token and
/// revoke the one making the request. Every browser session made with the
/// old token ends, and its open streams close; a browser that rotated gets
/// a new session (cookie) for the new token. The new token is answered
/// once.
pub async fn rotate_token(
    State(st): State<AppState>,
    auth: Auth,
    peer: Peer,
    headers: axum::http::HeaderMap,
    OptJson(body): OptJson<p::TokenRequest>,
) -> ApiResult<Response> {
    auth.require_user()?;
    let label = body.and_then(|b| b.label);
    crate::directory::check_text("label", label.as_deref(), p::limits::MAX_LABEL_CHARS)?;
    let token = new_token();
    let new_id = st
        .store
        .rotate_token(
            auth.org.id,
            auth.token_id,
            &NewToken {
                org_id: auth.org.id,
                scope: TokenScope::User,
                seat_id: auth.seat.as_ref().map(|s| s.id),
                hash: hash_token(&token),
                label: label.or_else(|| Some("rotated".into())),
            },
        )
        .await?;
    st.token_revoked(auth.org.id, auth.token_id);
    let mut resp = (
        StatusCode::CREATED,
        Json(p::TokenResponse {
            token,
            scope: TokenScope::User,
            seat: auth.seat.map(|s| s.name),
        }),
    )
        .into_response();
    if auth.session.is_some() {
        let secret = new_token();
        let max_age = crate::web::SESSION_DAYS * 86_400;
        st.store
            .create_session(
                &hash_token(&secret),
                new_id,
                amail_store::now_ms() + max_age * 1000,
            )
            .await?;
        resp.headers_mut().insert(
            header::SET_COOKIE,
            crate::web::session_cookie(&secret, max_age, crate::web::https(&st, peer, &headers)),
        );
    }
    Ok(resp)
}

/// `DELETE /v1/org` (user tokens) with `{"confirm": "<org name>"}`: delete
/// the org and all its data. The browser session ends with it.
pub async fn delete_own_org(
    State(st): State<AppState>,
    auth: Auth,
    Json(req): Json<p::DeleteOrgRequest>,
) -> ApiResult<Response> {
    auth.require_user()?;
    if req.confirm.trim() != auth.org.name {
        return Err(ApiError::unprocessable(
            "confirm",
            "to delete the org, confirm with its name",
        ));
    }
    delete_org(&st, &auth.org).await?;
    tracing::info!("org {} deleted by its user", auth.org.name);
    let mut resp = StatusCode::NO_CONTENT.into_response();
    resp.headers_mut().insert(
        header::SET_COOKIE,
        axum::http::HeaderValue::from_static(
            "amail_session=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0",
        ),
    );
    Ok(resp)
}

/// Delete an org's data and end its open streams.
pub async fn delete_org(st: &AppState, org: &amail_store::Org) -> ApiResult<()> {
    if !st.store.delete_org(org.id).await? {
        return Err(ApiError::not_found(format!("no org {:?}", org.name)));
    }
    close_org(st, org.id);
    Ok(())
}

/// End every event stream, command stream and conversation mirror of the
/// org on this instance (the org was suspended or deleted).
pub fn close_org(st: &AppState, org_id: i64) {
    st.bus.close_org(org_id);
    st.presence.close_org(org_id);
    st.nodes.close_org(org_id);
    st.mirror.close_org(org_id);
}

/// Background: end the streams of orgs that were suspended or deleted
/// (by another instance, or on the database with `amail admin`), and the
/// streams whose token or browser session is no longer valid.
pub async fn sweep(st: AppState) {
    loop {
        tokio::time::sleep(st.config.sweep_every).await;
        st.limiters.prune();
        if let Err(e) = st.revocations.sweep(&*st.store).await {
            tracing::warn!("checking the credentials of open streams: {e}");
        }
        let open = st.limiters.streams.orgs();
        if open.is_empty() {
            continue;
        }
        match st.store.orgs().await {
            Ok(orgs) => {
                let active: HashSet<i64> = orgs
                    .iter()
                    .filter(|o| o.suspended_at.is_none())
                    .map(|o| o.id)
                    .collect();
                for id in open.into_iter().filter(|id| !active.contains(id)) {
                    close_org(&st, id);
                }
            }
            Err(e) => tracing::warn!("looking for suspended orgs: {e}"),
        }
    }
}

/// Background (with a retention period): clear the bodies of old messages
/// and delete the old mail of deleted orgs, a thousand at a time.
pub async fn retention(st: AppState) {
    let Some(days) = st.config.retention_days else {
        return;
    };
    loop {
        let cutoff = amail_store::now_ms() - i64::from(days) * 86_400_000;
        let mut total = 0;
        loop {
            match st.store.expire_bodies(cutoff, 1000).await {
                Ok(n) => {
                    total += n;
                    if n < 1000 {
                        break;
                    }
                }
                Err(e) => {
                    tracing::warn!("retention: {e}");
                    break;
                }
            }
        }
        if total > 0 {
            tracing::info!(
                "retention: cleared the bodies of {total} messages older than {days} days"
            );
        }
        // Mail of deleted orgs older than the period goes altogether.
        let mut total = 0;
        loop {
            match st.store.expire_deleted_org_mail(cutoff, 1000).await {
                Ok(n) => {
                    total += n;
                    if n < 1000 {
                        break;
                    }
                }
                Err(e) => {
                    tracing::warn!("retention: {e}");
                    break;
                }
            }
        }
        if total > 0 {
            tracing::info!(
                "retention: deleted {total} messages of deleted orgs older than {days} days"
            );
        }
        tokio::time::sleep(st.config.retention_every).await;
    }
}
