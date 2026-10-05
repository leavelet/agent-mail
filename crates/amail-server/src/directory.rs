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

/// A free-text field of at most `max` characters (see
/// [`amail_proto::limits`]).
pub(crate) fn check_text(field: &str, v: Option<&str>, max: usize) -> ApiResult<()> {
    match v {
        Some(s) => p::limits::check_chars(field, s, max).map_err(ApiError::bad_request),
        None => Ok(()),
    }
}

fn check_seat_text(
    display_name: Option<&str>,
    role: Option<&str>,
    harness: Option<&str>,
    model: Option<&str>,
    description: Option<&str>,
) -> ApiResult<()> {
    use p::limits::*;
    check_text("display_name", display_name, MAX_DISPLAY_NAME_CHARS)?;
    check_text("role", role, MAX_ROLE_CHARS)?;
    check_text("harness", harness, MAX_HARNESS_CHARS)?;
    check_text("model", model, MAX_MODEL_CHARS)?;
    check_description(description)
}

/// A description (of an org, seat, team or list), if given.
pub(crate) fn check_description(d: Option<&str>) -> ApiResult<()> {
    match d {
        Some(d) => p::check_description(d).map_err(ApiError::bad_request),
        None => Ok(()),
    }
}

fn inner(v: &Option<Option<String>>) -> Option<&str> {
    v.as_ref().and_then(|v| v.as_deref())
}

fn check_name(name: &str) -> ApiResult<()> {
    if valid_name(name) {
        Ok(())
    } else {
        Err(ApiError::bad_request(format!(
            "invalid name {name:?}: must match [a-z0-9][a-z0-9._-]{{0,62}}"
        )))
    }
}

pub async fn get_org(State(st): State<AppState>, auth: Auth) -> ApiResult<Json<p::OrgInfo>> {
    auth.require_directory()?;
    let mut org = render::org(&auth.org, st.config.limits.org_storage_bytes);
    // Webhook URLs often carry credentials (in the path or as user:password):
    // only the user sees them, not the agents.
    if !auth.is_user() {
        org.webhook_url = None;
        org.notice = None;
    }
    Ok(Json(org))
}

