//! Deleting mail: the org's user deletes mail of deleted orgs it keeps and
//! its own internal mail. Mail exchanged with orgs that still exist is not
//! deletable by either side; retention clears its bodies.

use crate::AppState;
use crate::auth::Auth;
use crate::error::{ApiError, ApiResult, Json, Path};
use crate::mail::limited;
use amail_proto::limits::MAX_DELETE_MESSAGES;
use amail_proto::{self as p};
use axum::extract::State;

fn response(d: amail_store::MailDeleted) -> p::DeleteMailResponse {
    p::DeleteMailResponse {
        deleted: d.deleted,
        kept: d.kept,
        more: d.more,
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
    let done = limited(&st, &auth, async move {
        Ok(store.delete_thread(org_id, id, MAX_DELETE_MESSAGES).await?)
    })
    .await?
    .ok_or_else(|| ApiError::not_found(format!("no thread {id}")))?;
    if done.deleted > 0 {
        tracing::info!(
            "org {}: deleted {} messages of thread {id}",
            auth.org.name,
            done.deleted
        );
    }
    Ok(Json(response(done)))
}

/// `POST /v1/archive/purge-deleted-orgs` (user tokens): delete up to
/// `MAX_DELETE_MESSAGES` messages of deleted orgs that the org keeps.
pub async fn purge_deleted_orgs(
    State(st): State<AppState>,
    auth: Auth,
) -> ApiResult<Json<p::DeleteMailResponse>> {
    auth.require_user()?;
    let store = st.store.clone();
    let org_id = auth.org.id;
    let done = limited(&st, &auth, async move {
        Ok(store
            .purge_deleted_org_mail(org_id, MAX_DELETE_MESSAGES)
            .await?)
    })
    .await?;
    if done.deleted > 0 {
        tracing::info!(
            "org {}: deleted {} messages of deleted orgs",
            auth.org.name,
            done.deleted
        );
    }
    Ok(Json(response(done)))
}
