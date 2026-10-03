//! Directory and administration endpoints: seats, tokens, lists, links, org
//! settings. Reads need a user or seat token; writes need a user token.

use crate::AppState;
use crate::auth::{Auth, hash_token, new_token};
use crate::error::{ApiError, ApiResult, Json, OptJson, Path};
use crate::render;
use amail_proto::{self as p, Address, DeliveryMode, TokenScope, valid_name};
use amail_store::{Member, NewToken};
use axum::extract::State;
use axum::http::StatusCode;

fn check_name(name: &str) -> ApiResult<()> {
    if valid_name(name) {
        Ok(())
    } else {
        Err(ApiError::bad_request(format!(
            "invalid name {name:?}: must match [a-z0-9][a-z0-9._-]{{0,62}}"
        )))
    }
}

pub async fn whoami(auth: Auth) -> Json<p::WhoAmI> {
    Json(p::WhoAmI {
        org: auth.org.name.clone(),
        scope: auth.scope,
        seat: auth.seat.map(|s| s.name),
    })
}

pub async fn get_org(auth: Auth) -> ApiResult<Json<p::OrgInfo>> {
    auth.require_directory()?;
    Ok(Json(render::org(&auth.org)))
}

pub async fn patch_org(
    State(st): State<AppState>,
    auth: Auth,
    Json(patch): Json<p::OrgPatch>,
) -> ApiResult<Json<p::OrgInfo>> {
    auth.require_user()?;
    let check = |v: Option<i64>, min: i64, name: &str| match v {
        Some(x) if x < min => Err(ApiError::bad_request(format!(
            "{name} must be at least {min}"
        ))),
        _ => Ok(()),
    };
    check(patch.send_rate_per_min, 0, "send_rate_per_min")?;
    check(patch.send_burst, 1, "send_burst")?;
    check(patch.max_recipients, 1, "max_recipients")?;
    check(patch.max_thread_depth, 1, "max_thread_depth")?;
    check(patch.ack_timeout_min, 1, "ack_timeout_min")?;
    check(patch.focus_max_min, 1, "focus_max_min")?;
    check(patch.digest_interval_min, 1, "digest_interval_min")?;
    if let Some(Some(url)) = &patch.webhook_url
        && !crate::webhook::valid_url(url)
    {
        return Err(ApiError::bad_request(
            "webhook_url must be an http:// or https:// URL",
        ));
    }
    Ok(Json(render::org(
        &st.store.update_org(auth.org.id, &patch).await?,
    )))
}

// --- seats -------------------------------------------------------------------

async fn seat_out(st: &AppState, auth: &Auth, s: &amail_store::Seat) -> ApiResult<p::Seat> {
    let presence = st.presence.get(auth.org.id, &s.name).await?.map(|mut p| {
        p.node_connected = st.nodes.connected(auth.org.id) > 0;
        p
    });
    Ok(render::seat(s, presence))
}

pub async fn list_seats(State(st): State<AppState>, auth: Auth) -> ApiResult<Json<Vec<p::Seat>>> {
    auth.require_directory()?;
    let mut presence = crate::ui::presences(&st, auth.org.id).await?;
    Ok(Json(
        st.store
            .seats(auth.org.id)
            .await?
            .iter()
            .map(|s| render::seat(s, presence.remove(&s.name)))
            .collect(),
    ))
}

pub async fn create_seat(
    State(st): State<AppState>,
    auth: Auth,
    Json(seat): Json<p::NewSeat>,
) -> ApiResult<(StatusCode, Json<p::Seat>)> {
    auth.require_user()?;
    check_name(&seat.name)?;
    if let Some(g) = &seat.group {
        check_name(g)?;
    }
    let s = st.store.create_seat(auth.org.id, &seat).await?;
    Ok((StatusCode::CREATED, Json(seat_out(&st, &auth, &s).await?)))
}

pub async fn get_seat(
    State(st): State<AppState>,
    auth: Auth,
    Path(name): Path<String>,
) -> ApiResult<Json<p::Seat>> {
    auth.require_directory()?;
    let s = st
        .store
        .seat_by_name(auth.org.id, &name)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("no seat {name:?}")))?;
    Ok(Json(seat_out(&st, &auth, &s).await?))
}

pub async fn patch_seat(
    State(st): State<AppState>,
    auth: Auth,
    Path(name): Path<String>,
    Json(patch): Json<p::SeatPatch>,
) -> ApiResult<Json<p::Seat>> {
    auth.require_user()?;
    if let Some(Some(g)) = &patch.group {
        check_name(g)?;
    }
    let s = st.store.update_seat(auth.org.id, &name, &patch).await?;
    Ok(Json(seat_out(&st, &auth, &s).await?))
}