pub async fn patch_org(
    State(st): State<AppState>,
    auth: Auth,
    Json(patch): Json<p::OrgPatch>,
) -> ApiResult<Json<p::OrgInfo>> {
    auth.require_user()?;
    let public = st.config.public;
    let check = |v: Option<i64>, min: i64, name: &str| {
        let max = crate::config::ORG_SETTING_MAX
            .iter()
            .find(|m| m.name == name && (public || !m.public_only))
            .map(|m| m.max);
        match v {
            Some(x) if x < min => Err(ApiError::bad_request(format!(
                "{name} must be at least {min}"
            ))),
            Some(x) if max.is_some_and(|m| x > m) => Err(ApiError::bad_request(format!(
                "{name} must be at most {}",
                max.unwrap_or_default()
            ))),
            _ => Ok(()),
        }
    };
    check(patch.send_rate_per_min, 0, "send_rate_per_min")?;
    check(patch.send_burst, 1, "send_burst")?;
    check(patch.max_recipients, 1, "max_recipients")?;
    check(patch.max_thread_depth, 1, "max_thread_depth")?;
    check(patch.ack_timeout_min, 1, "ack_timeout_min")?;
    check(patch.focus_max_min, 1, "focus_max_min")?;
    check(patch.digest_interval_min, 1, "digest_interval_min")?;
    check(patch.new_threads_per_hour, 0, "new_threads_per_hour")?;
    check_description(inner(&patch.description))?;
    if let Some(Some(d)) = &patch.display_name {
        p::check_paragraph("display_name", d, p::limits::MAX_ORG_DISPLAY_NAME_CHARS)
            .map_err(ApiError::bad_request)?;
    }
    if let Some(Some(url)) = &patch.webhook_url {
        if !crate::webhook::valid_url(url) {
            return Err(ApiError::bad_request(
                "webhook_url must be an http:// or https:// URL",
            ));
        }
        // Names are checked again whenever the webhook fires.
        crate::netguard::check_literal(url, st.config.webhook_private_targets)
            .map_err(|e| ApiError::unprocessable("webhook_target", e))?;
    }
    let org = st.store.update_org(auth.org.id, &patch).await?;
    let mut changed = Vec::new();
    if org.visibility != auth.org.visibility {
        changed.push(match org.visibility {
            p::OrgVisibility::Hidden => {
                "visibility: hidden. To orgs without an active link with yours, your org now \
                 answers as a name no org has: they cannot read its card or ask for a link."
            }
            p::OrgVisibility::ListedByName => {
                "visibility: listed-by-name. Anyone signed in who knows your org's name can now \
                 read its card (name, display name, description) and ask for a link."
            }
        });
    }
    if org.outbound != auth.org.outbound {
        changed.push(match org.outbound {
            p::Outbound::ContactsOnly => {
                "outbound: contacts-only. Only you and your org's contacts for an org (the seats \
                 open to it) may now write to that org."
            }
            p::Outbound::AnySeat => "outbound: any-seat. Every seat may now write to linked orgs.",
        });
    }
    if !changed.is_empty() {
        crate::notices::tell_user(
            &st,
            org.id,
            "Org settings changed",
            &format!(
                "Your org's settings changed:\n\n{}\n",
                changed
                    .iter()
                    .map(|c| format!("- {c}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
        )
        .await;
    }
    Ok(Json(render::org(&org, st.config.limits.org_storage_bytes)))
}

// --- seats -------------------------------------------------------------------

async fn seat_out(st: &AppState, auth: &Auth, s: &amail_store::Seat) -> ApiResult<p::Seat> {
    let presence = st.presence.get(auth.org.id, &s.name).await?.map(|mut p| {
        p.node_connected = st.nodes.connected(auth.org.id) > 0;
        p
    });
    let open = crate::openings::seat_audiences(st, &auth.org)
        .await?
        .remove(&s.name)
        .unwrap_or_default();
    Ok(render::seat(s, presence, open))
}

pub async fn list_seats(State(st): State<AppState>, auth: Auth) -> ApiResult<Json<Vec<p::Seat>>> {
    auth.require_directory()?;
    let mut presence = crate::ui::presences(&st, auth.org.id).await?;
    let mut open = crate::openings::seat_audiences(&st, &auth.org).await?;
    Ok(Json(
        st.store
            .seats(auth.org.id)
            .await?
            .iter()
            .map(|s| {
                render::seat_in_list(
                    s,
                    presence.remove(&s.name),
                    open.remove(&s.name).unwrap_or_default(),
                )
            })
            .collect(),
    ))
}

pub async fn create_seat(
    State(st): State<AppState>,
    auth: Auth,
    Json(seat): Json<p::NewSeat>,
) -> ApiResult<(StatusCode, Json<p::Seat>)> {
    auth.require_user()?;
    crate::public::check_new_name(&st, &seat.name)?;
    check_seat_text(
        seat.display_name.as_deref(),
        seat.role.as_deref(),
        seat.harness.as_deref(),
        seat.model.as_deref(),
        seat.description.as_deref(),
    )?;
    if let Some(g) = &seat.group {
        check_name(g)?;
    }
    if let Some(max) = st.config.limits.seats_per_org {
        let n = st
            .store
            .seats(auth.org.id)
            .await?
            .iter()
            .filter(|s| s.kind == amail_proto::SeatKind::Agent)
            .count() as i64;
        if n >= max {
            return Err(ApiError::unprocessable(
                "seat_limit",
                format!("this server allows at most {max} agent seats per org"),
            ));
        }
    }
    let s = st.store.create_seat(auth.org.id, &seat).await?;
    st.directory_changed(auth.org.id, "seat", &s.name, "created");
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
    check_seat_text(
        inner(&patch.display_name),
        inner(&patch.role),
        inner(&patch.harness),
        inner(&patch.model),
        inner(&patch.description),
    )?;
    if let Some(Some(g)) = &patch.group {
        check_name(g)?;
    }
    // Leaving a team or stopping being its contact can close the seat.
    let (s, changes) = crate::boundary::changing(&st, &auth.org, async {
        Ok(st.store.update_seat(auth.org.id, &name, &patch).await?)
    })
    .await?;
    st.directory_changed(auth.org.id, "seat", &s.name, "updated");
    if !changes.is_empty() {
        crate::openings::tell_openings(&st, &auth.org, &format!("seat {name} changed"), &changes)
            .await;
    }
    Ok(Json(seat_out(&st, &auth, &s).await?))
}

pub async fn delete_seat(
    State(st): State<AppState>,
    auth: Auth,
    Path(name): Path<String>,
) -> ApiResult<StatusCode> {
    auth.require_user()?;
    let (_, changes) = crate::boundary::changing(&st, &auth.org, async {
        Ok(st.store.delete_seat(auth.org.id, &name).await?)
    })
    .await?;
    st.directory_changed(auth.org.id, "seat", &name, "deleted");
    if !changes.is_empty() {
        crate::openings::tell_openings(
            &st,
            &auth.org,
            &format!("seat {name} was deleted"),
            &changes,
        )
        .await;
    }
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
    let label = body.and_then(|b| b.label);
    check_text("label", label.as_deref(), p::limits::MAX_LABEL_CHARS)?;
    check_token_room(&st, auth.org.id).await?;
    let token = new_token();
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
        TokenScope::Operator => {
            return Err(ApiError::forbidden(
                "operator tokens are issued on the server's database (amail admin operator-token)",
            ));
        }
    };
    check_text("label", req.label.as_deref(), p::limits::MAX_LABEL_CHARS)?;
    check_token_room(&st, auth.org.id).await?;
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
/// Public mode: refuse a new token when the org holds the most it may.
async fn check_token_room(st: &AppState, org_id: i64) -> ApiResult<()> {
    if let Some(max) = st.config.limits.tokens_per_org
        && st.store.tokens(org_id).await?.len() as i64 >= max
    {
        return Err(ApiError::unprocessable(
            "token_limit",
            format!("this server allows at most {max} live tokens per org; revoke some first"),
        ));
    }
    Ok(())
}

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
        st.token_revoked(auth.org.id, id);
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found(format!("no token {id}")))
    }
}

