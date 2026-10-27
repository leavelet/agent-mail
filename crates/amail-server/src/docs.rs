//! Documents (`/v1/docs…`): an org's knowledge base. One record per
//! document with a number (`d12`), an owner, a kind, a status, a one-line
//! description and a Markdown body kept whole per revision.
//!
//! - Every call needs a user or seat token, except `GET /docs/index`,
//!   which a node token may call for an agent seat (`seat=`) to inject the
//!   seat's index. Everything stays inside the caller's org.
//! - The actor of every change is the token's seat (a user token: the user
//!   seat, which may do everything). The store checks who may write what:
//!   the owner, the user and the owner's `editors` revise; the owner and
//!   the user change status, owner, editors, pinning and expiry, and
//!   verify; kinds that are activated by the human (`decision`, or every
//!   kind of a team with `doc_activate_by = human`) need the user to
//!   become `active`.
//! - Comments are mail in the document's discussion thread (`[d12]
//!   <title>`), to the owner, copied to earlier commenters.
//! - Notification mail goes to the owner only: a revision by another seat
//!   (not with `silent`), a new document that supplements or supersedes
//!   theirs, and the reminders of the background pass (expiry once,
//!   dangling links once a day), sent through the task reminder path
//!   ([`crate::tasks::remind`]).
//! - Changes publish `doc` events (see [`DocHub`]) for the web interface.
//! - A subject with `[d12]` links the message's thread to document 12; a
//!   task title that names `d12` links the task ([`after_send`],
//!   [`link_task_title`]).

use crate::AppState;
use crate::auth::Auth;
use crate::error::{ApiError, ApiResult, Json, OptJson, Path, Query};
use crate::limits::Pool;
use crate::render;
use crate::tasks::Reminder;
use amail_proto::limits::*;
use amail_proto::{self as p, DocStatus, TokenScope};
use amail_store::docmd;
use amail_store::{
    DocBodyRec, DocChange, DocChanged, DocDetailRec, DocEventRec, DocKindPut, DocLinkRec, DocQuery,
    DocRec, DocSearchQuery, DocWrite, NewDocRec, NewMessage, Org, Seat, StoreError, TaskRec,
};
use axum::extract::State;
use axum::http::StatusCode;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Mutex;
use tokio::sync::broadcast;

// --- live updates ------------------------------------------------------------

const CHANNEL: usize = 256;

/// `doc` events per org for this instance's event streams (as
/// [`crate::tasks::TaskHub`] does for `task` events).
#[derive(Default)]
pub struct DocHub {
    orgs: Mutex<HashMap<i64, broadcast::Sender<p::DocBusEvent>>>,
}

impl DocHub {
    pub fn deliver(&self, org_id: i64, ev: p::DocBusEvent) {
        let mut orgs = self.orgs.lock().unwrap();
        if let Some(tx) = orgs.get(&org_id)
            && tx.send(ev).is_err()
        {
            orgs.remove(&org_id);
        }
    }

    pub fn subscribe(&self, org_id: i64) -> broadcast::Receiver<p::DocBusEvent> {
        self.orgs
            .lock()
            .unwrap()
            .entry(org_id)
            .or_insert_with(|| broadcast::channel(CHANNEL).0)
            .subscribe()
    }

    pub fn close_org(&self, org_id: i64) {
        self.orgs.lock().unwrap().remove(&org_id);
    }
}

/// Tell this instance's streams and the other instances that a document
/// changed.
pub fn publish(st: &AppState, org_id: i64, num: i64, change: &str, fields: &[String]) {
    let ev = p::DocBusEvent {
        num,
        change: change.to_string(),
        fields: fields.to_vec(),
    };
    st.docs.deliver(org_id, ev.clone());
    if let Some(c) = &st.cluster {
        c.notify(crate::cluster::Wire::Doc { o: org_id, v: ev });
    }
}

// --- rendering -----------------------------------------------------------------

/// The advice line of a document: split a large body; keep an index short.
pub fn advice(d: &DocRec) -> Option<String> {
    if d.tokens > DOC_SPLIT_ADVICE_TOKENS {
        return Some(format!(
            "the body is ~{} tokens; split it with amail doc split d{}",
            docmd::thousands(d.tokens),
            d.num
        ));
    }
    let chars = if d.body.is_empty() {
        d.body_bytes as usize
    } else {
        d.body.chars().count()
    };
    if d.kind == "index" && chars > DOC_INDEX_BODY_CHARS {
        return Some(format!(
            "an index document is shown cut at {} characters; this one has {}: move detail into the documents it lists",
            docmd::thousands(DOC_INDEX_BODY_CHARS as i64),
            docmd::thousands(chars as i64)
        ));
    }
    None
}

pub fn doc_out(d: &DocRec, now: i64) -> p::Doc {
    p::Doc {
        num: d.num,
        slug: d.slug.clone(),
        kind: d.kind.clone(),
        title: d.title.clone(),
        description: d.description.clone(),
        status: d.status,
        owner: d.owner.clone(),
        team: d.team.clone(),
        labels: d.labels.clone(),
        editors: d.editors.clone(),
        pinned: d.pinned,
        rev: d.rev,
        bytes: d.body_bytes,
        tokens: d.tokens,
        sha256: d.sha256.clone(),
        verified_at: d.verified_at.map(render::ts),
        verified_by: d.verified_by.clone(),
        expires_at: d.expires_at.map(render::ts),
        stale: d.stale(now),
        superseded_by: d.superseded_by,
        replaces: d.replaces.clone(),
        supplements: d.supplements,
        parent: d.parent,
        thread: d.thread_id,
        body: d.body.clone(),
        advice: advice(d),
        created_at: render::ts(d.created_at),
        updated_at: render::ts(d.updated_at),
    }
}

pub fn doc_ref(d: &DocRec, now: i64) -> p::DocRef {
    p::DocRef {
        num: d.num,
        slug: d.slug.clone(),
        kind: d.kind.clone(),
        title: d.title.clone(),
        description: d.description.clone(),
        status: d.status,
        stale: d.stale(now),
    }
}

/// A link's target as written: `d12#rollback`, `t3`, `#45`, `thread 7`,
/// `@w1`, `src/a.rs:10`.
fn shown(l: &DocLinkRec) -> String {
    match l.kind.as_str() {
        "doc" => match &l.anchor {
            Some(a) => format!("d{}#{a}", l.id),
            None => format!("d{}", l.id),
        },
        "task" => format!("t{}", l.id),
        "message" => format!("#{}", l.id),
        "thread" => format!("thread {}", l.id),
        "seat" => format!("@{}", l.id),
        _ => match l.line {
            Some(n) => format!("{}:{n}", l.id),
            None => l.id.clone(),
        },
    }
}

pub fn link_out(l: &DocLinkRec) -> p::DocLink {
    p::DocLink {
        src: l.src,
        src_anchor: l.src_anchor.clone(),
        kind: l.kind.clone(),
        target: shown(l),
        num: l.num,
        anchor: l.anchor.clone(),
        title_at_write: l.title_at_write.clone(),
        title: l.title.clone(),
        renamed: matches!((&l.title, &l.title_at_write), (Some(a), Some(b)) if a != b),
        dangling: l.dangling,
        stale: l.stale,
    }
}

fn event_out(e: &DocEventRec) -> p::DocEvent {
    p::DocEvent {
        seq: e.seq,
        at: render::ts(e.at),
        actor: e.actor.clone(),
        written_by: e.written_by.clone(),
        kind: e.kind,
        field: e.field.clone(),
        from: e.from.clone(),
        to: e.to.clone(),
        note: e.note.clone(),
        message_id: e.message_id,
        rev: e.rev,
    }
}

fn detail_out(r: &DocDetailRec, now: i64) -> p::DocDetail {
    let d = &r.doc;
    let find = |n: i64| {
        r.relatives
            .iter()
            .find(|x| x.num == n)
            .map(|x| doc_ref(x, now))
    };
    p::DocDetail {
        doc: doc_out(d, now),
        read: None,
        sections: r
            .sections
            .iter()
            .map(|s| p::DocSection {
                anchor: s.anchor.clone(),
                level: s.level,
                heading: s.heading.clone(),
                path: s.path.clone(),
                start: s.start,
                end: s.end,
                tokens: s.tokens,
            })
            .collect(),
        parent: d.parent.and_then(find),
        children: r.children.iter().filter_map(|n| find(*n)).collect(),
        superseded_by: d.superseded_by.and_then(find),
        replaces: d.replaces.iter().filter_map(|n| find(*n)).collect(),
        supplements: d.supplements.and_then(find),
        supplemented_by: r.supplemented_by.iter().filter_map(|n| find(*n)).collect(),
        related: d.related.iter().filter_map(|n| find(*n)).collect(),
        links: r.links.iter().map(link_out).collect(),
        linked_from: r.linked_from.iter().map(link_out).collect(),
        threads: r
            .threads
            .iter()
            .map(|t| p::DocThreadLink {
                thread_id: t.thread_id,
                linked_by: t.seat.clone(),
                linked_at: render::ts(t.created_at),
            })
            .collect(),
        tasks: r.tasks.iter().map(crate::tasks::task_ref).collect(),
        comments: r
            .comments
            .iter()
            .map(|c| p::DocComment {
                message_id: c.message_id,
                by: c.by.clone(),
                at: render::ts(c.at),
                anchor: c.anchor.clone(),
                path: c.path.clone(),
                removed: c.removed,
            })
            .collect(),
        events: r.events.iter().map(event_out).collect(),
        event_count: d.event_seq,
        existing: false,
        unchanged: false,
        candidates: Vec::new(),
        candidates_line: None,
        dangling: Vec::new(),
        moved: Vec::new(),
    }
}

