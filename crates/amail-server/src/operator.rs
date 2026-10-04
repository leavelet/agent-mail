//! The operator's endpoints (operator tokens only): load per org, and
//! suspending, resuming and deleting orgs. Operators see counts and names,
//! never mail or conversations.

use crate::AppState;
use crate::auth::Operator;
use crate::error::{ApiError, ApiResult, Json, Path};
use crate::render::ts;
use amail_proto as p;
use axum::extract::State;
use axum::http::StatusCode;

async fn org(st: &AppState, name: &str) -> ApiResult<amail_store::Org> {
    st.store
        .org_by_name(name)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("no org {name:?}")))
}

/// `GET /v1/operator/stats`.
pub async fn stats(
    State(st): State<AppState>,
    _op: Operator,
) -> ApiResult<Json<p::OperatorStatsResponse>> {
    let s = st.store.operator_stats().await?;
    let mut out = p::OperatorStatsResponse {
        mail_last_5min: s.messages_5min,
        ..Default::default()
    };
    for o in s.orgs {
        let nodes = st.nodes.connected(o.id) as i64;
        out.orgs += 1;
        out.suspended += i64::from(o.suspended_at.is_some());
        out.seats += o.seats;
        out.nodes_online += nodes;
        out.mail_last_hour += o.messages_hour;
        out.list.push(p::OrgStatInfo {
            name: o.name,
            created_at: ts(o.created_at),
            suspended_at: o.suspended_at.map(ts),
            seats: o.seats,
            nodes,
            messages: o.messages,
            messages_hour: o.messages_hour,
            messages_day: o.messages_day,
            last_activity: o.last_activity.map(ts),
            stored_bytes: o.stored_bytes,
        });
    }
    Ok(Json(out))
}

/// `POST /v1/operator/orgs/{name}/suspend`: the org's tokens and sessions
/// stop working and its open streams end; its data is kept.
pub async fn suspend(
    State(st): State<AppState>,
    _op: Operator,
    Path(name): Path<String>,
) -> ApiResult<StatusCode> {
    let o = org(&st, &name).await?;
    st.store
        .set_org_suspended(o.id, Some(amail_store::now_ms()))
        .await?;
    crate::public::close_org(&st, o.id);
    tracing::info!("org {name} suspended by the operator");
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /v1/operator/orgs/{name}/resume`.
pub async fn resume(
    State(st): State<AppState>,
    _op: Operator,
    Path(name): Path<String>,
) -> ApiResult<StatusCode> {
    let o = org(&st, &name).await?;
    st.store.set_org_suspended(o.id, None).await?;
    tracing::info!("org {name} resumed by the operator");
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /v1/operator/orgs/{name}` with `{"confirm": "<name>"}`: delete
/// the org with all its data.
pub async fn delete_org(
    State(st): State<AppState>,
    _op: Operator,
    Path(name): Path<String>,
    Json(req): Json<p::DeleteOrgRequest>,
) -> ApiResult<StatusCode> {
    if req.confirm.trim() != name {
        return Err(ApiError::unprocessable(
            "confirm",
            "to delete an org, confirm with its name",
        ));
    }
    let o = org(&st, &name).await?;
    crate::public::delete_org(&st, &o).await?;
    tracing::info!("org {name} deleted by the operator");
    Ok(StatusCode::NO_CONTENT)
}
