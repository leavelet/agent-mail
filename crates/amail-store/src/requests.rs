//! Requests for the person (M10): the SQL shared by both backends, as
//! `tasks.rs` does it. [`impl_request_queries!`] generates `mod rq` for one
//! sqlx database type; `impl_store_common!` invokes it and implements the
//! request methods of [`crate::Store`] on top of it.
//!
//! Every write first takes the org's row (`UPDATE orgs ... WHERE id`), as
//! task and document writes do: numbers, the "one answer" rule and the
//! live attention request of a seat are decided on a state no other write
//! changes meanwhile. Requests and their events are charged to the org's
//! `request_bytes`.
//!
//! Format rules (title, body, options) are checked here and refused with
//! [`crate::StoreError::Refused`]; the resource rules of public servers
//! come in [`RequestBounds`].

use amail_proto::{
    RequestClass, RequestEventKind, RequestKind, RequestOption, RequestSource, RequestState,
};

/// The resource rules of requests that a public server configures (see
/// `amail_server::config::RequestLimits`); `None`: no limit.
#[derive(Debug, Clone, Copy, Default)]
pub struct RequestBounds {
    /// Open requests of one org.
    pub open_per_org: Option<i64>,
    /// Requests one seat may make per hour (the user seat and nodes are
    /// not counted).
    pub per_seat_per_hour: Option<i64>,
}

/// A request to create.
#[derive(Debug, Clone)]
pub struct NewRequestRec {
    pub org_id: i64,
    pub kind: RequestKind,
    /// The seat the request is about.
    pub seat_id: i64,
    /// The seat that makes it (`None`: a node).
    pub asker_id: Option<i64>,
    /// The task's number.
    pub task: Option<i64>,
    pub title: String,
    pub body: String,
    pub options: Vec<RequestOption>,
    pub free_text: bool,
    pub default: Option<String>,
    pub class: RequestClass,
    /// When the default is taken (with a default) or the first reminder
    /// is sent; `None`: from the class. Not used for `attention` and
    /// `permission`, which have no deadline.
    pub deadline: Option<i64>,
    pub source: RequestSource,
    /// `attention`: the seat's attention code (one live request per seat
    /// and code: creating another answers the live one).
    pub attention: Option<String>,
    /// The org's storage quota (bytes); `None`: unlimited.
    pub quota: Option<i64>,
    pub bounds: RequestBounds,
}

/// A stored request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestRec {
    pub id: i64,
    pub org_id: i64,
    pub num: i64,
    pub kind: RequestKind,
    pub seat_id: i64,
    pub seat: String,
    pub asker_id: Option<i64>,
    pub asker: Option<String>,
    pub task_id: Option<i64>,
    /// The task's number.
    pub task: Option<i64>,
    pub title: String,
    pub body: String,
    pub options: Vec<RequestOption>,
    pub free_text: bool,
    pub default: Option<String>,
    pub class: RequestClass,
    pub state: RequestState,
    pub deadline: Option<i64>,
    pub remind_at: Option<i64>,
    pub answer_key: Option<String>,
    pub answer_text: Option<String>,
    pub answer_note: Option<String>,
    pub answer_by: Option<String>,
    pub answered_at: Option<i64>,
    pub thread_id: Option<i64>,
    /// The first message of the thread (the mail to the user).
    pub message_id: Option<i64>,
    pub source: RequestSource,
    pub attention: Option<String>,
    pub attention_live: bool,
    /// The last event's seq.
    pub rev: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

impl RequestRec {
    pub fn is_open(&self) -> bool {
        self.state == RequestState::Open
    }

    /// The option with this key.
    pub fn option(&self, key: &str) -> Option<&RequestOption> {
        self.options.iter().find(|o| o.key == key)
    }
}

/// One event of a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestEventRec {
    pub request_id: i64,
    pub seq: i64,
    pub at: i64,
    pub actor: Option<String>,
    pub kind: RequestEventKind,
    pub note: Option<String>,
    pub message_id: Option<i64>,
}

/// An answer to record.
#[derive(Debug, Clone, Default)]
pub struct RequestAnswerRec {
    pub org_id: i64,
    pub num: i64,
    /// The key of the chosen option (the caller resolves numbers and
    /// labels).
    pub option: Option<String>,
    pub text: Option<String>,
    pub note: Option<String>,
    /// The answerer's name.
    pub by: String,
    pub if_rev: Option<i64>,
    /// The message that answered it (a reply in the thread), if any.
    pub message_id: Option<i64>,
    pub quota: Option<i64>,
}