/// The text a reminder mail shows about a document.
pub fn reminder_block(d: &DocRec) -> String {
    let mut s = format!(
        "\nd{}: {}\nKind: {}  Status: {}  Owner: {}",
        d.num, d.title, d.kind, d.status, d.owner
    );
    if let Some(v) = d.verified_at {
        s.push_str(&format!("  Verified: {}", render::ts(v)));
    }
    if let Some(e) = d.expires_at {
        s.push_str(&format!("  Expires: {}", render::ts(e)));
    }
    s.push_str(&format!("\nShow it: amail doc show d{}\n", d.num));
    s
}

// --- request checks --------------------------------------------------------------

/// The caller's seat: the actor of its changes.
fn actor(auth: &Auth) -> ApiResult<&Seat> {
    if auth.scope == TokenScope::Node {
        return Err(ApiError::forbidden("node tokens cannot use documents"));
    }
    auth.mail_seat()
}

fn write_ctx(st: &AppState, auth: &Auth, num: i64, if_rev: Option<i64>) -> ApiResult<DocWrite> {
    Ok(DocWrite {
        org_id: auth.org.id,
        num,
        actor_id: Some(actor(auth)?.id),
        is_user: auth.is_user(),
        written_by: None,
        quota: st.config.limits.org_storage_bytes,
        if_rev,
        bounds: st.config.limits.docs.bounds(),
    })
}

fn one_line(s: &str) -> bool {
    !s.chars()
        .any(|c| c.is_control() || amail_proto::text::is_line_break(c))
}

fn check_line(
    field: &str,
    code: &'static str,
    s: &str,
    max: usize,
    empty_ok: bool,
) -> ApiResult<String> {
    let t = s.trim();
    if t.is_empty() && !empty_ok {
        return Err(ApiError::unprocessable(
            code,
            format!("the {field} is empty"),
        ));
    }
    if !one_line(t) {
        return Err(ApiError::unprocessable(
            code,
            format!("the {field} must be a single line without control characters"),
        ));
    }
    if t.chars().count() > max {
        return Err(ApiError::unprocessable(
            code,
            format!("the {field} is limited to {max} characters"),
        ));
    }
    Ok(t.to_string())
}

fn check_title(s: &str) -> ApiResult<String> {
    check_line("title", "invalid_title", s, MAX_DOC_TITLE_CHARS, false)
}

fn check_description(s: &str) -> ApiResult<String> {
    check_line(
        "description",
        "invalid_description",
        s,
        MAX_DOC_DESCRIPTION_CHARS,
        true,
    )
}

fn check_message(s: &Option<String>) -> ApiResult<Option<String>> {
    match s {
        Some(m) => {
            let t = m.trim();
            if t.chars().count() > MAX_DOC_MESSAGE_CHARS {
                return Err(ApiError::unprocessable(
                    "invalid_message",
                    format!(
                        "the revision message is limited to {MAX_DOC_MESSAGE_CHARS} characters"
                    ),
                ));
            }
            Ok(Some(t.to_string()).filter(|t| !t.is_empty()))
        }
        None => Ok(None),
    }
}

/// `12` or `d12`: a document number.
fn as_num(s: &str) -> Option<i64> {
    let n = s.strip_prefix('d').unwrap_or(s);
    if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    n.parse::<i64>().ok().filter(|n| *n > 0)
}

fn check_slug(s: &str) -> ApiResult<String> {
    let t = s.trim();
    let ok = !t.is_empty()
        && t.chars().count() <= MAX_DOC_SLUG_CHARS
        && t.chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && t.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'));
    if !ok {
        return Err(ApiError::unprocessable(
            "invalid_slug",
            format!(
                "invalid slug {t:?}: 1-{MAX_DOC_SLUG_CHARS} characters of a-z, 0-9, '.', '_' and '-', starting with a letter or digit"
            ),
        ));
    }
    if as_num(t).is_some() {
        return Err(ApiError::unprocessable(
            "invalid_slug",
            format!("the slug {t:?} looks like a document number"),
        ));
    }
    Ok(t.to_string())
}

fn check_labels(v: &[String]) -> ApiResult<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for l in v {
        let l = l.trim();
        if l.is_empty()
            || l.chars().count() > MAX_DOC_LABEL_CHARS
            || l.chars()
                .any(|c| c == ',' || c.is_whitespace() || c.is_control())
        {
            return Err(ApiError::unprocessable(
                "invalid_label",
                format!(
                    "invalid label {l:?}: 1-{MAX_DOC_LABEL_CHARS} characters, no commas, spaces or control characters"
                ),
            ));
        }
        if !out.iter().any(|x| x == l) {
            out.push(l.to_string());
        }
    }
    if out.len() > MAX_DOC_LABELS {
        return Err(ApiError::unprocessable(
            "too_many_labels",
            format!("a document has at most {MAX_DOC_LABELS} labels"),
        ));
    }
    Ok(out)
}

/// Editors: live seats of the org, at most [`MAX_DOC_EDITORS`].
async fn check_editors(st: &AppState, org: &Org, v: &[String]) -> ApiResult<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for e in v {
        let e = e.trim();
        if !out.iter().any(|x| x == e) {
            seat_id(st, org, e).await?;
            out.push(e.to_string());
        }
    }
    if out.len() > MAX_DOC_EDITORS {
        return Err(ApiError::unprocessable(
            "too_many_editors",
            format!("a document has at most {MAX_DOC_EDITORS} editors"),
        ));
    }
    Ok(out)
}

fn check_body(s: &str) -> ApiResult<()> {
    if s.len() > MAX_DOC_BODY_BYTES {
        return Err(ApiError::unprocessable(
            "body_too_large",
            format!(
                "the body is {} bytes; a document holds at most {} bytes: split it into parts (amail doc split) or move sections out (amail doc extract)",
                docmd::thousands(s.len() as i64),
                docmd::thousands(MAX_DOC_BODY_BYTES as i64)
            ),
        ));
    }
    Ok(())
}

fn check_kind_name(s: &str) -> ApiResult<String> {
    let t = s.trim();
    let ok = !t.is_empty()
        && t.chars().count() <= MAX_DOC_KIND_NAME_CHARS
        && t.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && t.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-'));
    if !ok {
        return Err(ApiError::unprocessable(
            "invalid_kind",
            format!(
                "invalid kind {t:?}: 1-{MAX_DOC_KIND_NAME_CHARS} characters of a-z, 0-9, '_' and '-', starting with a letter"
            ),
        ));
    }
    Ok(t.to_string())
}

fn paths(v: &[p::DocPathCheck]) -> Vec<(String, bool)> {
    v.iter()
        .map(|c| (c.path.trim().to_string(), c.exists))
        .filter(|(p, _)| !p.is_empty())
        .collect()
}

/// A live seat of the org (not the hidden system seat).
async fn seat_id(st: &AppState, org: &Org, name: &str) -> ApiResult<i64> {
    match st.store.seat_by_name(org.id, name.trim()).await? {
        Some(s) if s.kind != p::SeatKind::System => Ok(s.id),
        _ => Err(ApiError::not_found(format!("no seat {name:?}"))),
    }
}

async fn team_id(st: &AppState, org: &Org, name: &str) -> ApiResult<i64> {
    st.store
        .teams(org.id)
        .await?
        .into_iter()
        .find(|t| t.name == name.trim())
        .map(|t| t.id)
        .ok_or_else(|| ApiError::not_found(format!("no team {name:?}")))
}

/// `d12`, `12`, `d12#anchor` or a slug (with `#anchor`): the document's
/// number and the anchor.
async fn resolve(st: &AppState, org_id: i64, s: &str) -> ApiResult<(i64, Option<String>)> {
    let s = s.trim();
    let (id, anchor) = match s.split_once('#') {
        Some((a, b)) => (a, Some(b.to_string()).filter(|b| !b.is_empty())),
        None => (s, None),
    };
    if let Some(n) = as_num(id) {
        return Ok((n, anchor));
    }
    match st.store.doc_num_by_slug(org_id, id).await? {
        Some(n) => Ok((n, anchor)),
        None => Err(ApiError::not_found(format!(
            "no document {id:?} (a number like d12, or a slug)"
        ))),
    }
}

/// Store errors of document writes: refusals are 422 with their code
/// (`stale`, `frozen`, `slug_taken` are 409, `not_owner` 403), the storage
/// quota 507 with the server's limit.
fn store_err(st: &AppState, e: StoreError) -> ApiError {
    match e {
        StoreError::Refused(c @ ("stale" | "frozen" | "slug_taken"), m) => {
            ApiError::new(StatusCode::CONFLICT, c, m)
        }
        StoreError::Refused("not_owner", m) => ApiError::new(StatusCode::FORBIDDEN, "not_owner", m),
        StoreError::Refused(code, m) => ApiError::unprocessable(code, m),
        StoreError::StorageQuota(stored) => {
            let mut e = ApiError::storage_quota(stored, st.config.limits.org_storage_bytes);
            e.message = e.message.replace("this message", "this change");
            e
        }
        e => e.into(),
    }
}

