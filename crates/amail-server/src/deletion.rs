//! Deleting mail: the org's user deletes mail of deleted orgs it keeps and
//! its own internal mail. Mail exchanged with orgs that still exist is not
//! deletable by either side; retention clears its bodies.

use crate::AppState;
use crate::auth::Auth;
use crate::error::{ApiError, ApiResult, Json, Path, Query};
use crate::limits::Pool;
use crate::mail::limited;
use amail_proto::limits::MAX_DELETE_MESSAGES;
use amail_proto::{self as p};
use axum::extract::State;

/// Tell the mailbox streams of the seats that lost deliveries: a pending
/// delivery that went no longer wakes the seat, and a node whose seat was
/// shown the mail in a turn finds it gone (releasing it after a failed turn
/// then releases nothing).
async fn publish(st: &AppState, org_id: i64, d: &amail_store::MailDeleted) {
    if d.seqs.is_empty() {
        return;
    }
    let names: std::collections::HashMap<i64, String> = match st.store.seats(org_id).await {
        Ok(seats) => seats.into_iter().map(|s| (s.id, s.name)).collect(),
        Err(e) => {
            tracing::warn!("org {org_id}: seats after deleting mail: {e}");
            return;
        }
    };
    let events: Vec<(String, i64)> = d
        .seqs
        .iter()
        .filter_map(|b| names.get(&b.seat_id).map(|n| (n.clone(), b.seq)))
        .collect();
    st.bus.publish(org_id, &events);
}

fn response(d: amail_store::MailDeleted) -> p::DeleteMailResponse {
    p::DeleteMailResponse {
        deleted: d.deleted,
        kept: d.kept,
        more: d.more,
        next_before_id: d.next_before_id,
    }
}

/// `DELETE /v1/threads/{id}` (user tokens): delete the messages of the
/// thread the org may delete, at most `MAX_DELETE_MESSAGES` per call; the
/// answer counts the deleted ones and those that stay. A thread the org
/// cannot see answers like a missing one.
pub async fn delete_thread(
    State(st): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
) -> ApiResult<Json<p::DeleteMailResponse>> {
    auth.require_user()?;
    if !st.store.org_sees_thread(auth.org.id, id).await? {
        return Err(ApiError::not_found(format!("no thread {id}")));
    }
    let store = st.store.clone();
    let org_id = auth.org.id;
    let done = limited(&st, &auth, Pool::Other, async move {
        Ok(store.delete_thread(org_id, id, MAX_DELETE_MESSAGES).await?)
    })
    .await?
    .ok_or_else(|| ApiError::not_found(format!("no thread {id}")))?;
    publish(&st, org_id, &done).await;
    if done.deleted > 0 {
        tracing::info!(
            "org {}: deleted {} messages of thread {id}",
            auth.org.name,
            done.deleted
        );
    }
    Ok(Json(response(done)))
}

#[derive(serde::Deserialize)]
pub struct PurgeParams {
    before_id: Option<i64>,
}

/// `POST /v1/archive/purge-deleted-orgs` (user tokens): delete up to
/// `MAX_DELETE_MESSAGES` messages of deleted orgs that the org keeps,
/// looking at its mail below `before_id` for a bounded time (as a search
/// does); `next_before_id` continues.
pub async fn purge_deleted_orgs(
    State(st): State<AppState>,
    auth: Auth,
    Query(q): Query<PurgeParams>,
) -> ApiResult<Json<p::DeleteMailResponse>> {
    auth.require_user()?;
    let store = st.store.clone();
    let org_id = auth.org.id;
    let done = limited(&st, &auth, Pool::Other, async move {
        Ok(store
            .purge_deleted_org_mail(
                org_id,
                MAX_DELETE_MESSAGES,
                q.before_id,
                Some(crate::mail::SCAN_TIME_LIMIT),
            )
            .await?)
    })
    .await?;
    publish(&st, org_id, &done).await;
    if done.deleted > 0 {
        tracing::info!(
            "org {}: deleted {} messages of deleted orgs",
            auth.org.name,
            done.deleted
        );
    }
    Ok(Json(response(done)))
}
