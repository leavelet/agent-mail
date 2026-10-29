//! Web control of seats (M10), the server's part: `POST
//! /v1/seats/{name}/prompt` and `/interrupt` (user tokens). The server
//! records what the user did (a worklog entry, and for a prompt a system
//! mail that keeps the text in the mail archive) and sends a command to
//! the seat's node on the event stream, as `sleep` and `wake` do; what
//! the node does with it depends on the harness (see
//! [`amail_proto::NodeCommand`]).

use crate::AppState;
use crate::auth::Auth;
use crate::error::{ApiError, ApiResult, Json, Path};
use amail_proto::{self as p, NodeCommand, Presence, SeatKind};
use amail_store::{NewDelivery, NewMessage, Seat};
use axum::extract::State;
use axum::http::StatusCode;

/// The agent seat `name` and its presence, refused with 409 `no_node`
/// when no node reports the seat or no node of the org is connected.
async fn hosted(st: &AppState, auth: &Auth, name: &str) -> ApiResult<(Seat, Presence)> {
    auth.require_user()?;
    let seat = st
        .store
        .seat_by_name(auth.org.id, name)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("no seat {name:?}")))?;
    if seat.kind != SeatKind::Agent {
        return Err(ApiError::unprocessable(
            "not_an_agent",
            "only agent seats have sessions",
        ));
    }
    let presence = st.presence.get(auth.org.id, &seat.name).await?;
    match presence {
        Some(p) if st.nodes.connected(auth.org.id) > 0 => Ok((seat, p)),
        _ => Err(no_node(&seat.name)),
    }
}

fn no_node(seat: &str) -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "no_node",
        format!("no connected node hosts {seat}"),
    )
}

fn send(st: &AppState, org_id: i64, seat: &Seat, cmd: NodeCommand) -> ApiResult<u32> {
    match st.nodes.send(org_id, cmd) {
        0 => Err(no_node(&seat.name)),
        n => Ok(n as u32),
    }
}

/// `POST /v1/seats/{name}/prompt {text}`: give the seat a prompt from the
/// user. Recorded as the seat's worklog entry `prompt_from_user` and as a
/// system mail with the text (a `Cc` archive delivery to the seat, which
/// notifies nobody); then the node gets a `prompt` command.
pub async fn prompt(
    State(st): State<AppState>,
    auth: Auth,
    Path(name): Path<String>,
    Json(req): Json<p::SeatPrompt>,
) -> ApiResult<Json<p::SeatControlResponse>> {
    let text = req.text.trim_end().to_string();
    if text.trim().is_empty() {
        return Err(ApiError::unprocessable(
            "prompt_empty",
            "the prompt is empty",
        ));
    }
    if text.len() > p::MAX_SEAT_PROMPT_BYTES {
        return Err(ApiError::unprocessable(
            "prompt_too_large",
            format!(
                "the prompt has {} bytes; the limit is {}",
                text.len(),
                p::MAX_SEAT_PROMPT_BYTES
            ),
        ));
    }
    let (seat, presence) = hosted(&st, &auth, &name).await?;
    let user = auth.mail_seat()?.clone();
    crate::handover::log(
        &st,
        auth.org.id,
        seat.id,
        "prompt_from_user",
        format!("{} sent a prompt: {}", user.name, text),
        None,
    )
    .await;
    archive_mail(&st, &auth, &seat, &user, &text).await;
    let cmd = NodeCommand {
        op: "prompt".into(),
        seat: seat.name.clone(),
        node: presence.node,
        text: Some(text),
        ..Default::default()
    };
    let nodes = send(&st, auth.org.id, &seat, cmd)?;
    Ok(Json(p::SeatControlResponse {
        queued: true,
        seat: seat.name,
        nodes,
    }))
}

/// The system mail that keeps a prompt in the mail archive. Best effort.
async fn archive_mail(st: &AppState, auth: &Auth, seat: &Seat, user: &Seat, text: &str) {
    let org = &auth.org;
    let system = match st.store.system_seat(org.id).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("org {}: prompt mail not stored: {e}", org.id);
            return;
        }
    };
    let subject = format!("Prompt from {} to {}", user.name, seat.name);
    let body = format!(
        "{user} sent this prompt to {seat} from the web interface; the node gave it to the \
         seat's session.\n\n{text}\n",
        user = user.name,
        seat = seat.name
    );
    let sent = st
        .store
        .send(&NewMessage {
            org_id: org.id,
            sender_seat_id: system.id,
            thread_id: None,
            parent_id: None,
            depth: 0,
            subject,
            body,
            to_addrs: Vec::new(),
            cc_addrs: vec![format!("{}@{}", seat.name, org.name)],
            priority: p::Priority::Normal,
            ack_required: false,
            meta: Default::default(),
            deliveries: vec![NewDelivery {
                seat_id: seat.id,
                org_id: org.id,
                class: p::DeliveryClass::Cc,
                mode: p::DeliveryMode::Archive,
            }],
            ack_due: None,
            written_by: None,
            storage_quota: None,
            notice: false,
            guards: Vec::new(),
        })
        .await;
    if let Err(e) = sent {
        tracing::warn!("org {}: prompt mail not stored: {e}", org.id);
    }
}

/// `POST /v1/seats/{name}/interrupt`: stop the seat's current turn. A
/// Claude Code seat has no structured interrupt: 409
/// `no_structured_interrupt` (the web shows the terminal instead).
/// Recorded as the seat's worklog entry `interrupt`; the node gets an
/// `interrupt` command.
pub async fn interrupt(
    State(st): State<AppState>,
    auth: Auth,
    Path(name): Path<String>,
) -> ApiResult<Json<p::SeatControlResponse>> {
    let (seat, presence) = hosted(&st, &auth, &name).await?;
    let harness = presence.harness.clone().or_else(|| seat.harness.clone());
    if harness.as_deref() == Some("claude") {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "no_structured_interrupt",
            format!(
                "{} runs Claude Code, which has no structured interrupt: use its terminal",
                seat.name
            ),
        ));
    }
    let user = auth.mail_seat()?.clone();
    crate::handover::log(
        &st,
        auth.org.id,
        seat.id,
        "interrupt",
        format!("{} interrupted the seat's turn", user.name),
        None,
    )
    .await;
    let cmd = NodeCommand {
        op: "interrupt".into(),
        seat: seat.name.clone(),
        node: presence.node,
        ..Default::default()
    };
    let nodes = send(&st, auth.org.id, &seat, cmd)?;
    Ok(Json(p::SeatControlResponse {
        queued: true,
        seat: seat.name,
        nodes,
    }))
}