/// The error of a refused write to one document. `stale` is 409 with
/// `{error, rev, doc, since, diff}`: the document, the revisions and events
/// after `if_rev`, and the diff from `if_rev` to the current revision.
async fn write_failed(st: &AppState, w: &DocWrite, e: StoreError) -> ApiError {
    let StoreError::Refused("stale", msg) = e else {
        return store_err(st, e);
    };
    let mut err = ApiError::new(StatusCode::CONFLICT, "stale", msg);
    let since_rev = w.if_rev.unwrap_or(0);
    let now = amail_store::now_ms();
    let detail = st.store.doc_detail(w.org_id, w.num, DOC_SHOWN_EVENTS).await;
    let events = st
        .store
        .doc_events_since_rev(w.org_id, w.num, since_rev, MAX_DOC_PAGE)
        .await;
    if let (Ok(Some(d)), Ok(evs)) = (detail, events) {
        let diff = diff_text(st, w.org_id, w.num, since_rev, d.doc.rev)
            .await
            .unwrap_or_default();
        let mut extra = serde_json::Map::new();
        extra.insert("rev".into(), serde_json::json!(d.doc.rev));
        extra.insert(
            "doc".into(),
            serde_json::to_value(detail_out(&d, now)).unwrap_or_default(),
        );
        extra.insert(
            "since".into(),
            serde_json::to_value(evs.iter().map(event_out).collect::<Vec<_>>()).unwrap_or_default(),
        );
        extra.insert("diff".into(), serde_json::json!(diff));
        err.extra = Some(extra);
    }
    err
}

/// A unified diff between two revisions (a title or description change as
/// header lines).
async fn diff_text(st: &AppState, org_id: i64, num: i64, from: i64, to: i64) -> ApiResult<String> {
    let a = if from > 0 {
        st.store.doc_revision(org_id, num, from).await?
    } else {
        None
    };
    let b = st
        .store
        .doc_revision(org_id, num, to)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("d{num} has no revision {to}")))?;
    if from > 0 && a.is_none() {
        return Err(ApiError::not_found(format!(
            "d{num} has no revision {from}"
        )));
    }
    let mut out = String::new();
    let (at, ad, ab) = a
        .as_ref()
        .map(|a| (a.title.as_str(), a.description.as_str(), a.body.as_str()))
        .unwrap_or(("", "", ""));
    if at != b.title {
        out.push_str(&format!("title: {at} -> {}\n", b.title));
    }
    if ad != b.description {
        out.push_str(&format!("description: {ad} -> {}\n", b.description));
    }
    out.push_str(&docmd::unified_diff(
        ab,
        &b.body,
        &format!("d{num} rev {from}"),
        &format!("d{num} rev {to}"),
    ));
    Ok(out)
}

async fn detail(st: &AppState, org_id: i64, num: i64) -> ApiResult<p::DocDetail> {
    let now = amail_store::now_ms();
    st.store
        .doc_detail(org_id, num, DOC_SHOWN_EVENTS)
        .await?
        .map(|d| detail_out(&d, now))
        .ok_or_else(|| ApiError::not_found(format!("no document d{num}")))
}

/// The detail of a write's document with what the write found.
async fn written(st: &AppState, ch: &DocChanged) -> ApiResult<p::DocDetail> {
    let mut d = detail(st, ch.doc.org_id, ch.doc.num).await?;
    d.unchanged = ch.unchanged;
    d.existing = ch.existing;
    d.dangling = ch.dangling.iter().map(link_out).collect();
    Ok(d)
}

/// Mail the owner about another seat's revision, unless `silent`.
async fn notify_revision(st: &AppState, org: &Org, me: &Seat, ch: &DocChanged, silent: bool) {
    let actor = me.id;
    if silent || !ch.revised || ch.doc.owner_id == actor {
        return;
    }
    let d = &ch.doc;
    let who = &me.name;
    let mut r = Reminder::doc(
        d.owner_id,
        d.owner.clone(),
        "revised",
        d,
        format!("d{} was revised by {who}: {}", d.num, d.title),
    );
    r.extra = Some(format!(
        "\nSee the change:\n    amail doc diff d{n} {} {}\n",
        d.rev - 1,
        d.rev,
        n = d.num
    ));
    crate::tasks::remind(st, org, Some(actor), vec![r]).await;
}

/// Mail the owners of documents a write newly supplements or supersedes.
async fn notify_related(st: &AppState, org: &Org, actor: i64, ch: &DocChanged, silent: bool) {
    if silent {
        return;
    }
    let mut rs = Vec::new();
    for (rel, t) in &ch.related_to {
        let d = &ch.doc;
        let (kind, line) = match rel.as_str() {
            "supersedes" => (
                "superseded",
                format!(
                    "d{} {} is superseded by d{} {}",
                    d.num, d.title, t.num, t.title
                ),
            ),
            _ => (
                "supplemented",
                format!(
                    "d{} {} supplements your d{}: {}",
                    d.num, d.title, t.num, t.title
                ),
            ),
        };
        // For `supersedes` the store answers the replacing document; the
        // owner told is the owner of the replaced one.
        let (owner_id, owner, about) = if rel == "supersedes" {
            (d.owner_id, d.owner.clone(), d)
        } else {
            (t.owner_id, t.owner.clone(), t)
        };
        rs.push(Reminder::doc(owner_id, owner, kind, about, line));
    }
    crate::tasks::remind(st, org, Some(actor), rs).await;
}

/// The worklog entry of the caller's change of document `num` (agent
/// seats only).
async fn worklog(st: &AppState, auth: &Auth, num: i64, text: String) {
    if let Ok(me) = actor(auth) {
        crate::handover::doc_event(st, me, num, text).await;
    }
}

/// `: message`, or nothing.
fn with_message(m: Option<&str>) -> String {
    m.map(str::trim)
        .filter(|m| !m.is_empty())
        .map(|m| format!(": {m}"))
        .unwrap_or_default()
}

/// What a change of fields did, for the worklog: `draft → active`,
/// `changed owner, labels`.
fn change_text(ch: &DocChanged) -> Option<String> {
    let mut parts = Vec::new();
    if ch.before.status != ch.doc.status {
        parts.push(format!("{} → {}", ch.before.status, ch.doc.status));
    }
    let rest: Vec<&str> = ch
        .fields
        .iter()
        .map(String::as_str)
        .filter(|f| *f != "status")
        .collect();
    if !rest.is_empty() {
        parts.push(format!("changed {}", rest.join(", ")));
    }
    if ch.revised {
        parts.push(format!("rev {}", ch.doc.rev));
    }
    (!parts.is_empty()).then(|| format!("d{}: {}", ch.doc.num, parts.join("; ")))
}

// --- handlers ------------------------------------------------------------------

/// `POST /v1/docs`.
pub async fn create(
    State(st): State<AppState>,
    auth: Auth,
    Json(req): Json<p::NewDoc>,
) -> ApiResult<(StatusCode, Json<p::DocDetail>)> {
    let me = actor(&auth)?.clone();
    let org = &auth.org;
    let title = check_title(&req.title)?;
    let description = check_description(req.description.as_deref().unwrap_or(""))?;
    let body = req.body.clone().unwrap_or_default();
    check_body(&body)?;
    let kind = check_kind_name(req.kind.as_deref().unwrap_or("reference"))?;
    let slug = req.slug.as_deref().map(check_slug).transpose()?;
    let team = match &req.team {
        Some(n) => Some(team_id(&st, org, n).await?),
        None => None,
    };
    let labels = check_labels(&req.labels)?;
    let editors = check_editors(&st, org, &req.editors).await?;
    let parent = match &req.parent {
        Some(x) => Some(resolve(&st, org.id, x).await?.0),
        None => None,
    };
    let supplements = match &req.supplements {
        Some(x) => Some(resolve(&st, org.id, x).await?.0),
        None => None,
    };
    let owner_id = match &req.owner {
        Some(n) => seat_id(&st, org, n).await?,
        None => me.id,
    };
    let message = check_message(&req.message)?;
    if let Some(k) = &req.client_key {
        let n = k.chars().count();
        if n == 0 || n > MAX_TASK_CLIENT_KEY_CHARS || k.chars().any(char::is_control) {
            return Err(ApiError::unprocessable(
                "invalid_client_key",
                format!(
                    "client_key is 1 to {MAX_TASK_CLIENT_KEY_CHARS} characters without control characters"
                ),
            ));
        }
    }
    let w = write_ctx(&st, &auth, 0, None)?;
    let ch = st
        .store
        .create_doc(&NewDocRec {
            w: w.clone(),
            owner_id,
            kind: kind.clone(),
            title: title.clone(),
            description: description.clone(),
            body,
            slug: slug.clone(),
            team_id: team,
            labels,
            editors,
            parent,
            supplements,
            message,
            client_key: req.client_key.clone(),
            paths: paths(&req.paths),
        })
        .await
        .map_err(|e| store_err(&st, e))?;
    if ch.existing {
        return Ok((StatusCode::OK, Json(written(&st, &ch).await?)));
    }
    let num = ch.doc.num;
    publish(&st, org.id, num, "created", &[]);
    worklog(
        &st,
        &auth,
        num,
        format!(
            "d{num} created ({} {}): {}",
            ch.doc.status, ch.doc.kind, ch.doc.title
        ),
    )
    .await;
    let mut d = written(&st, &ch).await?;
    let now = amail_store::now_ms();
    let candidates = st
        .store
        .doc_candidates(
            org.id,
            &kind,
            &title,
            &description,
            slug.as_deref(),
            num,
            st.config.limits.docs.candidates_scan,
        )
        .await?;
    if !candidates.is_empty() {
        let names: Vec<String> = candidates
            .iter()
            .map(|(c, _)| format!("d{} {}", c.num, c.title))
            .collect();
        d.candidates_line = Some(format!(
            "similar documents exist: {}; supersede or supplement one of them if this is the same subject",
            names.join(", ")
        ));
        d.candidates = candidates
            .iter()
            .map(|(c, s)| p::DocCandidate {
                doc: doc_ref(c, now),
                similarity: (s * 100.0).round() / 100.0,
            })
            .collect();
    }
    notify_related(&st, org, me.id, &ch, req.silent).await;
    if owner_id != me.id && !req.silent {
        let r = Reminder::doc(
            owner_id,
            ch.doc.owner.clone(),
            "revised",
            &ch.doc,
            format!(
                "d{num} was created by {} with you as its owner: {}",
                me.name, ch.doc.title
            ),
        );
        crate::tasks::remind(&st, org, Some(me.id), vec![r]).await;
    }
    Ok((StatusCode::CREATED, Json(d)))
}

