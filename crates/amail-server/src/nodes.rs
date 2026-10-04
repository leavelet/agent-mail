//! Endpoints for host nodes: pending-notification counts, presence reports
//! and seat launch information.

use crate::AppState;
use crate::auth::{Auth, hash_token, new_token};
use crate::error::{ApiError, ApiResult, Json, Path, Query};
use amail_proto::{self as p, SeatKind, TokenScope};
use amail_store::NewToken;
use axum::extract::State;
use serde::Deserialize;
use std::collections::HashSet;

/// Label of seat tokens minted by the launch endpoint.
const LAUNCH_LABEL: &str = "launch";

/// Maximum entries in one presence report.
const MAX_PRESENCE_BATCH: usize = 5000;

fn require_node_or_user(auth: &Auth) -> ApiResult<()> {
    match auth.scope {
        TokenScope::Node | TokenScope::User => Ok(()),
        TokenScope::Seat | TokenScope::Operator => {
            Err(ApiError::forbidden("requires a node or user token"))
        }
    }
}

#[derive(Deserialize)]
pub struct PendingParams {
    seat: Option<String>,
}

/// `GET /v1/pending`: pending-notification counts per seat (node and user
/// tokens). With `seat=`, only that seat.
pub async fn pending(
    State(st): State<AppState>,
    auth: Auth,
    Query(q): Query<PendingParams>,
) -> ApiResult<Json<p::PendingResponse>> {
    require_node_or_user(&auth)?;
    let seat_id = match &q.seat {
        Some(name) => Some(auth.mailbox(&st, Some(name)).await?.id),
        None => None,
    };
    let rows = st.store.pending(auth.org.id, seat_id).await?;
    // Nodes host agent sessions only: the user seat has none (its mail is
    // read through the CLI or the web), so nodes do not see it here.
    let node = auth.scope == TokenScope::Node;
    Ok(Json(p::PendingResponse {
        seats: rows
            .into_iter()
            .filter(|r| !(node && r.user))
            .map(|r| p::PendingCounts {
                seat: r.seat_name,
                to: r.to,
                cc: r.cc,
                list: r.list,
                urgent: r.urgent,
                seq: r.last_seq,
                digest: r.digest,
                digest_since: r.digest_since.map(crate::render::ts),
                focus_until: r
                    .focus_until
                    .filter(|t| *t > amail_store::now_ms())
                    .map(crate::render::ts),
            })
            .collect(),
        digest_interval_ms: Some(auth.org.digest_interval_min.max(1) as u64 * 60_000),
    }))
}

#[derive(serde::Serialize)]
pub struct PresenceAccepted {
    accepted: usize,
}

/// `POST /v1/presence` (node tokens): report seat session states.
pub async fn presence(
    State(st): State<AppState>,
    auth: Auth,
    Json(report): Json<p::PresenceReport>,
) -> ApiResult<Json<PresenceAccepted>> {
    if auth.scope != TokenScope::Node {
        return Err(ApiError::forbidden("requires a node token"));
    }
    if report.seats.len() > MAX_PRESENCE_BATCH {
        return Err(ApiError::bad_request(format!(
            "at most {MAX_PRESENCE_BATCH} seats per report"
        )));
    }
    if let Some(n) = &report.node
        && n.len() > 200
    {
        return Err(ApiError::bad_request("node label is too long"));
    }
    let live: HashSet<String> = st
        .store
        .seat_seqs(auth.org.id)
        .await?
        .into_iter()
        .map(|(_, name, _)| name)
        .collect();
    let seats: Vec<p::PresenceUpdate> = report
        .seats
        .into_iter()
        .filter(|u| live.contains(&u.seat))
        .collect();
    let changed = st
        .presence
        .update(auth.org.id, report.node.as_deref(), &seats)
        .await?;
    st.presence_changed(auth.org.id, changed);
    Ok(Json(PresenceAccepted {
        accepted: seats.len(),
    }))
}

/// `GET /v1/seats/{name}/launch` (node and user tokens): what a node needs to
/// start a session for an agent seat, with a new seat token.
pub async fn launch(
    State(st): State<AppState>,
    auth: Auth,
    Path(name): Path<String>,
) -> ApiResult<Json<p::LaunchInfo>> {
    require_node_or_user(&auth)?;
    let seats = st.store.seats(auth.org.id).await?;
    let seat = seats
        .iter()
        .find(|s| s.name == name)
        .ok_or_else(|| ApiError::not_found(format!("no seat {name:?}")))?;
    if seat.kind != SeatKind::Agent {
        return Err(ApiError::unprocessable(
            "not_an_agent",
            "only agent seats can be launched",
        ));
    }
    let user_seat = seats
        .iter()
        .find(|s| s.kind == SeatKind::User)
        .map(|s| s.name.clone())
        .unwrap_or_default();
    // One live launch token per seat: a new launch supersedes the last.
    for t in st.store.tokens(auth.org.id).await? {
        if t.scope == TokenScope::Seat
            && t.seat_name.as_deref() == Some(seat.name.as_str())
            && t.label.as_deref() == Some(LAUNCH_LABEL)
        {
            st.store.revoke_token(auth.org.id, t.id).await?;
        }
    }
    let token = new_token();
    st.store
        .create_token(&NewToken {
            org_id: auth.org.id,
            scope: TokenScope::Seat,
            seat_id: Some(seat.id),
            hash: hash_token(&token),
            label: Some(LAUNCH_LABEL.into()),
        })
        .await?;
    Ok(Json(p::LaunchInfo {
        org: auth.org.name.clone(),
        seat: seat.name.clone(),
        harness: seat.harness.clone(),
        model: seat.model.clone(),
        role: seat.role.clone(),
        display_name: seat.display_name.clone(),
        parent: seat.parent_name.clone(),
        permission_mode: seat.permission_mode,
        user_seat,
        token,
    }))
}