pub async fn delete_seat(
    State(st): State<AppState>,
    auth: Auth,
    Path(name): Path<String>,
) -> ApiResult<StatusCode> {
    auth.require_user()?;
    st.store.delete_seat(auth.org.id, &name).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn seat_token(
    State(st): State<AppState>,
    auth: Auth,
    Path(name): Path<String>,
    OptJson(body): OptJson<p::TokenRequest>,
) -> ApiResult<(StatusCode, Json<p::TokenResponse>)> {
    auth.require_user()?;
    let seat = st
        .store
        .seat_by_name(auth.org.id, &name)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("no seat {name:?}")))?;
    let token = new_token();
    let label = body.and_then(|b| b.label);
    st.store
        .create_token(&NewToken {
            org_id: auth.org.id,
            scope: TokenScope::Seat,
            seat_id: Some(seat.id),
            hash: hash_token(&token),
            label,
        })
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(p::TokenResponse {
            token,
            scope: TokenScope::Seat,
            seat: Some(seat.name),
        }),
    ))
}

/// `POST /v1/tokens`: create a `node` or an additional `user` token.
pub async fn org_token(
    State(st): State<AppState>,
    auth: Auth,
    Json(req): Json<p::TokenRequest>,
) -> ApiResult<(StatusCode, Json<p::TokenResponse>)> {
    auth.require_user()?;
    let scope = req.scope.unwrap_or(TokenScope::Node);
    let seat = match scope {
        TokenScope::Node => None,
        TokenScope::User => auth.seat.clone(),
        TokenScope::Seat => {
            return Err(ApiError::bad_request(
                "use POST /v1/seats/{name}/tokens for seat tokens",
            ));
        }
    };
    let token = new_token();
    st.store
        .create_token(&NewToken {
            org_id: auth.org.id,
            scope,
            seat_id: seat.as_ref().map(|s| s.id),
            hash: hash_token(&token),
            label: req.label,
        })
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(p::TokenResponse {
            token,
            scope,
            seat: seat.map(|s| s.name),
        }),
    ))
}

/// `GET /v1/tokens`: live tokens of the org (labels and scopes only).
pub async fn list_tokens(
    State(st): State<AppState>,
    auth: Auth,
) -> ApiResult<Json<Vec<p::TokenInfo>>> {
    auth.require_user()?;
    Ok(Json(
        st.store
            .tokens(auth.org.id)
            .await?
            .into_iter()
            .map(|t| p::TokenInfo {
                id: t.id,
                scope: t.scope,
                seat: t.seat_name,
                label: t.label,
                created_at: render::ts(t.created_at),
            })
            .collect(),
    ))
}

/// `DELETE /v1/tokens/{id}`. The org's last user token cannot be revoked.
pub async fn revoke_token(
    State(st): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    auth.require_user()?;
    let tokens = st.store.tokens(auth.org.id).await?;
    let target = tokens
        .iter()
        .find(|t| t.id == id)
        .ok_or_else(|| ApiError::not_found(format!("no token {id}")))?;
    if target.scope == TokenScope::User
        && tokens
            .iter()
            .filter(|t| t.scope == TokenScope::User)
            .count()
            == 1
    {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "last_user_token",
            "cannot revoke the org's last user token",
        ));
    }
    if st.store.revoke_token(auth.org.id, id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found(format!("no token {id}")))
    }
}

// --- lists -------------------------------------------------------------------

pub async fn list_lists(State(st): State<AppState>, auth: Auth) -> ApiResult<Json<Vec<p::List>>> {
    auth.require_directory()?;
    Ok(Json(
        st.store
            .lists(auth.org.id)
            .await?
            .iter()
            .map(|l| render::list(l, None))
            .collect(),
    ))
}

pub async fn create_list(
    State(st): State<AppState>,
    auth: Auth,
    Json(list): Json<p::NewList>,
) -> ApiResult<(StatusCode, Json<p::List>)> {
    auth.require_user()?;
    check_name(&list.name)?;
    let l = st.store.create_list(auth.org.id, &list).await?;
    Ok((
        StatusCode::CREATED,
        Json(render::list(&l, Some(Vec::new()))),
    ))
}

pub async fn get_list(
    State(st): State<AppState>,
    auth: Auth,
    Path(name): Path<String>,
) -> ApiResult<Json<p::List>> {
    auth.require_directory()?;
    let l = st
        .store
        .list_by_name(auth.org.id, &name)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("no list {name:?}")))?;
    let members = st
        .store
        .list_members(l.id)
        .await?
        .into_iter()
        .map(|m| p::ListMember {
            address: m.address,
            mode: m.mode,
        })
        .collect();
    Ok(Json(render::list(&l, Some(members))))
}