#[derive(Deserialize, Default)]
pub struct GetParams {
    rev: Option<i64>,
    section: Option<String>,
}

/// `GET /v1/docs/{num}` (`?rev=N`, `?section=anchor`; `{num}` may be
/// `d12#anchor` or a slug).
pub async fn get(
    State(st): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    Query(q): Query<GetParams>,
) -> ApiResult<Json<p::DocDetail>> {
    actor(&auth)?;
    let (num, anchor) = resolve(&st, auth.org.id, &id).await?;
    let anchor = q.section.clone().or(anchor);
    let mut d = detail(&st, auth.org.id, num).await?;
    if q.rev.is_none() && anchor.is_none() {
        return Ok(Json(d));
    }
    let (rev, title, body) = match q.rev {
        Some(r) => {
            let x = st
                .store
                .doc_revision(auth.org.id, num, r)
                .await?
                .ok_or_else(|| ApiError::not_found(format!("d{num} has no revision {r}")))?;
            (x.rev, x.title, x.body)
        }
        None => (
            d.doc.rev,
            d.doc.title.clone(),
            std::mem::take(&mut d.doc.body),
        ),
    };
    d.doc.body.clear();
    let read = match &anchor {
        Some(a) => {
            let parsed = docmd::parse(&body);
            let s = parsed.section(a).ok_or_else(|| {
                ApiError::unprocessable(
                    "no_section",
                    format!("d{num} has no section #{a} (amail doc read d{num} --sections)"),
                )
            })?;
            let text = body[s.start..s.end].to_string();
            p::DocText {
                rev,
                title,
                anchor: Some(a.clone()),
                path: Some(s.path.clone()),
                tokens: docmd::token_estimate(&text),
                text,
            }
        }
        None => p::DocText {
            rev,
            title,
            anchor: None,
            path: None,
            tokens: docmd::token_estimate(&body),
            text: body,
        },
    };
    d.read = Some(read);
    Ok(Json(d))
}

/// `PUT /v1/docs/{num}/body`: the body, or one section.
pub async fn put_body(
    State(st): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    Json(req): Json<p::DocBodyWrite>,
) -> ApiResult<Json<p::DocDetail>> {
    let (num, anchor) = resolve(&st, auth.org.id, &id).await?;
    let w = write_ctx(&st, &auth, num, req.if_rev)?;
    check_body(&req.body)?;
    let message = check_message(&req.message)?;
    let section = req.section.or(anchor);
    let ch = match st
        .store
        .write_doc_body(
            &w,
            &DocBodyRec {
                body: req.body,
                section: section.clone(),
                message: message.clone(),
                paths: paths(&req.paths),
            },
        )
        .await
    {
        Ok(c) => c,
        Err(e) => return Err(write_failed(&st, &w, e).await),
    };
    if ch.revised {
        publish(&st, auth.org.id, num, "revised", &ch.fields);
        notify_revision(&st, &auth.org, actor(&auth)?, &ch, req.silent).await;
        let at = section.map(|a| format!("#{a}")).unwrap_or_default();
        worklog(
            &st,
            &auth,
            num,
            format!(
                "d{num}{at} revised (rev {}){}",
                ch.doc.rev,
                with_message(message.as_deref())
            ),
        )
        .await;
    }
    Ok(Json(written(&st, &ch).await?))
}

/// `PATCH /v1/docs/{num}`.
pub async fn patch(
    State(st): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    Json(req): Json<p::DocPatch>,
) -> ApiResult<Json<p::DocDetail>> {
    let org = &auth.org;
    let (num, _) = resolve(&st, org.id, &id).await?;
    let w = write_ctx(&st, &auth, num, req.if_rev)?;
    let mut ch = DocChange {
        title: req.title.as_deref().map(check_title).transpose()?,
        description: req
            .description
            .as_deref()
            .map(check_description)
            .transpose()?,
        labels: req.labels.as_deref().map(check_labels).transpose()?,
        status: req.status,
        pinned: req.pinned,
        message: check_message(&req.message)?,
        ..Default::default()
    };
    if let Some(s) = &req.slug {
        ch.slug = Some(s.as_deref().map(check_slug).transpose()?);
    }
    if let Some(t) = &req.team {
        ch.team_id = Some(match t {
            Some(n) => Some(team_id(&st, org, n).await?),
            None => None,
        });
    }
    if let Some(x) = &req.parent {
        ch.parent = Some(match x {
            Some(x) => Some(resolve(&st, org.id, x).await?.0),
            None => None,
        });
    }
    if let Some(x) = &req.supplements {
        ch.supplements = Some(match x {
            Some(x) => Some(resolve(&st, org.id, x).await?.0),
            None => None,
        });
    }
    if let Some(v) = &req.related {
        let mut nums = Vec::new();
        for x in v {
            nums.push(resolve(&st, org.id, x).await?.0);
        }
        ch.related = Some(nums);
    }
    if let Some(o) = &req.owner {
        ch.owner_id = Some(seat_id(&st, org, o).await?);
    }
    if let Some(v) = &req.editors {
        ch.editors = Some(check_editors(&st, org, v).await?);
    }
    if let Some(e) = &req.expires_at {
        ch.expires_at = Some(match e {
            Some(t) => Some(render::parse_ts(t.trim()).ok_or_else(|| {
                ApiError::bad_request("expires_at must be an RFC 3339 timestamp")
            })?),
            None => None,
        });
    }
    let changed = match st.store.change_doc(&w, &ch).await {
        Ok(c) => c,
        Err(e) => return Err(write_failed(&st, &w, e).await),
    };
    if !changed.fields.is_empty() {
        publish(&st, org.id, num, "changed", &changed.fields);
    }
    let actor_id = w.actor_id.unwrap_or_default();
    notify_revision(&st, org, actor(&auth)?, &changed, req.silent).await;
    notify_related(&st, org, actor_id, &changed, req.silent).await;
    let d = &changed.doc;
    let mut owner_mail = Vec::new();
    if changed.fields.iter().any(|f| f == "owner") && d.owner_id != actor_id && !req.silent {
        owner_mail.push(Reminder::doc(
            d.owner_id,
            d.owner.clone(),
            "revised",
            d,
            format!("d{} is now yours to keep: {}", d.num, d.title),
        ));
    }
    // The owner learns that someone else activated (or archived, or set
    // back to draft) the document.
    if changed.before.status != d.status && d.owner_id != actor_id && !req.silent {
        let who = &actor(&auth)?.name;
        let what = match d.status {
            DocStatus::Active => "activated",
            DocStatus::Archived => "archived",
            DocStatus::Draft => "set back to draft",
            DocStatus::Superseded => "marked superseded",
        };
        owner_mail.push(Reminder::doc(
            d.owner_id,
            d.owner.clone(),
            "status",
            d,
            format!("d{} was {what} by {who}: {}", d.num, d.title),
        ));
    }
    if !owner_mail.is_empty() {
        crate::tasks::remind(&st, org, Some(actor_id), owner_mail).await;
    }
    if let Some(t) = change_text(&changed) {
        worklog(
            &st,
            &auth,
            num,
            format!("{t}{}", with_message(req.message.as_deref())),
        )
        .await;
    }
    Ok(Json(written(&st, &changed).await?))
}

/// `POST /v1/docs/{num}/supersede`: `by` replaces the document.
pub async fn supersede(
    State(st): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    Json(req): Json<p::DocSupersede>,
) -> ApiResult<Json<p::DocDetail>> {
    let (num, _) = resolve(&st, auth.org.id, &id).await?;
    let (by, _) = resolve(&st, auth.org.id, &req.by).await?;
    let w = write_ctx(&st, &auth, num, None)?;
    let ch = st
        .store
        .supersede_doc(&w, by)
        .await
        .map_err(|e| store_err(&st, e))?;
    publish(&st, auth.org.id, num, "relation", &ch.fields);
    publish(&st, auth.org.id, by, "relation", &["replaces".to_string()]);
    worklog(&st, &auth, num, format!("d{num} superseded by d{by}")).await;
    notify_related(
        &st,
        &auth.org,
        w.actor_id.unwrap_or_default(),
        &ch,
        req.silent,
    )
    .await;
    Ok(Json(written(&st, &ch).await?))
}