// --- lists -------------------------------------------------------------------

/// The org's lists with their member counts (up to 500 lists of 1,000
/// members in public mode): in one of the org's light query slots.
async fn lists_of(st: &AppState, auth: &Auth) -> ApiResult<Vec<amail_store::List>> {
    let (store, org_id) = (st.store.clone(), auth.org.id);
    crate::mail::limited(st, auth, crate::mail::Pool::Light, async move {
        Ok(store.lists(org_id).await?)
    })
    .await
}

pub async fn list_lists(State(st): State<AppState>, auth: Auth) -> ApiResult<Json<Vec<p::List>>> {
    auth.require_directory()?;
    Ok(Json(
        lists_of(&st, &auth)
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
    crate::public::check_new_name(&st, &list.name)?;
    check_description(list.description.as_deref())?;
    if let Some(max) = st.config.limits.lists_per_org
        && lists_of(&st, &auth).await?.len() as i64 >= max
    {
        return Err(ApiError::unprocessable(
            "list_limit",
            format!("this server allows at most {max} lists per org"),
        ));
    }
    let l = st.store.create_list(auth.org.id, &list).await?;
    st.directory_changed(auth.org.id, "list", &l.name, "created");
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
    let (store, org_id) = (st.store.clone(), auth.org.id);
    let (l, members) = crate::mail::limited(&st, &auth, crate::mail::Pool::Light, async move {
        let l = store
            .list_by_name(org_id, &name)
            .await?
            .ok_or_else(|| ApiError::not_found(format!("no list {name:?}")))?;
        let members = store.list_members(l.id).await?;
        Ok((l, members))
    })
    .await?;
    let out = members
        .into_iter()
        .map(|m| p::ListMember {
            address: m.address,
            mode: m.mode,
        })
        .collect();
    Ok(Json(render::list(&l, Some(out))))
}

pub async fn delete_list(
    State(st): State<AppState>,
    auth: Auth,
    Path(name): Path<String>,
) -> ApiResult<StatusCode> {
    auth.require_user()?;
    st.store.delete_list(auth.org.id, &name).await?;
    st.directory_changed(auth.org.id, "list", &name, "deleted");
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
        Some(_) => {
            return Err(ApiError::unprocessable(
                "local_only",
                "a list holds seats of its own org only (mail to other orgs names their seats)",
            ));
        }
    };
    let member = Member {
        address: a.to_string(),
        seat_id,
        mode,
    };
    let max = amail_proto::limits::MAX_LIST_MEMBERS;
    if l.member_count >= max
        && !st
            .store
            .list_members(l.id)
            .await?
            .iter()
            .any(|m| m.address == member.address)
    {
        return Err(ApiError::unprocessable(
            "too_many_members",
            format!("a list has at most {max} members"),
        ));
    }
    st.store.put_member(auth.org.id, l.id, &member).await?;
    st.directory_changed(auth.org.id, "list", &l.name, "updated");
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
        st.directory_changed(auth.org.id, "list", &l.name, "updated");
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
    let mut out = Vec::new();
    for l in st.store.links(auth.org.id).await? {
        let mut v = render::link(&l);
        if l.state == p::LinkState::Active
            && let Some(peer) = st.store.org_by_id(l.peer_org_id).await?
        {
            v.primary = crate::boundary::org_primary(&st, &auth.org, &peer).await?;
        }
        out.push(v);
    }
    Ok(Json(out))
}

/// `PATCH /v1/links/{org}` (user): set the org's primary contact for a
/// linked org, or a team's (`team`). The seat must be a contact for that
/// org (open to it).
pub async fn patch_link(
    State(st): State<AppState>,
    auth: Auth,
    Path(org): Path<String>,
    Json(req): Json<p::LinkPatch>,
) -> ApiResult<Json<p::Link>> {
    auth.require_user()?;
    let links = st.store.links(auth.org.id).await?;
    let l = links
        .iter()
        .find(|l| l.peer_name == org && l.peer_org_id != 0 && l.state == p::LinkState::Active)
        .ok_or_else(|| ApiError::not_found(format!("no active link with {org:?}")))?;
    let peer = st
        .store
        .org_by_id(l.peer_org_id)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("no active link with {org:?}")))?;
    let team = match &req.team {
        None => None,
        Some(t) => Some(
            st.store
                .teams(auth.org.id)
                .await?
                .into_iter()
                .find(|x| &x.name == t)
                .ok_or_else(|| ApiError::not_found(format!("no team {t:?}")))?,
        ),
    };
    let seat = match &req.primary {
        None => return Err(ApiError::bad_request("give primary (a seat name, or null)")),
        Some(None) => None,
        Some(Some(name)) => {
            let contacts = crate::boundary::shown_by(&st, &auth.org)
                .await?
                .remove(&peer.name)
                .unwrap_or_default();
            let c = contacts.seats.get(name).ok_or_else(|| {
                ApiError::unprocessable(
                    "not_a_contact",
                    format!(
                        "{name:?} is not open to {org}: open it first (amail open {name} --to {org})"
                    ),
                )
            })?;
            Some(c.id)
        }
    };
    st.store
        .set_primary(auth.org.id, peer.id, team.as_ref().map(|t| t.id), seat)
        .await?;
    let mut v = render::link(l);
    v.primary = crate::boundary::org_primary(&st, &auth.org, &peer).await?;
    crate::notices::tell_user(
        &st,
        auth.org.id,
        &format!("Primary contact for {org} changed"),
        &format!(
            "{} primary contact for {org} is now {}. It is added in Cc when a seat that is not a contact writes to {org}.\n",
            match &team {
                Some(t) => format!("Team {}'s", t.name),
                None => "Your org's".to_string(),
            },
            req.primary.flatten().unwrap_or_else(|| "the default (the user seat if it is open, else the seat opened first)".into())
        ),
    )
    .await;
    Ok(Json(v))
}