/// A query of [`crate::Store::requests`].
#[derive(Debug, Clone, Default)]
pub struct RequestQuery {
    pub org_id: i64,
    /// Empty: every state.
    pub states: Vec<RequestState>,
    pub kind: Option<RequestKind>,
    /// Requests about this seat.
    pub seat_id: Option<i64>,
    /// Requests about this seat or made by it.
    pub mine: Option<i64>,
    /// Requests about one of these seats or made by one of them (a team's
    /// seats); `Some` and empty matches nothing.
    pub among: Option<Vec<i64>>,
    pub class: Option<RequestClass>,
    /// The task's number.
    pub task: Option<i64>,
    pub limit: i64,
}

/// What the deadline pass found: open requests whose deadline passed and
/// that took their default (`expired`), and open requests without a default
/// that are due a reminder (each marked reminded until its next one).
#[derive(Debug, Clone, Default)]
pub struct RequestsDue {
    pub expired: Vec<RequestRec>,
    pub remind: Vec<RequestRec>,
}

/// Check the format rules of a new request (refused with a code).
pub(crate) fn check_new(n: &NewRequestRec) -> crate::Result<()> {
    use crate::tasks::refused;
    use amail_proto::*;
    let title = n.title.trim();
    if title.is_empty() {
        return Err(refused("title_empty", "a request needs a title"));
    }
    if title.contains(['\n', '\r']) {
        return Err(refused("invalid_title", "the title must be a single line"));
    }
    if title.chars().count() > MAX_REQUEST_TITLE_CHARS {
        return Err(refused(
            "title_too_long",
            format!("the title is limited to {MAX_REQUEST_TITLE_CHARS} characters"),
        ));
    }
    if n.body.len() > MAX_REQUEST_BODY_BYTES {
        return Err(refused(
            "body_too_large",
            format!(
                "the body has {} bytes; the limit is {MAX_REQUEST_BODY_BYTES}",
                n.body.len()
            ),
        ));
    }
    if n.free_text && !matches!(n.kind, RequestKind::Question | RequestKind::Plan) {
        return Err(refused(
            "free_text_not_allowed",
            format!("a {} request takes one of its options", n.kind),
        ));
    }
    let count = n.options.len();
    let ok = if count == 0 {
        n.free_text
    } else {
        (MIN_REQUEST_OPTIONS..=MAX_REQUEST_OPTIONS).contains(&count)
    };
    if !ok {
        return Err(refused(
            "options_count",
            format!(
                "a request has {MIN_REQUEST_OPTIONS} to {MAX_REQUEST_OPTIONS} options, \
                 or none with free_text ({count} given)"
            ),
        ));
    }
    let mut keys: Vec<&str> = Vec::new();
    for o in &n.options {
        if !valid_option_key(&o.key) {
            return Err(refused(
                "invalid_option_key",
                format!(
                    "option key {:?}: 1 to {MAX_REQUEST_OPTION_KEY_CHARS} characters of A-Z, a-z, 0-9, \
                     '_', '-', '.', and not digits only",
                    o.key
                ),
            ));
        }
        if keys.iter().any(|k| k.eq_ignore_ascii_case(&o.key)) {
            return Err(refused(
                "duplicate_option",
                format!("option key {:?} is given twice", o.key),
            ));
        }
        keys.push(&o.key);
        let label = o.label.trim();
        if label.is_empty() || label.contains(['\n', '\r']) {
            return Err(refused(
                "invalid_option_label",
                format!("option {:?} needs a label of one line", o.key),
            ));
        }
        if label.chars().count() > MAX_REQUEST_OPTION_LABEL_CHARS {
            return Err(refused(
                "option_label_too_long",
                format!("option labels are limited to {MAX_REQUEST_OPTION_LABEL_CHARS} characters"),
            ));
        }
        if o.description
            .as_deref()
            .is_some_and(|d| d.chars().count() > MAX_REQUEST_OPTION_DESCRIPTION_CHARS)
        {
            return Err(refused(
                "option_description_too_long",
                format!(
                    "option descriptions are limited to {MAX_REQUEST_OPTION_DESCRIPTION_CHARS} characters"
                ),
            ));
        }
    }
    if let Some(d) = &n.default
        && !n.options.iter().any(|o| &o.key == d)
    {
        return Err(refused(
            "invalid_default",
            format!("the default {d:?} is not the key of an option"),
        ));
    }
    Ok(())
}

/// The bytes a request's texts count against the storage quota.
pub(crate) fn request_bytes(
    title: &str,
    body: &str,
    options: &str,
    attention: Option<&str>,
) -> i64 {
    (title.len() + body.len() + options.len() + attention.map_or(0, str::len)) as i64
        + amail_proto::STORED_BYTES_PER_REQUEST
}