/// `30d`, `12h`, `2w`, `90` (days), or `none`: milliseconds (`None`: no
/// expiry).
fn parse_interval(s: &str) -> ApiResult<Option<i64>> {
    let t = s.trim().to_ascii_lowercase();
    if t == "none" || t == "never" || t == "0" {
        return Ok(None);
    }
    let (n, unit) = match t.char_indices().last() {
        Some((i, c)) if c.is_ascii_alphabetic() => (&t[..i], c),
        _ => (t.as_str(), 'd'),
    };
    let n: i64 = n
        .parse()
        .ok()
        .filter(|n| *n > 0 && *n <= 36_500)
        .ok_or_else(|| {
            ApiError::bad_request(format!("{s:?} is not an interval (30d, 12h, 2w or none)"))
        })?;
    let ms = match unit {
        'h' => n * 3_600_000,
        'd' => n * 86_400_000,
        'w' => n * 7 * 86_400_000,
        _ => {
            return Err(ApiError::bad_request(format!(
                "{s:?} is not an interval (30d, 12h, 2w or none)"
            )));
        }
    };
    Ok(Some(ms))
}

/// `POST /v1/docs/{num}/verify`.
pub async fn verify(
    State(st): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    OptJson(req): OptJson<p::DocVerify>,
) -> ApiResult<Json<p::DocDetail>> {
    let (num, _) = resolve(&st, auth.org.id, &id).await?;
    let w = write_ctx(&st, &auth, num, None)?;
    let req = req.unwrap_or_default();
    let valid = req.for_.as_deref().map(parse_interval).transpose()?;
    let ch = st
        .store
        .verify_doc(&w, valid)
        .await
        .map_err(|e| store_err(&st, e))?;
    publish(&st, auth.org.id, num, "verified", &ch.fields);
    let until = match ch.doc.expires_at {
        Some(t) => format!(" until {}", render::ts(t)),
        None => String::new(),
    };
    worklog(&st, &auth, num, format!("d{num} verified{until}")).await;
    Ok(Json(written(&st, &ch).await?))
}

/// The answer of `split`, `extract` and `merge`.
async fn moved_out(
    st: &AppState,
    org: &Org,
    num: i64,
    m: &amail_store::DocMovedRec,
    change: &str,
) -> ApiResult<p::DocDetail> {
    publish(st, org.id, num, change, &["body".to_string()]);
    for d in &m.created {
        publish(
            st,
            org.id,
            d.num,
            if change == "merged" {
                "revised"
            } else {
                "created"
            },
            &[],
        );
    }
    for n in &m.rewritten {
        publish(st, org.id, *n, "revised", &["body".to_string()]);
    }
    for n in &m.tasks_rewritten {
        crate::tasks::publish(st, org.id, *n, "changed", &["body".to_string()]);
    }
    let mut d = detail(st, org.id, num).await?;
    d.moved = m
        .moved
        .iter()
        .map(|(from, to)| p::DocMove {
            from: from.clone(),
            to: to.clone(),
        })
        .collect();
    Ok(d)
}

/// `POST /v1/docs/{num}/split`.
pub async fn split(
    State(st): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    OptJson(req): OptJson<p::DocSplit>,
) -> ApiResult<Json<p::DocDetail>> {
    let (num, _) = resolve(&st, auth.org.id, &id).await?;
    let req = req.unwrap_or_default();
    let level = req.level.unwrap_or(1);
    if !(1..=6).contains(&level) {
        return Err(ApiError::bad_request("level is a heading level, 1 to 6"));
    }
    let w = write_ctx(&st, &auth, num, req.if_rev)?;
    let m = match st.store.split_doc(&w, level).await {
        Ok(m) => m,
        Err(e) => return Err(write_failed(&st, &w, e).await),
    };
    let into: Vec<String> = m.created.iter().map(|d| format!("d{}", d.num)).collect();
    worklog(
        &st,
        &auth,
        num,
        format!("d{num} split into {}", into.join(", ")),
    )
    .await;
    Ok(Json(moved_out(&st, &auth.org, num, &m, "split").await?))
}

/// `POST /v1/docs/{num}/extract`.
pub async fn extract(
    State(st): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    Json(req): Json<p::DocExtract>,
) -> ApiResult<Json<p::DocDetail>> {
    let (num, anchor) = resolve(&st, auth.org.id, &id).await?;
    let anchor = Some(req.anchor.trim().trim_start_matches('#').to_string())
        .filter(|a| !a.is_empty())
        .or(anchor)
        .ok_or_else(|| ApiError::bad_request("anchor is required"))?;
    let title = check_title(&req.into)?;
    let kind = req.kind.as_deref().map(check_kind_name).transpose()?;
    let w = write_ctx(&st, &auth, num, req.if_rev)?;
    let m = match st
        .store
        .extract_doc(&w, &anchor, &title, kind.as_deref())
        .await
    {
        Ok(m) => m,
        Err(e) => return Err(write_failed(&st, &w, e).await),
    };
    let into: Vec<String> = m.created.iter().map(|d| format!("d{}", d.num)).collect();
    worklog(
        &st,
        &auth,
        num,
        format!("d{num}#{anchor} extracted into {}", into.join(", ")),
    )
    .await;
    Ok(Json(moved_out(&st, &auth.org, num, &m, "extracted").await?))
}

/// `POST /v1/docs/{num}/merge`: `num` is appended to `into`.
pub async fn merge(
    State(st): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    Json(req): Json<p::DocMerge>,
) -> ApiResult<Json<p::DocDetail>> {
    let (num, _) = resolve(&st, auth.org.id, &id).await?;
    let (into, _) = resolve(&st, auth.org.id, &req.into).await?;
    let w = write_ctx(&st, &auth, num, req.if_rev)?;
    let m = match st.store.merge_doc(&w, into).await {
        Ok(m) => m,
        Err(e) => return Err(write_failed(&st, &w, e).await),
    };
    worklog(&st, &auth, num, format!("d{num} merged into d{into}")).await;
    Ok(Json(moved_out(&st, &auth.org, num, &m, "merged").await?))
}

#[derive(Deserialize, Default)]
pub struct HistoryParams {
    after_seq: Option<i64>,
    limit: Option<i64>,
}

/// `GET /v1/docs/{num}/history`: every revision and a page of events.
pub async fn history(
    State(st): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    Query(q): Query<HistoryParams>,
) -> ApiResult<Json<p::DocHistory>> {
    actor(&auth)?;
    let (num, _) = resolve(&st, auth.org.id, &id).await?;
    let limit = q.limit.unwrap_or(200).clamp(1, MAX_DOC_PAGE);
    let evs = st
        .store
        .doc_events(auth.org.id, num, q.after_seq.unwrap_or(0), limit + 1)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("no document d{num}")))?;
    let more = evs.len() as i64 > limit;
    let events: Vec<p::DocEvent> = evs.iter().take(limit as usize).map(event_out).collect();
    let revisions = st
        .store
        .doc_revisions(auth.org.id, num)
        .await?
        .into_iter()
        .map(|r| p::DocRevision {
            rev: r.rev,
            title: r.title,
            author: r.author,
            written_by: r.written_by,
            message: r.message,
            sha256: r.sha256,
            bytes: r.bytes,
            at: render::ts(r.at),
        })
        .collect();
    Ok(Json(p::DocHistory {
        revisions,
        next_after_seq: if more {
            events.last().map(|e| e.seq)
        } else {
            None
        },
        events,
    }))
}

#[derive(Deserialize, Default)]
pub struct DiffParams {
    from: Option<i64>,
    to: Option<i64>,
}

/// `GET /v1/docs/{num}/diff?from=N&to=M` (default: the last change).
pub async fn diff(
    State(st): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    Query(q): Query<DiffParams>,
) -> ApiResult<Json<p::DocDiff>> {
    actor(&auth)?;
    let (num, _) = resolve(&st, auth.org.id, &id).await?;
    let d = st
        .store
        .doc(auth.org.id, num)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("no document d{num}")))?;
    let to = q.to.unwrap_or(d.rev);
    let from = q.from.unwrap_or(to - 1).max(0);
    let diff = diff_text(&st, auth.org.id, num, from, to).await?;
    Ok(Json(p::DocDiff {
        num,
        from,
        to,
        diff,
    }))
}

/// `GET /v1/docs/{num}/links`: links out and backlinks.
pub async fn links(
    State(st): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
) -> ApiResult<Json<p::DocLinks>> {
    actor(&auth)?;
    let (num, _) = resolve(&st, auth.org.id, &id).await?;
    let (out, inn) = st
        .store
        .doc_links(auth.org.id, num)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("no document d{num}")))?;
    Ok(Json(p::DocLinks {
        links: out.iter().map(link_out).collect(),
        linked_from: inn.iter().map(link_out).collect(),
    }))
}

#[derive(Deserialize, Default)]
pub struct ListParams {
    kind: Option<String>,
    status: Option<String>,
    team: Option<String>,
    owner: Option<String>,
    label: Option<String>,
    stale: Option<String>,
    pinned: Option<String>,
    before: Option<i64>,
    limit: Option<i64>,
    /// `search`, `find`: the terms.
    q: Option<String>,
    /// `index`: the node's budget, and the agent seat (node tokens).
    max_lines: Option<i64>,
    max_chars: Option<i64>,
    seat: Option<String>,
}

fn flag(v: &Option<String>) -> bool {
    matches!(v.as_deref(), Some("1" | "true" | "yes" | ""))
}

/// `draft`, `active,draft`, `all`; `None`: the call's default.
fn statuses(v: Option<&str>) -> ApiResult<Vec<DocStatus>> {
    match v {
        None => Ok(Vec::new()),
        Some("all") => Ok(DocStatus::ALL.to_vec()),
        Some(s) => s
            .split(',')
            .map(|x| x.trim().parse::<DocStatus>().map_err(ApiError::bad_request))
            .collect(),
    }
}