/// Another org by name, as `me` may know it: a suspended org counts as
/// absent, and so does a hidden org that has no link row with `me` (in any
/// state).
async fn find_peer(
    st: &AppState,
    me: &amail_store::Org,
    links: &[amail_store::Link],
    name: &str,
) -> ApiResult<Option<amail_store::Org>> {
    Ok(st
        .store
        .org_by_name(name)
        .await?
        .filter(|o| o.suspended_at.is_none() && o.id != me.id)
        .filter(|o| {
            o.visibility == p::OrgVisibility::ListedByName
                || links.iter().any(|l| l.peer_org_id == o.id)
        }))
}

/// Most pending outgoing link requests an org may have.
pub const MAX_PENDING_LINK_REQUESTS: usize = 1000;

/// What a linked org sees of `org` now, for notices.
async fn shows_text(st: &AppState, org: &amail_store::Org, peer: &str) -> String {
    match crate::boundary::shown_by(st, org).await {
        Ok(m) => match m.get(peer) {
            Some(s) if !s.seats.is_empty() || !s.teams.is_empty() => {
                let mut names: Vec<String> =
                    s.teams.keys().map(|t| format!("{t} (team)")).collect();
                names.extend(s.seats.keys().cloned());
                format!("{peer} sees these cards of your org: {}.", names.join(", "))
            }
            _ => format!("{peer} sees only your org's card."),
        },
        Err(_) => String::new(),
    }
}