pub async fn delete_list(
    State(st): State<AppState>,
    auth: Auth,
    Path(name): Path<String>,
) -> ApiResult<StatusCode> {
    auth.require_user()?;
    st.store.delete_list(auth.org.id, &name).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Normalize a member address: local seats are stored as `name`, external
/// ones as `name@org`.
fn member_address(auth: &Auth, raw: &str) -> ApiResult<Address> {
    let mut a: Address = raw
        .parse()
        .map_err(|e| ApiError::bad_request(format!("{e}")))?;
    if a.org.as_deref() == Some(auth.org.name.as_str()) {
        a.org = None;
    }
    Ok(a)
}

pub async fn put_member(
    State(st): State<AppState>,
    auth: Auth,
    Path((list, address)): Path<(String, String)>,
    OptJson(body): OptJson<p::PutMember>,
) -> ApiResult<Json<p::ListMember>> {
    auth.require_user()?;
    let l = st
        .store
        .list_by_name(auth.org.id, &list)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("no list {list:?}")))?;
    let mode = body.and_then(|b| b.mode).unwrap_or(DeliveryMode::Immediate);
    let a = member_address(&auth, &address)?;
    let seat_id = match &a.org {
        None => {
            let seat = st
                .store
                .seat_by_name(auth.org.id, &a.name)
                .await?
                .ok_or_else(|| {
                    ApiError::not_found(format!(
                        "no seat {:?} (list members must be seats)",
                        a.name
                    ))
                })?;
            Some(seat.id)
        }
        Some(org) => {
            let peer = st.store.org_by_name(org).await?;
            let linked = match &peer {
                Some(o) => st.store.link_active(auth.org.id, o.id).await?,
                None => false,
            };
            let peer = peer.filter(|_| linked).ok_or_else(|| {
                ApiError::unprocessable(
                    "not_linked",
                    format!("org {org:?} is not linked with {:?}", auth.org.name),
                )
            })?;
            st.store
                .seat_by_name(peer.id, &a.name)
                .await?
                .ok_or_else(|| {
                    ApiError::not_found(format!("no seat {a} (list members must be seats)"))
                })?;
            None
        }
    };
    let member = Member {
        address: a.to_string(),
        seat_id,
        mode,
    };
    st.store.put_member(auth.org.id, l.id, &member).await?;
    Ok(Json(p::ListMember {
        address: member.address,
        mode,
    }))
}

pub async fn delete_member(
    State(st): State<AppState>,
    auth: Auth,
    Path((list, address)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    auth.require_user()?;
    let l = st
        .store
        .list_by_name(auth.org.id, &list)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("no list {list:?}")))?;
    let a = member_address(&auth, &address)?;
    if st.store.remove_member(l.id, &a.to_string()).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found(format!(
            "{a} is not a member of {list}"
        )))
    }
}

// --- links -------------------------------------------------------------------

pub async fn list_links(State(st): State<AppState>, auth: Auth) -> ApiResult<Json<Vec<p::Link>>> {
    auth.require_directory()?;
    Ok(Json(
        st.store
            .links(auth.org.id)
            .await?
            .into_iter()
            .map(|l| p::Link {
                org: l.peer_name,
                state: l.state,
            })
            .collect(),
    ))
}

async fn peer_org(st: &AppState, name: &str) -> ApiResult<amail_store::Org> {
    st.store
        .org_by_name(name)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("no org {name:?}")))
}

pub async fn request_link(
    State(st): State<AppState>,
    auth: Auth,
    Json(req): Json<p::NewLink>,
) -> ApiResult<Json<p::Link>> {
    auth.require_user()?;
    let peer = peer_org(&st, &req.org).await?;
    let state = st.store.request_link(auth.org.id, peer.id).await?;
    Ok(Json(p::Link {
        org: peer.name,
        state,
    }))
}

pub async fn accept_link(
    State(st): State<AppState>,
    auth: Auth,
    Path(org): Path<String>,
) -> ApiResult<Json<p::Link>> {
    auth.require_user()?;
    let peer = peer_org(&st, &org).await?;
    st.store.accept_link(auth.org.id, peer.id).await?;
    Ok(Json(p::Link {
        org: peer.name,
        state: p::LinkState::Active,
    }))
}

pub async fn revoke_link(
    State(st): State<AppState>,
    auth: Auth,
    Path(org): Path<String>,
) -> ApiResult<StatusCode> {
    auth.require_user()?;
    let peer = peer_org(&st, &org).await?;
    if st.store.revoke_link(auth.org.id, peer.id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found(format!("no link with {org:?}")))
    }
}