fn terms(q: Option<&str>) -> ApiResult<Vec<String>> {
    let t: Vec<String> = q
        .unwrap_or("")
        .split_whitespace()
        .map(str::to_string)
        .take(10)
        .collect();
    if t.is_empty() {
        return Err(ApiError::bad_request("q (the search terms) is required"));
    }
    Ok(t)
}

/// `GET /v1/docs`: newest first.
pub async fn list(
    State(st): State<AppState>,
    auth: Auth,
    Query(q): Query<ListParams>,
) -> ApiResult<Json<p::DocList>> {
    actor(&auth)?;
    let org = auth.org.clone();
    let query = DocQuery {
        org_id: org.id,
        kind: q.kind.clone(),
        statuses: statuses(q.status.as_deref())?,
        team_id: match &q.team {
            Some(t) => Some(team_id(&st, &org, t).await?),
            None => None,
        },
        owner_id: match &q.owner {
            Some(o) => Some(seat_id(&st, &org, o).await?),
            None => None,
        },
        label: q.label.clone(),
        stale: flag(&q.stale),
        pinned: flag(&q.pinned),
        terms: Vec::new(),
        before: q.before,
        limit: q.limit.unwrap_or(100).clamp(1, MAX_DOC_PAGE),
        now: amail_store::now_ms(),
    };
    let limit = query.limit;
    let st2 = st.clone();
    let docs = crate::mail::limited(&st, &auth, Pool::Other, async move {
        Ok(st2.store.docs(&query).await?)
    })
    .await?;
    let now = amail_store::now_ms();
    Ok(Json(p::DocList {
        next_before: if docs.len() as i64 == limit {
            docs.last().map(|d| d.num)
        } else {
            None
        },
        docs: docs.iter().map(|d| doc_out(d, now)).collect(),
    }))
}

/// `GET /v1/docs/find?q=`: titles, slugs and descriptions, every status.
pub async fn find(
    State(st): State<AppState>,
    auth: Auth,
    Query(q): Query<ListParams>,
) -> ApiResult<Json<p::DocList>> {
    actor(&auth)?;
    let query = DocQuery {
        org_id: auth.org.id,
        statuses: match statuses(q.status.as_deref())? {
            v if v.is_empty() => DocStatus::ALL.to_vec(),
            v => v,
        },
        kind: q.kind.clone(),
        terms: terms(q.q.as_deref())?,
        limit: q.limit.unwrap_or(20).clamp(1, MAX_DOC_PAGE),
        now: amail_store::now_ms(),
        ..Default::default()
    };
    let st2 = st.clone();
    let docs = crate::mail::limited(&st, &auth, Pool::Other, async move {
        Ok(st2.store.docs(&query).await?)
    })
    .await?;
    let now = amail_store::now_ms();
    Ok(Json(p::DocList {
        next_before: None,
        docs: docs.iter().map(|d| doc_out(d, now)).collect(),
    }))
}

/// A heading line's text without its attribute block (`Rounding
/// {#rounding}` is `Rounding`).
fn heading_text(line: &str) -> &str {
    let t = line.trim_start_matches('#').trim();
    match t.rfind('{') {
        Some(i) if t.ends_with('}') && matches!(t[i + 1..].chars().next(), Some('#' | '.')) => {
            t[..i].trim_end()
        }
        _ => t,
    }
}

/// The sentence of a section's text that holds the most terms (the heading
/// line only when nothing else matches), cut to [`DOC_SNIPPET_CHARS`]
/// characters. A sentence ends at `.`, `!` or `?` followed by a space or
/// the end of the line (not inside a code span), and at a line's end. A
/// heading shows without its `{#id}` attribute, and a cut never ends inside
/// a code span.
pub fn snippet(text: &str, terms: &[String]) -> String {
    let t = text.trim_start();
    let content = if t.starts_with('#') {
        t.split_once('\n').map_or("", |x| x.1)
    } else {
        text
    };
    let sentences = |t: &str| -> Vec<String> {
        let mut out = Vec::new();
        let mut cur = String::new();
        let mut code = false;
        let mut chars = t.chars().peekable();
        while let Some(c) = chars.next() {
            cur.push(c);
            match c {
                '`' => code = !code,
                '\n' => {
                    code = false;
                    out.push(std::mem::take(&mut cur));
                }
                '。' | '！' | '？' if !code => out.push(std::mem::take(&mut cur)),
                '.' | '!' | '?' if !code && chars.peek().is_none_or(|n| n.is_whitespace()) => {
                    out.push(std::mem::take(&mut cur))
                }
                _ => {}
            }
        }
        out.push(cur);
        out.into_iter()
            .map(|s| {
                let s = s.trim();
                if s.starts_with('#') {
                    heading_text(s).to_string()
                } else {
                    s.to_string()
                }
            })
            .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|s| !s.is_empty())
            .collect()
    };
    let lower: Vec<String> = terms.iter().map(|t| t.to_lowercase()).collect();
    let score = |s: &str| {
        let l = s.to_lowercase();
        lower.iter().filter(|t| l.contains(t.as_str())).count()
    };
    let mut best: Option<(usize, String)> = None;
    for s in sentences(content).into_iter().chain(sentences(text)) {
        let n = score(&s);
        if n > 0 && best.as_ref().is_none_or(|b| n > b.0) {
            best = Some((n, s));
        }
    }
    let s = best
        .map(|b| b.1)
        .or_else(|| sentences(content).into_iter().next())
        .unwrap_or_default();
    if s.chars().count() <= DOC_SNIPPET_CHARS {
        return s;
    }
    let mut cut: String = s.chars().take(DOC_SNIPPET_CHARS - 1).collect();
    // Not inside a code span: end before the span's opening backtick.
    if cut.matches('`').count() % 2 == 1
        && let Some(i) = cut.rfind('`')
    {
        cut.truncate(i);
        let kept = cut.trim_end().len();
        cut.truncate(kept);
    }
    format!("{cut}…")
}

/// `GET /v1/docs/search?q=`: one hit per matching section.
pub async fn search(
    State(st): State<AppState>,
    auth: Auth,
    Query(q): Query<ListParams>,
) -> ApiResult<Json<p::DocSearchResult>> {
    let me = actor(&auth)?.clone();
    let org = auth.org.clone();
    let terms = terms(q.q.as_deref())?;
    let mut limit = q.limit.unwrap_or(DOC_SEARCH_HITS).max(1);
    if let Some(max) = st.config.limits.docs.search_hits {
        limit = limit.min(max);
    }
    let query = DocSearchQuery {
        org_id: org.id,
        terms: terms.clone(),
        kind: q.kind.clone(),
        statuses: statuses(q.status.as_deref())?,
        team_id: match &q.team {
            Some(t) => Some(team_id(&st, &org, t).await?),
            None => None,
        },
        label: q.label.clone(),
        owner_id: match &q.owner {
            Some(o) => Some(seat_id(&st, &org, o).await?),
            None => None,
        },
        caller_team: me.team_id,
        limit,
        now: amail_store::now_ms(),
    };
    let now = query.now;
    let st2 = st.clone();
    let res = crate::mail::limited(&st, &auth, Pool::Scan, async move {
        Ok(st2.store.search_docs(&query).await?)
    })
    .await?;
    let hits = res
        .hits
        .iter()
        .map(|h| {
            let d = &h.doc;
            let snip = if res.titles_only {
                if d.description.is_empty() {
                    d.title.clone()
                } else {
                    d.description.clone()
                }
            } else {
                snippet(&h.text, &terms)
            };
            let stale = d.stale(now);
            let target = if h.anchor.is_empty() {
                format!("d{}", d.num)
            } else {
                format!("d{}#{}", d.num, h.anchor)
            };
            let place = if h.path.is_empty() {
                d.title.as_str()
            } else {
                h.path.as_str()
            };
            let mut line = format!(
                "{target}  {}  {}{}  {place} — {snip}",
                d.kind,
                d.status,
                if stale { "  stale" } else { "" }
            );
            if let Some(v) = d.verified_at {
                line.push_str(&format!("  (verified {})", &render::ts(v)[..10]));
            }
            p::DocSearchHit {
                num: d.num,
                slug: d.slug.clone(),
                anchor: h.anchor.clone(),
                path: h.path.clone(),
                kind: d.kind.clone(),
                status: d.status,
                title: d.title.clone(),
                stale,
                verified_at: d.verified_at.map(render::ts),
                snippet: snip,
                line,
            }
        })
        .collect();
    Ok(Json(p::DocSearchResult {
        hits,
        titles_only: res.titles_only,
    }))
}

/// The index line of a document: `d12  runbook  <title> — <description>`,
/// with `  (stale)` when it is past its expiry.
pub fn index_line(d: &DocRec, now: i64) -> String {
    let mut s = format!("d{}  {}  {}", d.num, d.kind, d.title);
    if !d.description.is_empty() {
        s.push_str(" — ");
        s.push_str(&d.description);
    }
    if d.stale(now) {
        s.push_str("  (stale)");
    }
    s
}