/// Generates `mod rq`: the request queries for one sqlx database type.
macro_rules! impl_request_queries {
    ($db:ty) => {
        #[allow(dead_code)]
        pub(crate) mod rq {
            use $crate::requests::*;
            use $crate::tasks::refused;
            use $crate::{now_ms, Result, StoreError};
            use amail_proto::*;

            pub(crate) type Conn = <$db as sqlx::Database>::Connection;
            type Db = $db;

            const COLS: &str = "r.id, r.org_id, r.num, r.kind, r.seat_id, s.name AS seat, r.asker_id, \
                 a.name AS asker, r.task_id, t.num AS task_num, r.title, r.body, r.options, r.free_text, \
                 r.default_key, r.class, r.state, r.deadline, r.remind_at, r.answer_key, r.answer_text, \
                 r.answer_note, r.answer_by, r.answered_at, r.thread_id, r.message_id, r.source, \
                 r.attention, r.attention_live, r.rev, r.created_at, r.updated_at \
                 FROM requests r JOIN seats s ON s.id = r.seat_id LEFT JOIN seats a ON a.id = r.asker_id \
                 LEFT JOIN tasks t ON t.id = r.task_id";

            #[derive(sqlx::FromRow)]
            struct Row {
                id: i64,
                org_id: i64,
                num: i64,
                kind: String,
                seat_id: i64,
                seat: String,
                asker_id: Option<i64>,
                asker: Option<String>,
                task_id: Option<i64>,
                task_num: Option<i64>,
                title: String,
                body: String,
                options: String,
                free_text: bool,
                default_key: Option<String>,
                class: String,
                state: String,
                deadline: Option<i64>,
                remind_at: Option<i64>,
                answer_key: Option<String>,
                answer_text: Option<String>,
                answer_note: Option<String>,
                answer_by: Option<String>,
                answered_at: Option<i64>,
                thread_id: Option<i64>,
                message_id: Option<i64>,
                source: String,
                attention: Option<String>,
                attention_live: bool,
                rev: i64,
                created_at: i64,
                updated_at: i64,
            }

            impl Row {
                fn into_rec(self) -> Result<RequestRec> {
                    Ok(RequestRec {
                        id: self.id,
                        org_id: self.org_id,
                        num: self.num,
                        kind: $crate::common::parse(&self.kind)?,
                        seat_id: self.seat_id,
                        seat: self.seat,
                        asker_id: self.asker_id,
                        asker: self.asker,
                        task_id: self.task_id,
                        task: self.task_num,
                        title: self.title,
                        body: self.body,
                        options: serde_json::from_str(&self.options)
                            .map_err(|e| StoreError::Invalid(format!("corrupt row: {e}")))?,
                        free_text: self.free_text,
                        default: self.default_key,
                        class: $crate::common::parse(&self.class)?,
                        state: $crate::common::parse(&self.state)?,
                        deadline: self.deadline,
                        remind_at: self.remind_at,
                        answer_key: self.answer_key,
                        answer_text: self.answer_text,
                        answer_note: self.answer_note,
                        answer_by: self.answer_by,
                        answered_at: self.answered_at,
                        thread_id: self.thread_id,
                        message_id: self.message_id,
                        source: $crate::common::parse(&self.source)?,
                        attention: self.attention,
                        attention_live: self.attention_live,
                        rev: self.rev,
                        created_at: self.created_at,
                        updated_at: self.updated_at,
                    })
                }
            }

            /// Take the org's row for this write (see the module docs).
            async fn lock(c: &mut Conn, org_id: i64) -> Result<()> {
                let n = sqlx::query("UPDATE orgs SET org_request_seq = org_request_seq WHERE id = $1 AND deleted_at IS NULL")
                    .bind(org_id)
                    .execute(&mut *c)
                    .await?
                    .rows_affected();
                if n == 0 {
                    return Err(StoreError::NotFound("org".into()));
                }
                Ok(())
            }

            /// Add `delta` to the org's request bytes; growth is refused
            /// over `quota`.
            async fn charge(c: &mut Conn, org_id: i64, delta: i64, quota: Option<i64>) -> Result<()> {
                if delta == 0 {
                    return Ok(());
                }
                let ok: Option<i64> = sqlx::query_scalar(
                    "UPDATE orgs SET request_bytes = request_bytes + $1 WHERE id = $2 \
                     AND ($1 <= 0 OR CAST($3 AS BIGINT) IS NULL OR stored_bytes + task_bytes + doc_bytes + request_bytes + $1 <= $3) RETURNING id",
                )
                .bind(delta)
                .bind(org_id)
                .bind(quota)
                .fetch_optional(&mut *c)
                .await?;
                if ok.is_none() {
                    let stored: i64 = sqlx::query_scalar(
                        "SELECT stored_bytes + task_bytes + doc_bytes + request_bytes FROM orgs WHERE id = $1",
                    )
                    .bind(org_id)
                    .fetch_one(&mut *c)
                    .await?;
                    return Err(StoreError::StorageQuota(stored));
                }
                Ok(())
            }

            /// Append an event; returns the bytes it adds (also to the
            /// request's own count).
            async fn event(
                c: &mut Conn,
                r: &RequestRec,
                at: i64,
                actor: Option<&str>,
                kind: RequestEventKind,
                note: Option<&str>,
                message_id: Option<i64>,
            ) -> Result<i64> {
                let seq: i64 = sqlx::query_scalar("UPDATE requests SET rev = rev + 1, updated_at = $2 WHERE id = $1 RETURNING rev")
                    .bind(r.id)
                    .bind(at)
                    .fetch_one(&mut *c)
                    .await?;
                let bytes = STORED_BYTES_PER_REQUEST_EVENT
                    + (note.map_or(0, str::len) + actor.map_or(0, str::len)) as i64;
                sqlx::query(
                    "INSERT INTO request_events (request_id, seq, org_id, at, actor, kind, note, message_id) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
                )
                .bind(r.id)
                .bind(seq)
                .bind(r.org_id)
                .bind(at)
                .bind(actor)
                .bind(kind.as_str())
                .bind(note)
                .bind(message_id)
                .execute(&mut *c)
                .await?;
                sqlx::query("UPDATE requests SET bytes = bytes + $1 WHERE id = $2")
                    .bind(bytes)
                    .bind(r.id)
                    .execute(&mut *c)
                    .await?;
                Ok(bytes)
            }

            pub async fn by_num(c: &mut Conn, org_id: i64, num: i64) -> Result<Option<RequestRec>> {
                let r: Option<Row> = sqlx::query_as(&format!("SELECT {COLS} WHERE r.org_id = $1 AND r.num = $2"))
                    .bind(org_id)
                    .bind(num)
                    .fetch_optional(&mut *c)
                    .await?;
                r.map(Row::into_rec).transpose()
            }

            async fn must(c: &mut Conn, org_id: i64, num: i64) -> Result<RequestRec> {
                by_num(c, org_id, num)
                    .await?
                    .ok_or_else(|| StoreError::NotFound(format!("no request r{num}")))
            }

            /// Create a request; an `attention` request for a seat and code
            /// that has a live one answers that one (true: it existed).
            pub async fn create(c: &mut Conn, n: &NewRequestRec) -> Result<(RequestRec, bool)> {
                check_new(n)?;
                lock(c, n.org_id).await?;
                let seat_org: Option<i64> = sqlx::query_scalar("SELECT org_id FROM seats WHERE id = $1 AND deleted_at IS NULL")
                    .bind(n.seat_id)
                    .fetch_optional(&mut *c)
                    .await?;
                if seat_org != Some(n.org_id) {
                    return Err(StoreError::NotFound("seat".into()));
                }
                if let Some(code) = &n.attention {
                    let live: Option<i64> = sqlx::query_scalar(
                        "SELECT num FROM requests WHERE org_id = $1 AND seat_id = $2 AND attention_live \
                         AND attention = $3 ORDER BY num DESC LIMIT 1",
                    )
                    .bind(n.org_id)
                    .bind(n.seat_id)
                    .bind(code)
                    .fetch_optional(&mut *c)
                    .await?;
                    if let Some(num) = live {
                        return Ok((must(c, n.org_id, num).await?, true));
                    }
                }
                let now = now_ms();
                if let Some(max) = n.bounds.open_per_org {
                    let open: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM requests WHERE org_id = $1 AND state = 'open'")
                        .bind(n.org_id)
                        .fetch_one(&mut *c)
                        .await?;
                    if open >= max {
                        return Err(refused(
                            "too_many_open_requests",
                            format!("an org has at most {max} open requests on this server; answer or withdraw some"),
                        ));
                    }
                }
                if let (Some(max), Some(asker)) = (n.bounds.per_seat_per_hour, n.asker_id) {
                    let made: i64 = sqlx::query_scalar(
                        "SELECT COUNT(*) FROM requests WHERE asker_id = $1 AND created_at > $2",
                    )
                    .bind(asker)
                    .bind(now - 3_600_000)
                    .fetch_one(&mut *c)
                    .await?;
                    if made >= max {
                        return Err(refused(
                            "request_rate",
                            format!("a seat makes at most {max} requests per hour on this server"),
                        ));
                    }
                }
                let task_id: Option<i64> = match n.task {
                    Some(t) => Some(
                        sqlx::query_scalar("SELECT id FROM tasks WHERE org_id = $1 AND num = $2")
                            .bind(n.org_id)
                            .bind(t)
                            .fetch_optional(&mut *c)
                            .await?
                            .ok_or_else(|| StoreError::NotFound(format!("no task t{t}")))?,
                    ),
                    None => None,
                };
                let num: i64 = sqlx::query_scalar(
                    "UPDATE orgs SET org_request_seq = org_request_seq + 1 WHERE id = $1 RETURNING org_request_seq",
                )
                .bind(n.org_id)
                .fetch_one(&mut *c)
                .await?;
                let options = serde_json::to_string(&n.options).unwrap_or_else(|_| "[]".into());
                let title = n.title.trim();
                // `attention` and `permission` requests have no deadline:
                // the node's report (or the hook's hold) ends them.
                let timed = !matches!(n.kind, RequestKind::Attention | RequestKind::Permission);
                let deadline = timed.then(|| n.deadline.unwrap_or(now + n.class.deadline_ms()));
                let remind_at = if n.default.is_none() { deadline } else { None };
                let bytes = request_bytes(title, &n.body, &options, n.attention.as_deref());
                charge(c, n.org_id, bytes, n.quota).await?;
                let id: i64 = sqlx::query_scalar(
                    "INSERT INTO requests (org_id, num, kind, seat_id, asker_id, task_id, title, body, options, \
                     free_text, default_key, class, state, deadline, remind_at, source, attention, attention_live, \
                     bytes, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, 'open', \
                     $13, $14, $15, $16, $17, $18, $19, $19) RETURNING id",
                )
                .bind(n.org_id)
                .bind(num)
                .bind(n.kind.as_str())
                .bind(n.seat_id)
                .bind(n.asker_id)
                .bind(task_id)
                .bind(title)
                .bind(&n.body)
                .bind(&options)
                .bind(n.free_text)
                .bind(&n.default)
                .bind(n.class.as_str())
                .bind(deadline)
                .bind(remind_at)
                .bind(n.source.as_str())
                .bind(&n.attention)
                .bind(n.attention.is_some())
                .bind(bytes)
                .bind(now)
                .fetch_one(&mut *c)
                .await?;
                let _ = id;
                let r = must(c, n.org_id, num).await?;
                let actor = r.asker.clone().unwrap_or_else(|| "node".into());
                let b = event(c, &r, now, Some(&actor), RequestEventKind::Created, None, None).await?;
                charge(c, n.org_id, b, n.quota).await?;
                Ok((must(c, n.org_id, num).await?, false))
            }

            /// Record the request's thread and its first message.
            pub async fn set_thread(c: &mut Conn, org_id: i64, num: i64, thread_id: i64, message_id: i64) -> Result<()> {
                sqlx::query("UPDATE requests SET thread_id = $3, message_id = $4 WHERE org_id = $1 AND num = $2")
                    .bind(org_id)
                    .bind(num)
                    .bind(thread_id)
                    .bind(message_id)
                    .execute(&mut *c)
                    .await?;
                Ok(())
            }

            /// Record an answer (see [`crate::Store::answer_request`]).
            pub async fn answer(c: &mut Conn, a: &RequestAnswerRec) -> Result<RequestRec> {
                lock(c, a.org_id).await?;
                let r = must(c, a.org_id, a.num).await?;
                match r.state {
                    RequestState::Open => {}
                    RequestState::Answered | RequestState::Expired => {
                        return Err(refused(
                            "answered",
                            format!(
                                "r{} was answered by {} already",
                                r.num,
                                r.answer_by.as_deref().unwrap_or("someone")
                            ),
                        ));
                    }
                    s => return Err(refused("not_open", format!("r{} is {s}", r.num))),
                }
                if let Some(want) = a.if_rev
                    && want != r.rev
                {
                    return Err(refused("stale", format!("r{} changed since rev {want}: it is at rev {}", r.num, r.rev)));
                }
                let text = a.text.as_deref().map(str::trim).filter(|t| !t.is_empty());
                match (&a.option, text) {
                    (Some(k), _) if r.option(k).is_none() => {
                        return Err(refused("invalid_option", format!("r{} has no option {k:?}", r.num)));
                    }
                    (None, Some(_)) if !r.free_text => {
                        return Err(refused(
                            "free_text_not_allowed",
                            format!("r{} takes one of its options", r.num),
                        ));
                    }
                    (None, None) => {
                        return Err(refused("answer_required", "give an option or a text"));
                    }
                    _ => {}
                }
                if text.is_some_and(|t| t.chars().count() > MAX_REQUEST_ANSWER_CHARS) {
                    return Err(refused(
                        "answer_too_long",
                        format!("an answer is limited to {MAX_REQUEST_ANSWER_CHARS} characters"),
                    ));
                }
                let note = a.note.as_deref().map(str::trim).filter(|t| !t.is_empty());
                if note.is_some_and(|t| t.chars().count() > MAX_REQUEST_NOTE_CHARS) {
                    return Err(refused(
                        "note_too_long",
                        format!("a note is limited to {MAX_REQUEST_NOTE_CHARS} characters"),
                    ));
                }
                let now = now_ms();
                sqlx::query(
                    "UPDATE requests SET state = 'answered', answer_key = $2, answer_text = $3, answer_note = $4, \
                     answer_by = $5, answered_at = $6, remind_at = NULL, bytes = bytes + $7 WHERE id = $1",
                )
                .bind(r.id)
                .bind(&a.option)
                .bind(text)
                .bind(note)
                .bind(&a.by)
                .bind(now)
                .bind((text.map_or(0, str::len) + note.map_or(0, str::len)) as i64)
                .execute(&mut *c)
                .await?;
                let what = match (&a.option, text) {
                    (Some(k), _) => k.clone(),
                    (None, Some(t)) => cut_chars(t, 200),
                    _ => String::new(),
                };
                let b = event(c, &r, now, Some(&a.by), RequestEventKind::Answered, Some(&what), a.message_id).await?;
                charge(c, a.org_id, b + (text.map_or(0, str::len) + note.map_or(0, str::len)) as i64, a.quota).await?;
                must(c, a.org_id, a.num).await
            }

            /// Withdraw an open request (see [`crate::Store::withdraw_request`]).
            pub async fn withdraw(c: &mut Conn, org_id: i64, num: i64, actor: &str, note: Option<&str>) -> Result<RequestRec> {
                lock(c, org_id).await?;
                let r = must(c, org_id, num).await?;
                if !r.is_open() {
                    return Err(refused("not_open", format!("r{} is {}", r.num, r.state)));
                }
                let now = now_ms();
                sqlx::query("UPDATE requests SET state = 'withdrawn', remind_at = NULL WHERE id = $1")
                    .bind(r.id)
                    .execute(&mut *c)
                    .await?;
                let b = event(c, &r, now, Some(actor), RequestEventKind::Withdrawn, note, None).await?;
                charge(c, org_id, b, None).await?;
                must(c, org_id, num).await
            }

            /// Record an event that changes nothing else (a comment, the
            /// creation mail); `None` when there is no such request.
            pub async fn note(
                c: &mut Conn,
                org_id: i64,
                num: i64,
                actor: Option<&str>,
                kind: RequestEventKind,
                note: Option<&str>,
                message_id: Option<i64>,
            ) -> Result<Option<RequestRec>> {
                lock(c, org_id).await?;
                let Some(r) = by_num(c, org_id, num).await? else {
                    return Ok(None);
                };
                let b = event(c, &r, now_ms(), actor, kind, note, message_id).await?;
                charge(c, org_id, b, None).await?;
                by_num(c, org_id, num).await
            }

            /// Link a thread that names the request; false when it was
            /// linked, is the request's own thread, or there is no such
            /// request.
            pub async fn link_thread(c: &mut Conn, org_id: i64, num: i64, thread_id: i64, actor: Option<&str>, note: &str) -> Result<bool> {
                lock(c, org_id).await?;
                let Some(r) = by_num(c, org_id, num).await? else {
                    return Ok(false);
                };
                if r.thread_id == Some(thread_id) {
                    return Ok(false);
                }
                let now = now_ms();
                let n = sqlx::query(
                    "INSERT INTO request_threads (request_id, thread_id, org_id, created_at) VALUES ($1, $2, $3, $4) \
                     ON CONFLICT (request_id, thread_id) DO NOTHING",
                )
                .bind(r.id)
                .bind(thread_id)
                .bind(org_id)
                .bind(now)
                .execute(&mut *c)
                .await?
                .rows_affected();
                if n == 0 {
                    return Ok(false);
                }
                let b = event(c, &r, now, actor, RequestEventKind::Linked, Some(note), None).await?;
                charge(c, org_id, b + 48, None).await?;
                Ok(true)
            }

            pub async fn threads(c: &mut Conn, org_id: i64, num: i64) -> Result<Vec<i64>> {
                Ok(sqlx::query_scalar(
                    "SELECT rt.thread_id FROM request_threads rt JOIN requests r ON r.id = rt.request_id \
                     WHERE r.org_id = $1 AND r.num = $2 ORDER BY rt.created_at",
                )
                .bind(org_id)
                .bind(num)
                .fetch_all(&mut *c)
                .await?)
            }

            /// Requests whose own thread this is.
            pub async fn thread_requests(c: &mut Conn, org_id: i64, thread_id: i64) -> Result<Vec<RequestRec>> {
                let rows: Vec<Row> = sqlx::query_as(&format!(
                    "SELECT {COLS} WHERE r.org_id = $1 AND r.thread_id = $2 ORDER BY r.num"
                ))
                .bind(org_id)
                .bind(thread_id)
                .fetch_all(&mut *c)
                .await?;
                rows.into_iter().map(Row::into_rec).collect()
            }

            pub async fn events(c: &mut Conn, org_id: i64, num: i64, limit: i64) -> Result<Vec<RequestEventRec>> {
                let rows: Vec<(i64, i64, i64, Option<String>, String, Option<String>, Option<i64>)> = sqlx::query_as(
                    "SELECT e.request_id, e.seq, e.at, e.actor, e.kind, e.note, e.message_id FROM request_events e \
                     JOIN requests r ON r.id = e.request_id WHERE r.org_id = $1 AND r.num = $2 \
                     ORDER BY e.seq DESC LIMIT $3",
                )
                .bind(org_id)
                .bind(num)
                .bind(limit)
                .fetch_all(&mut *c)
                .await?;
                let mut out = Vec::with_capacity(rows.len());
                for (request_id, seq, at, actor, kind, note, message_id) in rows.into_iter().rev() {
                    out.push(RequestEventRec {
                        request_id,
                        seq,
                        at,
                        actor,
                        kind: $crate::common::parse(&kind)?,
                        note,
                        message_id,
                    });
                }
                Ok(out)
            }

            /// Requests by query: open ones first, `now` before `soon`
            /// before `later`, oldest first; then the others, newest first.
            /// True when cut at the limit.
            pub async fn list(c: &mut Conn, q: &RequestQuery) -> Result<(Vec<RequestRec>, bool)> {
                let limit = q.limit.clamp(1, MAX_REQUEST_PAGE);
                let mut b = sqlx::QueryBuilder::<Db>::new(format!("SELECT {COLS} WHERE r.org_id = "));
                b.push_bind(q.org_id);
                if !q.states.is_empty() {
                    b.push(" AND r.state IN (");
                    let mut sep = b.separated(", ");
                    for s in &q.states {
                        sep.push_bind(s.as_str());
                    }
                    b.push(")");
                }
                if let Some(k) = q.kind {
                    b.push(" AND r.kind = ").push_bind(k.as_str());
                }
                if let Some(s) = q.seat_id {
                    b.push(" AND r.seat_id = ").push_bind(s);
                }
                if let Some(m) = q.mine {
                    b.push(" AND (r.seat_id = ").push_bind(m);
                    b.push(" OR r.asker_id = ").push_bind(m);
                    b.push(")");
                }
                if let Some(ids) = &q.among {
                    if ids.is_empty() {
                        b.push(" AND 1 = 0");
                    } else {
                        for col in [" AND (r.seat_id IN (", ") OR r.asker_id IN ("] {
                            b.push(col);
                            let mut sep = b.separated(", ");
                            for id in ids {
                                sep.push_bind(*id);
                            }
                        }
                        b.push("))");
                    }
                }
                if let Some(cl) = q.class {
                    b.push(" AND r.class = ").push_bind(cl.as_str());
                }
                if let Some(t) = q.task {
                    b.push(" AND t.num = ").push_bind(t);
                }
                b.push(
                    " ORDER BY CASE WHEN r.state = 'open' THEN 0 ELSE 1 END, \
                     CASE WHEN r.state <> 'open' THEN 0 WHEN r.class = 'now' THEN 0 WHEN r.class = 'soon' THEN 1 ELSE 2 END, \
                     CASE WHEN r.state = 'open' THEN r.created_at ELSE -r.num END, r.num LIMIT ",
                );
                b.push_bind(limit + 1);
                let rows: Vec<Row> = b.build_query_as().fetch_all(&mut *c).await?;
                let mut out = rows.into_iter().map(Row::into_rec).collect::<Result<Vec<_>>>()?;
                let truncated = out.len() as i64 > limit;
                out.truncate(limit as usize);
                Ok((out, truncated))
            }

            /// The deadline pass: open requests with a default whose
            /// deadline passed take it (`expired`, answered by `deadline`);
            /// open requests without a default due a reminder are marked
            /// reminded until their next one. At most `limit` of each; each
            /// once, also with several instances.
            pub async fn claim_due(c: &mut Conn, now: i64, limit: i64) -> Result<RequestsDue> {
                let mut due = RequestsDue::default();
                let rows: Vec<(i64, i64)> = sqlx::query_as(
                    "SELECT org_id, num FROM requests WHERE state = 'open' AND default_key IS NOT NULL \
                     AND deadline IS NOT NULL AND deadline <= $1 ORDER BY deadline LIMIT $2",
                )
                .bind(now)
                .bind(limit)
                .fetch_all(&mut *c)
                .await?;
                for (org_id, num) in rows {
                    let r = must(c, org_id, num).await?;
                    let n = sqlx::query(
                        "UPDATE requests SET state = 'expired', answer_key = default_key, answer_by = 'deadline', \
                         answered_at = $2, remind_at = NULL WHERE id = $1 AND state = 'open'",
                    )
                    .bind(r.id)
                    .bind(now)
                    .execute(&mut *c)
                    .await?
                    .rows_affected();
                    if n == 0 {
                        continue;
                    }
                    let b = event(c, &r, now, Some("deadline"), RequestEventKind::Expired, r.default.as_deref(), None).await?;
                    charge(c, org_id, b, None).await?;
                    due.expired.push(must(c, org_id, num).await?);
                }
                let rows: Vec<(i64, i64, i64)> = sqlx::query_as(
                    "SELECT org_id, num, remind_at FROM requests WHERE state = 'open' AND remind_at IS NOT NULL \
                     AND remind_at <= $1 ORDER BY remind_at LIMIT $2",
                )
                .bind(now)
                .bind(limit)
                .fetch_all(&mut *c)
                .await?;
                for (org_id, num, at) in rows {
                    let r = must(c, org_id, num).await?;
                    let next = now + r.class.remind_every_ms();
                    let n = sqlx::query("UPDATE requests SET remind_at = $2 WHERE id = $1 AND state = 'open' AND remind_at = $3")
                        .bind(r.id)
                        .bind(next)
                        .bind(at)
                        .execute(&mut *c)
                        .await?
                        .rows_affected();
                    if n == 0 {
                        continue;
                    }
                    let b = event(c, &r, now, None, RequestEventKind::Reminded, None, None).await?;
                    charge(c, org_id, b, None).await?;
                    due.remind.push(must(c, org_id, num).await?);
                }
                Ok(due)
            }

            /// The org's `attention` requests whose condition the node
            /// still reports.
            pub async fn live_attention(c: &mut Conn, org_id: i64) -> Result<Vec<RequestRec>> {
                let rows: Vec<Row> = sqlx::query_as(&format!(
                    "SELECT {COLS} WHERE r.org_id = $1 AND r.attention_live ORDER BY r.num"
                ))
                .bind(org_id)
                .fetch_all(&mut *c)
                .await?;
                rows.into_iter().map(Row::into_rec).collect()
            }

            /// The node reports the condition of an `attention` request
            /// gone: it is no longer live, and if open, it is withdrawn
            /// (by `node`). The request as it is now; `None`: no such live
            /// request.
            pub async fn end_attention(c: &mut Conn, org_id: i64, num: i64) -> Result<Option<RequestRec>> {
                lock(c, org_id).await?;
                let Some(r) = by_num(c, org_id, num).await?.filter(|r| r.attention_live) else {
                    return Ok(None);
                };
                sqlx::query("UPDATE requests SET attention_live = FALSE WHERE id = $1")
                    .bind(r.id)
                    .execute(&mut *c)
                    .await?;
                if r.is_open() {
                    sqlx::query("UPDATE requests SET state = 'withdrawn' WHERE id = $1")
                        .bind(r.id)
                        .execute(&mut *c)
                        .await?;
                    let b = event(
                        c,
                        &r,
                        now_ms(),
                        Some("node"),
                        RequestEventKind::Withdrawn,
                        Some("the node reports the condition gone"),
                        None,
                    )
                    .await?;
                    charge(c, org_id, b, None).await?;
                }
                by_num(c, org_id, num).await
            }

            /// Delete the org's requests (the org is being deleted).
            pub async fn delete_org(c: &mut Conn, org_id: i64) -> Result<()> {
                for sql in [
                    "DELETE FROM request_threads WHERE org_id = $1",
                    "DELETE FROM request_events WHERE org_id = $1",
                    "DELETE FROM requests WHERE org_id = $1",
                ] {
                    sqlx::query(sql).bind(org_id).execute(&mut *c).await?;
                }
                Ok(())
            }
        }
    };
}
