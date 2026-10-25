//! Attention features on the server: acknowledgement escalation (system
//! mail), focus mode, and periodic cleanup.
//!
//! **Escalation.** A message sent with `ack_required` gets a deadline (the
//! sender's `ack_timeout_s`, else the org's `ack_timeout_min`). When it
//! passes and some recipients have not acknowledged, the sender gets a
//! system mail naming them. If they still have not acknowledged after the
//! same time again, the sender gets a second one with their leads (parents
//! in the sender's org) in Cc, once. The step is taken with a conditional
//! update, so with several instances only one sends it.
//!
//! **System mail** comes from the org's hidden `system` seat, which has no
//! tokens and cannot be addressed: no client can send it. It is labelled
//! `system`, is not counted against any budget and asks for no
//! acknowledgement.

use crate::AppState;
use crate::auth::Auth;
use crate::error::{ApiError, ApiResult, Json, Query};
use amail_proto::{DeliveryClass, DeliveryMode, FocusRequest, FocusResponse, Priority};
use amail_store::{AckDue, NewDelivery, NewMessage};
use axum::extract::State;
use serde::Deserialize;
use std::collections::BTreeSet;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Config {
    /// How often due escalations are looked for.
    pub scan_every: Duration,
    /// How often expired claim keys, watches and instances are dropped.
    pub cleanup_every: Duration,
    /// How often task due times and stale tasks are looked for (the stale
    /// pass itself runs at most once an hour per org).
    pub task_scan_every: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            scan_every: Duration::from_secs(5),
            cleanup_every: Duration::from_secs(60),
            task_scan_every: Duration::from_secs(60),
        }
    }
}

pub async fn run(st: AppState) {
    let mut last_cleanup = std::time::Instant::now();
    loop {
        tokio::time::sleep(st.attention.scan_every).await;
        if let Err(e) = escalate_due(&st).await {
            tracing::warn!("acknowledgement escalation: {e}");
        }
        if last_cleanup.elapsed() >= st.attention.cleanup_every {
            last_cleanup = std::time::Instant::now();
            if let Err(e) = st.store.cleanup().await {
                tracing::warn!("cleanup: {e}");
            }
            // Notice mail has its own retention, whatever the server's.
            let cutoff =
                amail_store::now_ms() - amail_proto::limits::NOTICE_RETENTION_DAYS * 86_400_000;
            match st.store.delete_old_notices(cutoff, 1000).await {
                Ok(0) => {}
                Ok(n) => tracing::info!("cleanup: deleted {n} notice mails"),
                Err(e) => tracing::warn!("cleanup of notice mail: {e}"),
            }
        }
    }
}

/// Take every escalation step that is due.
pub async fn escalate_due(st: &AppState) -> amail_store::Result<()> {
    let now = amail_store::now_ms();
    for due in st.store.due_acks(now, 200).await? {
        if let Err(e) = escalate(st, &due, now).await {
            tracing::warn!("escalating message {}: {e}", due.message_id);
        }
    }
    Ok(())
}

fn minutes(ms: i64) -> String {
    let m = (ms + 30_000) / 60_000;
    if m >= 1 {
        format!("{m} min")
    } else {
        format!("{} s", (ms / 1000).max(1))
    }
}