/// Cut `lines` to `max_lines` and `max_chars` (newlines counted, the
/// more line included): the lines kept and the more line.
pub fn cut_index(
    lines: &[String],
    total: i64,
    max_lines: i64,
    max_chars: i64,
    team: Option<&str>,
) -> (Vec<String>, Option<String>) {
    let more_line = |n: i64| match team {
        Some(t) => format!("… and {n} more: amail doc list --team {t}"),
        None => format!("… and {n} more: amail doc list"),
    };
    let fits = |v: &[String], extra: usize| {
        let chars: usize = v.iter().map(|l| l.chars().count() + 1).sum::<usize>() + extra;
        v.len() as i64 <= max_lines && chars as i64 <= max_chars
    };
    if total as usize == lines.len() && fits(lines, 0) {
        return (lines.to_vec(), None);
    }
    let mut kept: Vec<String> = Vec::new();
    for l in lines {
        kept.push(l.clone());
        let rest = total - kept.len() as i64;
        let extra = more_line(rest.max(1)).chars().count();
        if !fits(&kept, extra) {
            kept.pop();
            break;
        }
    }
    let more = total - kept.len() as i64;
    (kept, (more > 0).then(|| more_line(more)))
}

/// `GET /v1/docs/index?team=&max_lines=&max_chars=`: what a seat is shown
/// each turn. A node token names the agent seat with `seat=`.
pub async fn index(
    State(st): State<AppState>,
    auth: Auth,
    Query(q): Query<ListParams>,
) -> ApiResult<Json<p::DocIndex>> {
    let org = auth.org.clone();
    let seat = if auth.scope == TokenScope::Node {
        auth.mailbox(&st, q.seat.as_deref()).await?
    } else {
        actor(&auth)?.clone()
    };
    let (team, team_name) = match &q.team {
        Some(t) => (
            Some(team_id(&st, &org, t).await?),
            Some(t.trim().to_string()),
        ),
        None => match seat.team_id {
            Some(t) => {
                let name = st
                    .store
                    .teams(org.id)
                    .await?
                    .into_iter()
                    .find(|x| x.id == t)
                    .map(|x| x.name);
                (Some(t), name)
            }
            None => (None, None),
        },
    };
    let now = amail_store::now_ms();
    let scan = st.config.limits.docs.index_scan;
    let st2 = st.clone();
    let (docs, total) = crate::mail::limited(&st, &auth, Pool::Light, async move {
        Ok(st2.store.doc_index(org.id, team, now, scan).await?)
    })
    .await?;
    let lines: Vec<String> = docs.iter().map(|d| index_line(d, now)).collect();
    let (lines, more_line) = cut_index(
        &lines,
        total,
        q.max_lines.unwrap_or(DOC_INDEX_LINES).max(0),
        q.max_chars.unwrap_or(DOC_INDEX_CHARS).max(0),
        team_name.as_deref(),
    );
    let mut text = lines.join("\n");
    if let Some(m) = &more_line {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(m);
    }
    Ok(Json(p::DocIndex {
        team: team_name,
        more: total - lines.len() as i64,
        lines,
        more_line,
        text,
    }))
}

/// `GET /v1/docs/check?team=`: dangling and stale links.
pub async fn check(
    State(st): State<AppState>,
    auth: Auth,
    Query(q): Query<ListParams>,
) -> ApiResult<Json<p::DocCheck>> {
    actor(&auth)?;
    let org = auth.org.clone();
    let team = match &q.team {
        Some(t) => Some(team_id(&st, &org, t).await?),
        None => None,
    };
    let limit = q.limit.unwrap_or(MAX_DOC_PAGE).clamp(1, MAX_DOC_PAGE);
    let st2 = st.clone();
    let rows = crate::mail::limited(&st, &auth, Pool::Other, async move {
        Ok(st2.store.doc_check(org.id, team, limit).await?)
    })
    .await?;
    Ok(Json(p::DocCheck {
        problems: rows.iter().map(link_out).collect(),
    }))
}

fn kind_out(k: &amail_store::DocKindRec) -> p::DocKind {
    p::DocKind {
        name: k.name.clone(),
        description: k.description.clone(),
        required_sections: k.required_sections.clone(),
        activate_by: k.activate_by,
        in_index: k.in_index,
        body_cap_bytes: k.body_cap_bytes,
        verify_days: k.verify_days,
        builtin: k.builtin,
        disabled: k.disabled,
        docs: k.docs,
    }
}

/// `GET /v1/docs/kinds`.
pub async fn kinds(State(st): State<AppState>, auth: Auth) -> ApiResult<Json<p::DocKindList>> {
    actor(&auth)?;
    let kinds = st.store.doc_kinds(auth.org.id).await?;
    Ok(Json(p::DocKindList {
        kinds: kinds.iter().map(kind_out).collect(),
    }))
}

/// `POST /v1/docs/kinds` (user token): add a kind or change one.
pub async fn put_kind(
    State(st): State<AppState>,
    auth: Auth,
    Json(req): Json<p::NewDocKind>,
) -> ApiResult<Json<p::DocKind>> {
    auth.require_user()?;
    let name = check_kind_name(&req.name)?;
    let description = req
        .description
        .as_deref()
        .map(|d| {
            check_line(
                "description",
                "invalid_description",
                d,
                MAX_DOC_DESCRIPTION_CHARS,
                true,
            )
        })
        .transpose()?;
    let sections = match &req.required_sections {
        Some(v) => {
            let mut out = Vec::new();
            for s in v {
                let t = check_line(
                    "section title",
                    "invalid_section",
                    s,
                    MAX_DOC_SECTION_NAME_CHARS,
                    false,
                )?;
                if t.contains(',') {
                    return Err(ApiError::unprocessable(
                        "invalid_section",
                        "a section title has no commas",
                    ));
                }
                out.push(t);
            }
            Some(out)
        }
        None => None,
    };
    if let Some(c) = req.body_cap_bytes
        && !(1..=MAX_DOC_BODY_BYTES as i64).contains(&c)
    {
        return Err(ApiError::unprocessable(
            "invalid_cap",
            format!("body_cap_bytes is 1 to {MAX_DOC_BODY_BYTES}"),
        ));
    }
    if req.verify_days.is_some_and(|d| !(0..=36_500).contains(&d)) {
        return Err(ApiError::unprocessable(
            "invalid_interval",
            "verify_days is 0 (none) to 36500",
        ));
    }
    let k = st
        .store
        .put_doc_kind(
            auth.org.id,
            &DocKindPut {
                name,
                description,
                required_sections: sections,
                activate_by: req.activate_by,
                in_index: req.in_index,
                body_cap_bytes: req.body_cap_bytes,
                verify_days: req.verify_days.map(|d| (d > 0).then_some(d)),
                disabled: req.disabled,
            },
            st.config.limits.docs.kinds_per_org,
        )
        .await
        .map_err(|e| store_err(&st, e))?;
    Ok(Json(kind_out(&k)))
}

/// `POST /v1/docs/{num}/comment`: a message in the document's discussion
/// thread (created by the first comment) with subject `[d12] <title>`, to
/// the owner, copied to the seats that commented before (the sender left
/// out; with nobody else, to the sender). A comment on a section starts
/// with the line `On d12#rollback (Deploy > Rollback):`.
pub async fn comment(
    State(st): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    Json(req): Json<p::DocCommentRequest>,
) -> ApiResult<Json<p::DocCommentResponse>> {
    let me = actor(&auth)?.clone();
    let (num, anchor) = resolve(&st, auth.org.id, &id).await?;
    let anchor = req
        .anchor
        .as_deref()
        .map(|a| a.trim().trim_start_matches('#').to_string())
        .filter(|a| !a.is_empty())
        .or(anchor);
    if req.body.trim().is_empty() {
        return Err(ApiError::bad_request("the comment is empty"));
    }
    let d = st
        .store
        .doc(auth.org.id, num)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("no document d{num}")))?;
    let path = match &anchor {
        Some(a) => {
            let parsed = docmd::parse(&d.body);
            let s = parsed.section(a).ok_or_else(|| {
                ApiError::unprocessable(
                    "no_section",
                    format!("d{num} has no section #{a} (amail doc read d{num} --sections)"),
                )
            })?;
            Some(s.path.clone())
        }
        None => None,
    };
    let mut to: Vec<String> = Vec::new();
    if d.owner != me.name {
        to.push(d.owner.clone());
    }
    let mut cc: Vec<String> = Vec::new();
    for a in st.store.doc_commenters(auth.org.id, num).await? {
        if a != me.name && !to.contains(&a) && !cc.contains(&a) {
            cc.push(a);
        }
    }
    let mut names = to.clone();
    names.extend(cc.iter().cloned());
    let live: Vec<String> = st
        .store
        .seats_by_names(auth.org.id, &names)
        .await?
        .into_iter()
        .map(|s| s.name)
        .collect();
    to.retain(|n| live.contains(n));
    cc.retain(|n| live.contains(n));
    // A message needs a recipient: with nobody else to tell (the owner
    // comments and nobody else has), the comment goes to the caller and is
    // marked read at once, so that nobody is mailed.
    let alone = to.is_empty() && cc.is_empty();
    if alone {
        to.push(me.name.clone());
    }
    let subject: String = format!("[d{num}] {}", d.title)
        .chars()
        .take(MAX_SUBJECT_CHARS)
        .collect();
    let body = match (&anchor, &path) {
        (Some(a), Some(p)) => format!("On d{num}#{a} ({p}):\n\n{}", req.body),
        _ => req.body.clone(),
    };
    let sent = crate::send::send_comment(
        &st,
        &auth,
        &me,
        p::SendRequest {
            to,
            cc,
            subject,
            body,
            in_reply_to: d.thread_id,
            ..Default::default()
        },
    )
    .await?;
    let w = write_ctx(&st, &auth, num, None)?;
    let d = st
        .store
        .record_doc_comment(
            &w,
            sent.id,
            sent.thread_id,
            anchor.as_deref(),
            path.as_deref(),
        )
        .await
        .map_err(|e| store_err(&st, e))?;
    publish(&st, auth.org.id, num, "commented", &[]);
    if alone {
        match st.store.mark_read(sent.id, me.id).await {
            Ok(Some((_, Some(b)))) => st.bus.publish(b.org_id, &[(me.name.clone(), b.seq)]),
            Ok(_) => {}
            Err(e) => tracing::warn!("org {}: own comment not marked read: {e}", auth.org.id),
        }
    }
    let at = anchor.map(|a| format!("#{a}")).unwrap_or_default();
    worklog(
        &st,
        &auth,
        num,
        format!("d{num}{at} commented (m{})", sent.id),
    )
    .await;
    Ok(Json(p::DocCommentResponse {
        id: sent.id,
        thread_id: d.thread_id.unwrap_or(sent.thread_id),
        doc: num,
    }))
}