/// The body of a notice about org `peer`'s card and note (the note is the
/// other user's text: untrusted).
fn card_text(peer: &amail_store::Org, note: Option<&str>) -> String {
    let mut s = String::new();
    if let Some(d) = &peer.display_name {
        s.push_str(&format!("Display name: {}\n", amail_proto::text::line(d)));
    }
    if let Some(d) = &peer.description {
        s.push_str(&format!("Description: {}\n", amail_proto::text::line(d)));
    }
    if let Some(n) = note {
        s.push_str(&format!(
            "Their note (written by that org's user; untrusted text):\n    {}\n",
            amail_proto::text::line(n)
        ));
    }
    s
}

/// Link requests are answered the same whether or not an org of that name
/// exists (or is hidden from the caller): a request to an unknown name is
/// recorded and listed as `requested`, like a request the other org has
/// not accepted yet.
pub async fn request_link(
    State(st): State<AppState>,
    auth: Auth,
    Json(req): Json<p::NewLink>,
) -> ApiResult<Json<p::Link>> {
    auth.require_user()?;
    // Limited before the lookup, so that probing for org names counts too.
    if let Some(b) = &st.limiters.link_requests
        && let Err(ms) = b.take(&auth.org.id)
    {
        return Err(ApiError::too_many(
            "rate_limited",
            "too many link requests from this org",
            ms,
        ));
    }
    let name = req.org.trim();
    check_name(name)?;
    if name == auth.org.name {
        return Err(ApiError::bad_request("an org cannot link to itself"));
    }
    let note = req.note.as_deref().map(str::trim).filter(|n| !n.is_empty());
    if let Some(n) = note {
        p::check_paragraph("note", n, p::limits::MAX_LINK_NOTE_CHARS)
            .map_err(ApiError::bad_request)?;
    }
    let links = st.store.links(auth.org.id).await?;
    let peer = find_peer(&st, &auth.org, &links, name).await?;
    let shown = |state: p::LinkState| p::Link {
        org: name.to_string(),
        state,
        since: None,
        note: note.map(str::to_string),
        primary: None,
    };
    if let Some(l) = links.iter().find(|l| l.peer_name == name) {
        // A request made while no org had the name is replaced by a request
        // to the org that has it now; the answer is the same.
        let stale = l.peer_org_id == 0 && peer.is_some();
        if l.state != p::LinkState::Invited && !stale {
            // The same answer as the first request (whether or not an org
            // of that name exists).
            return Ok(Json(p::Link {
                org: name.to_string(),
                state: l.state,
                since: None,
                note: l.note.clone(),
                primary: None,
            }));
        }
    } else if links
        .iter()
        .filter(|l| l.state == p::LinkState::Requested)
        .count()
        >= MAX_PENDING_LINK_REQUESTS
    {
        return Err(ApiError::unprocessable(
            "too_many_link_requests",
            format!(
                "this org has {MAX_PENDING_LINK_REQUESTS} pending link requests; withdraw some first"
            ),
        ));
    }
    let state = match &peer {
        Some(peer) => st.store.request_link(auth.org.id, peer.id, note).await?,
        None => {
            st.store
                .request_unknown_link(auth.org.id, name, note)
                .await?;
            p::LinkState::Requested
        }
    };
    if let Some(peer) = &peer {
        if state == p::LinkState::Active {
            link_active_notices(&st, &auth.org, peer).await;
        } else {
            crate::notices::tell_user(
                &st,
                peer.id,
                &format!("Org {} asks to link with your org", auth.org.name),
                &format!(
                    "The user of org {me} asks to link your orgs.\n\n{card}\nA link opens nothing by itself: each org then sees the other's card and only the seats and teams the other opens to it. Accept with `amail link accept {me}` (or in the web interface, Other orgs); ignore the request to decline it.\n",
                    me = auth.org.name,
                    card = card_text(&auth.org, note),
                ),
            )
            .await;
        }
    }
    Ok(Json(shown(state)))
}