async fn escalate(st: &AppState, due: &AckDue, now: i64) -> amail_store::Result<()> {
    let store = &st.store;
    let laggards = store.laggards(due.message_id).await?;
    if laggards.is_empty() {
        store
            .advance_ack(due.message_id, due.stage, due.stage, None)
            .await?;
        return Ok(());
    }
    let Some(msg) = store.message(due.message_id).await? else {
        store
            .advance_ack(due.message_id, due.stage, due.stage, None)
            .await?;
        return Ok(());
    };
    let seats = store.seats(due.org_id).await?;
    let name_of = |id: i64| seats.iter().find(|s| s.id == id).map(|s| s.name.clone());
    // Leads in the sender's org (other than the sender) of local laggards.
    let leads: BTreeSet<i64> = laggards
        .iter()
        .filter(|l| l.org_id == due.org_id)
        .filter_map(|l| l.parent_id)
        .filter(|p| *p != due.sender_seat_id && name_of(*p).is_some())
        .collect();
    let waited = due.due - due.created_at;
    let mut who = Vec::new();
    let rows = if laggards.iter().any(|l| l.org_id != due.org_id) {
        store.recipients(due.message_id).await?
    } else {
        Vec::new()
    };
    for l in &laggards {
        let mut s = if l.org_id == due.org_id {
            l.name.clone()
        } else {
            let Some(d) = rows.iter().find(|d| d.seat_id == l.seat_id) else {
                continue;
            };
            format!("{}@{}", l.name, d.org_name)
        };
        if due.stage >= 1
            && l.org_id == due.org_id
            && let Some(p) = l.parent_id.and_then(name_of)
        {
            s.push_str(&format!(" (lead: {p})"));
        }
        who.push(s);
    }
    let (next_stage, next_due, body, cc) = if due.stage == 0 {
        let next = (!leads.is_empty()).then_some(now + waited.max(1000));
        let mut body = format!(
            "Your message #{} asked for an acknowledgement. After {} it has not been \
             acknowledged by: {}.",
            msg.id,
            minutes(waited),
            who.join(", ")
        );
        if next.is_some() {
            body.push_str(&format!(
                "\nIf it is still not acknowledged in {}, their leads get a copy of this notice.",
                minutes(waited)
            ));
        }
        (1, next, body, Vec::new())
    } else {
        if leads.is_empty() {
            store
                .advance_ack(due.message_id, due.stage, 2, None)
                .await?;
            return Ok(());
        }
        let body = format!(
            "Your message #{} is still not acknowledged after {} by: {}.\n\
             Their leads are copied on this notice.",
            msg.id,
            minutes(now - due.created_at),
            who.join(", ")
        );
        (2, None, body, leads.into_iter().collect())
    };
    // Only one instance takes the step.
    if !store
        .advance_ack(due.message_id, due.stage, next_stage, next_due)
        .await?
    {
        return Ok(());
    }
    // The original subject, cut as on any one-line display.
    let subject = format!(
        "Not acknowledged: {}",
        amail_proto::text::cut(&msg.subject, amail_proto::limits::SUBJECT_SHOWN_CHARS)
    );
    let system = store.system_seat(due.org_id).await?;
    let mut deliveries = vec![NewDelivery {
        seat_id: due.sender_seat_id,
        org_id: due.org_id,
        class: DeliveryClass::To,
        mode: DeliveryMode::Immediate,
    }];
    for p in &cc {
        deliveries.push(NewDelivery {
            seat_id: *p,
            org_id: due.org_id,
            class: DeliveryClass::Cc,
            mode: DeliveryMode::Immediate,
        });
    }
    let org_name = msg.org_name.clone();
    let addr = |id: i64| name_of(id).map(|n| format!("{n}@{org_name}"));
    let sent = store
        .send(&NewMessage {
            org_id: due.org_id,
            sender_seat_id: system.id,
            thread_id: Some(msg.thread_id),
            parent_id: Some(msg.id),
            depth: msg.depth + 1,
            subject,
            body,
            to_addrs: addr(due.sender_seat_id).into_iter().collect(),
            cc_addrs: cc.iter().filter_map(|p| addr(*p)).collect(),
            priority: Priority::Normal,
            ack_required: false,
            meta: Default::default(),
            deliveries,
            ack_due: None,
            written_by: None,
            // Reminders from the mail system are never refused.
            storage_quota: None,
            notice: false,
            guards: Vec::new(),
        })
        .await?;
    let events: Vec<(String, i64)> = sent
        .seqs
        .iter()
        .filter_map(|b| name_of(b.seat_id).map(|n| (n, b.seq)))
        .collect();
    st.bus.publish(due.org_id, &events);
    tracing::info!(
        "message {}: escalation step {next_stage} sent as #{}",
        msg.id,
        sent.message_id
    );
    Ok(())
}

// --- focus ---------------------------------------------------------------------

#[derive(Deserialize)]
pub struct SeatParam {
    seat: Option<String>,
}

/// `PUT /v1/focus`: only urgent mail reaches the seat until the given time
/// (at most the org's `focus_max_min`). Seat tokens for their own seat,
/// user tokens for any (`seat=`).
pub async fn put_focus(
    State(st): State<AppState>,
    auth: Auth,
    Query(q): Query<SeatParam>,
    Json(req): Json<FocusRequest>,
) -> ApiResult<Json<FocusResponse>> {
    if auth.scope == amail_proto::TokenScope::Node {
        return Err(ApiError::forbidden("node tokens cannot set focus"));
    }
    let seat = auth.mailbox(&st, q.seat.as_deref()).await?;
    let now = amail_store::now_ms();
    let max = auth.org.focus_max_min.max(1) * 60_000;
    let want = match (req.for_s, req.until.as_deref()) {
        (Some(s), _) => now.saturating_add((s.min(i64::MAX as u64 / 2000) * 1000) as i64),
        (None, Some(t)) => crate::render::parse_ts(t)
            .ok_or_else(|| ApiError::bad_request("until must be an RFC 3339 timestamp"))?,
        (None, None) => now + max,
    };
    if want <= now {
        return Err(ApiError::bad_request("focus must end in the future"));
    }
    let until = want.min(now + max);
    let bump = st.store.set_focus(seat.id, Some(until)).await?;
    st.bus
        .publish(bump.org_id, &[(seat.name.clone(), bump.seq)]);
    Ok(Json(FocusResponse {
        seat: seat.name,
        focus_until: Some(crate::render::ts(until)),
        capped: want > until,
    }))
}

/// `DELETE /v1/focus`: end the seat's focus; held mail is shown at once.
pub async fn delete_focus(
    State(st): State<AppState>,
    auth: Auth,
    Query(q): Query<SeatParam>,
) -> ApiResult<Json<FocusResponse>> {
    if auth.scope == amail_proto::TokenScope::Node {
        return Err(ApiError::forbidden("node tokens cannot set focus"));
    }
    let seat = auth.mailbox(&st, q.seat.as_deref()).await?;
    let bump = st.store.set_focus(seat.id, None).await?;
    st.bus
        .publish(bump.org_id, &[(seat.name.clone(), bump.seq)]);
    Ok(Json(FocusResponse {
        seat: seat.name,
        focus_until: None,
        capped: false,
    }))
}