// --- mail and tasks into documents ---------------------------------------------------

/// Documents of the viewer's org that a thread cites (or whose discussion
/// thread it is), and the comments of the thread on those documents whose
/// discussion thread it is, for thread answers.
pub async fn thread_refs(
    st: &AppState,
    org_id: i64,
    thread_id: i64,
) -> (Vec<p::DocRef>, Vec<p::ThreadDocComment>) {
    let now = amail_store::now_ms();
    let docs = match st.store.thread_docs(org_id, thread_id).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("documents of thread {thread_id}: {e}");
            return (Vec::new(), Vec::new());
        }
    };
    let mut comments = Vec::new();
    for d in docs.iter().filter(|d| d.thread_id == Some(thread_id)) {
        match st.store.doc_detail(org_id, d.num, 0).await {
            Ok(Some(r)) => comments.extend(r.comments.iter().map(|c| p::ThreadDocComment {
                message_id: c.message_id,
                doc: d.num,
                anchor: c.anchor.clone(),
                path: c.path.clone(),
                removed: c.removed,
            })),
            Ok(None) => {}
            Err(e) => tracing::warn!("comments of d{} for thread {thread_id}: {e}", d.num),
        }
    }
    (docs.iter().map(|d| doc_ref(d, now)).collect(), comments)
}

/// Document numbers named as `[d12]` in a subject (at most 10).
pub fn subject_tags(subject: &str) -> Vec<i64> {
    crate::tasks::subject_tags_with(subject, 'd')
}

/// After a message is stored: a reply in a document's discussion thread is
/// a comment of it; a `[d12]` subject links the thread to document 12 of
/// the sender's org. Best effort: errors are logged.
pub async fn after_send(
    st: &AppState,
    org: &Org,
    sender: &Seat,
    written_by: Option<&str>,
    msg: &NewMessage,
    sent: &amail_store::Sent,
) {
    let w = |num: i64| DocWrite {
        org_id: org.id,
        num,
        actor_id: Some(sender.id),
        written_by: written_by.map(str::to_string),
        ..Default::default()
    };
    if msg.thread_id.is_some() {
        match st.store.thread_docs(org.id, sent.thread_id).await {
            Ok(docs) => {
                for d in docs.iter().filter(|d| d.thread_id == Some(sent.thread_id)) {
                    match st
                        .store
                        .record_doc_comment(&w(d.num), sent.message_id, sent.thread_id, None, None)
                        .await
                    {
                        Ok(_) => publish(st, org.id, d.num, "commented", &[]),
                        Err(e) => tracing::warn!("recording a comment of d{}: {e}", d.num),
                    }
                }
            }
            Err(e) => tracing::warn!("documents of thread {}: {e}", sent.thread_id),
        }
    }
    for num in subject_tags(&msg.subject) {
        let note = format!("[d{num}] in the subject of message {}", sent.message_id);
        match st
            .store
            .add_doc_thread(&w(num), sent.thread_id, &note)
            .await
        {
            Ok(true) => publish(st, org.id, num, "link", &[]),
            Ok(false) => {}
            Err(e) => tracing::warn!("linking thread {} to d{num}: {e}", sent.thread_id),
        }
    }
}

/// Link a task to the documents its title names (`d12`, `[d12]`). Best
/// effort: errors are logged.
pub async fn link_task_title(st: &AppState, org_id: i64, actor: Option<i64>, t: &TaskRec) {
    let nums = docmd::doc_refs(&t.title, 10);
    if nums.is_empty() {
        return;
    }
    let w = DocWrite {
        org_id,
        actor_id: actor,
        ..Default::default()
    };
    match st.store.link_task_docs(&w, t.id, t.num, &nums).await {
        Ok(v) => {
            for n in v {
                publish(st, org_id, n, "link", &[]);
            }
        }
        Err(e) => tracing::warn!("linking t{} to documents: {e}", t.num),
    }
}

// --- reminders -----------------------------------------------------------------------

/// The document reminders of one background pass: expiry (once per
/// confirmation) and dangling links (once a day per document), to the
/// owner. They join the task reminders of the pass, one mail per seat.
pub async fn due_reminders(st: &AppState, now: i64) -> amail_store::Result<Vec<(i64, Reminder)>> {
    let mut out = Vec::new();
    for d in st.store.claim_expired_docs(now, 500).await? {
        let mut r = Reminder::doc(
            d.owner_id,
            d.owner.clone(),
            "expired",
            &d,
            format!("d{} is due to be confirmed: {}", d.num, d.title),
        );
        r.extra = Some(format!(
            "\nd{n} passed its expiry ({}). Read it, then confirm that it is still current:\n    amail doc show d{n}\n    amail doc verify d{n}\n\
             When it is no longer true, supersede or archive it.\n",
            d.expires_at.map(render::ts).unwrap_or_default(),
            n = d.num
        ));
        out.push((d.org_id, r));
    }
    for (d, links) in st.store.claim_dangling_docs(now, 500).await? {
        let names: Vec<String> = links
            .iter()
            .map(|l| match &l.anchor {
                Some(a) => format!("d{}#{a}", l.id),
                None => format!("d{}", l.id),
            })
            .collect();
        let mut r = Reminder::doc(
            d.owner_id,
            d.owner.clone(),
            "dangling",
            &d,
            format!(
                "d{} has {} dangling link{}: {}",
                d.num,
                names.len(),
                if names.len() == 1 { "" } else { "s" },
                names.join(", ")
            ),
        );
        r.extra = Some(format!(
            "\nA dangling link names a document that does not exist or a section it no longer has. \
             Fix the links of d{n} (amail doc write d{n}), or list all with: amail doc check\n",
            n = d.num
        ));
        out.push((d.org_id, r));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_and_intervals() {
        assert_eq!(as_num("d12"), Some(12));
        assert_eq!(as_num("12"), Some(12));
        assert_eq!(as_num("d"), None);
        assert_eq!(as_num("deploy"), None);
        assert_eq!(parse_interval("30d").unwrap(), Some(30 * 86_400_000));
        assert_eq!(parse_interval("2w").unwrap(), Some(14 * 86_400_000));
        assert_eq!(parse_interval("12h").unwrap(), Some(12 * 3_600_000));
        assert_eq!(parse_interval("none").unwrap(), None);
        assert!(parse_interval("x").is_err());
        assert!(check_slug("runbook.deploy").is_ok());
        assert!(check_slug("d12").is_err() && check_slug("Bad").is_err());
    }

    #[test]
    fn snippets() {
        let t = "## Rollback\n\nFirst do this. Run the rollback script now. Then check.";
        assert_eq!(
            snippet(t, &["ROLLBACK script".into(), "script".into()]),
            "Run the rollback script now."
        );
        // A heading's attribute is not shown; a period inside a code span
        // or a word does not end the sentence.
        assert_eq!(
            snippet(
                "## Rounding {#rounding}\n\nOther text.",
                &["rounding".into()]
            ),
            "Rounding"
        );
        assert_eq!(
            snippet(
                "Tests may assert `format_cents(-1250) == \"-12.50\"` in tests/. Then more.",
                &["tests".into()]
            ),
            "Tests may assert `format_cents(-1250) == \"-12.50\"` in tests/."
        );
        // A cut inside a code span ends before the span.
        let code = format!("{} `{}`", "a ".repeat(90), "x".repeat(100));
        let s = snippet(&code, &["a".into()]);
        assert!(!s.contains('`'), "{s}");
        assert!(s.ends_with("a…"), "{s}");
        let long = format!("{} needle {}", "a".repeat(300), "b".repeat(300));
        assert_eq!(
            snippet(&long, &["needle".into()]).chars().count(),
            DOC_SNIPPET_CHARS
        );
    }

    #[test]
    fn index_budget() {
        let lines: Vec<String> = (1..=10)
            .map(|i| format!("d{i}  runbook  title {i}"))
            .collect();
        let (k, m) = cut_index(&lines, 10, 60, 3000, Some("core"));
        assert_eq!((k.len(), m), (10, None));
        let (k, m) = cut_index(&lines, 10, 3, 3000, Some("core"));
        assert_eq!(k.len(), 3);
        assert_eq!(
            m.as_deref(),
            Some("… and 7 more: amail doc list --team core")
        );
        let (k, m) = cut_index(&lines, 12, 60, 3000, None);
        assert_eq!(k.len(), 10);
        assert_eq!(m.as_deref(), Some("… and 2 more: amail doc list"));
        let (k, m) = cut_index(&lines, 10, 60, 100, None);
        let chars: usize = k.iter().map(|l| l.chars().count() + 1).sum::<usize>()
            + m.as_ref().unwrap().chars().count();
        assert!(chars <= 100, "{chars}");
    }
}