/// Tell both users that their orgs are now linked, and what each shows the
/// other.
async fn link_active_notices(st: &AppState, a: &amail_store::Org, b: &amail_store::Org) {
    for (me, other) in [(a, b), (b, a)] {
        let shows = shows_text(st, me, &other.name).await;
        crate::notices::tell_user(
            st,
            me.id,
            &format!("Your org is now linked with {}", other.name),
            &format!(
                "Your org and {o} are now linked.\n\n{card}\n{shows}\nSee what {o} opens to you with `amail org show {o}`; open seats or teams to it with `amail open <seat|team> --to {o}`.\n",
                o = other.name,
                card = card_text(other, None),
            ),
        )
        .await;
    }
}

pub async fn accept_link(
    State(st): State<AppState>,
    auth: Auth,
    Path(org): Path<String>,
) -> ApiResult<Json<p::Link>> {
    auth.require_user()?;
    let links = st.store.links(auth.org.id).await?;
    let missing = || ApiError::not_found("no pending link request from that org");
    let peer = find_peer(&st, &auth.org, &links, &org)
        .await?
        .ok_or_else(missing)?;
    let was_active = links
        .iter()
        .any(|l| l.peer_org_id == peer.id && l.state == p::LinkState::Active);
    st.store.accept_link(auth.org.id, peer.id).await?;
    if !was_active {
        link_active_notices(&st, &auth.org, &peer).await;
    }
    Ok(Json(p::Link {
        org: peer.name,
        state: p::LinkState::Active,
        since: Some(render::ts(amail_store::now_ms())),
        note: None,
        primary: None,
    }))
}

pub async fn revoke_link(
    State(st): State<AppState>,
    auth: Auth,
    Path(org): Path<String>,
) -> ApiResult<StatusCode> {
    auth.require_user()?;
    let links = st.store.links(auth.org.id).await?;
    let peer = find_peer(&st, &auth.org, &links, &org).await?;
    let removed = match &peer {
        Some(peer) => st.store.revoke_link(auth.org.id, peer.id).await?,
        None => false,
    };
    // A request made to the name while no (active) org had it.
    let unknown = st.store.revoke_unknown_link(auth.org.id, &org).await?;
    if let (true, Some(peer)) = (removed, &peer) {
        for (me, other) in [(&auth.org, peer), (peer, &auth.org)] {
            crate::notices::tell_user(
                &st,
                me.id,
                &format!("The link with {} ended", other.name),
                &format!(
                    "The link between your org and {o} ended (or a request between them was withdrawn). No mail can go between the two orgs now, in new or existing threads; stored mail stays where it is.\n",
                    o = other.name
                ),
            )
            .await;
        }
    }
    if removed || unknown {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found(format!("no link with {org:?}")))
    }
}
