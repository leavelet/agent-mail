//! SQL shared by both backends.
//!
//! The two schemas are identical and the queries below are portable between
//! SQLite and Postgres (`$n` placeholders, `RETURNING`, `ON CONFLICT`, partial
//! indexes). [`impl_store_common!`] instantiates them for one sqlx database
//! type and implements [`crate::Store`] on top of three hooks each backend
//! provides as inherent methods:
//!
//! - `read(f)`: run `f` on a connection outside any write transaction;
//! - `write(f)`: run `f` inside a write transaction;
//! - `send_impl` / `claim_impl`: the two hot paths, which differ per backend.

use crate::model::*;
use crate::{Result, StoreError};
use amail_proto::{
    DeliveryClass, DeliveryMode, LinkState, Meta, PermissionMode, PostPolicy, Priority, SeatKind,
};
use std::str::FromStr;

// Column lists and joins, as macros so they can be `concat!`ed into
// `&'static str` queries.

/// The bytes of message `m` counted against the storage quota without its
/// deliveries (see `message_bytes`); `$2` is `STORED_BYTES_PER_MESSAGE`.
macro_rules! message_bytes_sql {
    () => {
        "($2 + octet_length(m.subject) + octet_length(m.body) + octet_length(m.meta) \
         + octet_length(m.to_addrs) + octet_length(m.cc_addrs))"
    };
}

macro_rules! seat_select {
    () => {
        "SELECT s.id, s.org_id, s.name, s.kind, s.parent_id, p.name AS parent_name, \
         s.display_name, s.role, s.harness, s.model, s.permission_mode, s.group_name, s.last_seq, \
         s.created_at, s.deleted_at, s.focus_until \
         FROM seats s LEFT JOIN seats p ON p.id = s.parent_id"
    };
}

macro_rules! list_select {
    () => {
        "SELECT l.id, l.org_id, l.name, l.description, l.post_policy, l.managed, \
         (SELECT COUNT(*) FROM list_members lm WHERE lm.list_id = l.id) AS member_count, \
         l.created_at FROM lists l"
    };
}

/// A deleted org's mail is shown as from `name@<old name> (deleted org)`.
macro_rules! msg_cols {
    () => {
        "m.id, m.org_id, CASE WHEN mo.deleted_at IS NULL THEN mo.name \
         ELSE mo.deleted_name || ' (deleted org)' END AS org_name, m.sender_seat_id, ms.name AS sender_name, \
         ms.kind AS sender_kind, m.thread_id, m.parent_id, m.depth, m.subject, m.lines, \
         m.to_addrs, m.cc_addrs, m.priority, m.ack_required, m.meta, m.created_at, m.expired_at"
    };
}

macro_rules! msg_joins {
    () => {
        " JOIN orgs mo ON mo.id = m.org_id JOIN seats ms ON ms.id = m.sender_seat_id"
    };
}

macro_rules! deliv_cols {
    () => {
        "d.id AS d_id, d.org_id AS d_org_id, dorg.name AS d_org_name, d.message_id, d.seat_id, \
         ds.name AS seat_name, d.class, d.mode, d.seq, d.mod_seq, d.notified_at, \
         d.notified_via, d.read_at, d.acked_at"
    };
}

macro_rules! deliv_joins {
    () => {
        " JOIN seats ds ON ds.id = d.seat_id JOIN orgs dorg ON dorg.id = d.org_id"
    };
}

/// `SELECT` of delivery + message header (body left empty).
macro_rules! entry_select {
    () => {
        concat!(
            "SELECT ",
            deliv_cols!(),
            ", ",
            msg_cols!(),
            ", '' AS body FROM deliveries d",
            deliv_joins!(),
            " JOIN messages m ON m.id = d.message_id",
            msg_joins!()
        )
    };
}

/// `SELECT` of delivery + message with the start of its body (claims):
/// one character more than `amail_proto::limits::MAX_CLAIM_BODY_CHARS`, so
/// that a longer body is known to be longer without loading all of it.
macro_rules! entry_select_body {
    () => {
        concat!(
            "SELECT ",
            deliv_cols!(),
            ", ",
            msg_cols!(),
            ", substr(m.body, 1, 16385) AS body FROM deliveries d",
            deliv_joins!(),
            " JOIN messages m ON m.id = d.message_id",
            msg_joins!()
        )
    };
}

/// Candidate deliveries for a claim. Postgres appends a row-locking clause.
/// Binds: seat_id, mode mask (1 immediate, 2 digest), to, cc, list,
/// urgent_only, limit, and an idempotency key: when the seat already used
/// that key, nothing is a candidate (SQLite; Postgres binds NULL and checks
/// the key first). The seat's counters are checked first (`claim_gate!`;
/// Postgres evaluates it once, before reading deliveries), so a claim of a
/// kind of mail the seat has none of pending reads no deliveries.
macro_rules! claim_select {
    () => {
        concat!(
            "SELECT d.id FROM deliveries d JOIN messages m ON m.id = d.message_id \
         WHERE d.seat_id = $1 AND d.notified_at IS NULL AND EXISTS (",
            claim_gate!(),
            ") AND ((($2 & 1) <> 0 AND d.mode = 'immediate') OR (($2 & 2) <> 0 AND d.mode = 'digest')) \
         AND (($3 AND d.class = 'to') OR ($4 AND d.class = 'cc') OR ($5 AND d.class = 'list')) \
         AND (NOT $6 OR m.priority = 'urgent') \
         AND NOT EXISTS (SELECT 1 FROM claim_keys k WHERE k.seat_id = $1 AND k.key = $8) \
         ORDER BY d.seq LIMIT $7"
        )
    };
}

/// The seat (`$1`) if its counters say it has deliveries pending that a
/// claim with these binds (see `claim_select!`) could take.
macro_rules! claim_gate {
    () => {
        "SELECT 1 FROM seats g WHERE g.id = $1 AND ( \
         (($2 & 1) <> 0 AND (($3 AND g.pending_to > 0) OR ($4 AND g.pending_cc > 0) \
           OR ($5 AND g.pending_list > 0)) AND (NOT $6 OR g.pending_urgent > 0)) \
         OR (($2 & 2) <> 0 AND g.pending_digest > 0))"
    };
}

/// Columns and joins of [`AuthRow`]: a token (`t`) with its org and seat.
macro_rules! auth_select {
    () => {
        "SELECT t.id AS token_id, t.scope, t.seat_id, o.id, o.name, o.send_rate_per_min, o.send_burst, \
         o.max_recipients, o.max_thread_depth, o.webhook_url, o.ack_timeout_min, o.focus_max_min, \
         o.digest_interval_min, o.created_at, o.suspended_at, o.stored_bytes, s.id AS s_id, \
         s.org_id AS s_org_id, s.name AS s_name, \
         s.kind AS s_kind, s.parent_id AS s_parent_id, p.name AS s_parent_name, \
         s.display_name AS s_display_name, s.role AS s_role, s.harness AS s_harness, \
         s.model AS s_model, s.permission_mode AS s_permission_mode, s.group_name AS s_group_name, \
         s.last_seq AS s_last_seq, s.created_at AS s_created_at, \
         s.deleted_at AS s_deleted_at, s.focus_until AS s_focus_until \
         FROM tokens t JOIN orgs o ON o.id = t.org_id \
         LEFT JOIN seats s ON s.id = t.seat_id LEFT JOIN seats p ON p.id = s.parent_id"
    };
}

/// Runs a query body in an owned, boxed closure via `read` or `write`.
macro_rules! run {
    ($self:ident . $how:ident, |$c:ident| $body:expr) => {
        $self.$how(move |$c| Box::pin(async move { $body })).await
    };
}

// ---------------------------------------------------------------------------
// Row types (enums stored as TEXT) and their conversion to model records.

pub(crate) fn parse<T: FromStr<Err = String>>(s: &str) -> Result<T> {
    s.parse()
        .map_err(|e| StoreError::Invalid(format!("corrupt row: {e}")))
}

pub(crate) fn split_addrs(s: &str) -> Vec<String> {
    if s.is_empty() {
        Vec::new()
    } else {
        s.split(',').map(str::to_string).collect()
    }
}

pub(crate) fn join_addrs(v: &[String]) -> String {
    v.join(",")
}

/// A `LIKE` pattern matching `term` anywhere (escape character `\`).
pub(crate) fn like_pattern(term: &str) -> String {
    let mut p = String::with_capacity(term.len() + 2);
    p.push('%');
    for ch in term.chars() {
        if matches!(ch, '%' | '_' | '\\') {
            p.push('\\');
        }
        p.push(ch);
    }
    p.push('%');
    p
}

/// What differs between the two backends' SQL in search.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Dialect {
    Sqlite,
    Postgres,
}

impl Dialect {
    /// Case-insensitive `LIKE` (SQLite's `LIKE` folds ASCII letters).
    fn ilike(self) -> &'static str {
        match self {
            Dialect::Sqlite => "LIKE",
            Dialect::Postgres => "ILIKE",
        }
    }
    /// Character position of a substring, 0 when absent.
    fn strpos(self, hay: &str, needle: &str) -> String {
        match self {
            Dialect::Sqlite => format!("instr({hay}, {needle})"),
            Dialect::Postgres => format!("strpos({hay}, {needle})"),
        }
    }
    fn greatest(self) -> &'static str {
        match self {
            Dialect::Sqlite => "max",
            Dialect::Postgres => "GREATEST",
        }
    }
}

/// Ids of the messages visible to org `$1` in `[$3, $2)`: the org's own
/// and those delivered to it, each from its index.
pub(crate) const WINDOW_IDS: &str = "SELECT id FROM messages WHERE org_id = $1 AND id >= $3 AND id < $2 \
     UNION SELECT message_id FROM deliveries WHERE org_id = $1 AND message_id >= $3 AND message_id < $2";

/// One search step over the window `[$3, $2)` of org `$1`, at most `$4`
/// hits. Term `i` binds its `LIKE` pattern at `$(5+2i)` and its text at
/// `$(6+2i)`. The body itself is not returned: only an excerpt of
/// [`SNIPPET_CHARS`] characters starting 40 characters before the first
/// match in the body (`ex_start`, 1-based) and the body's length.
pub(crate) fn search_sql(d: Dialect, nterms: usize) -> String {
    let lb = "lower(m.body)";
    let positions: Vec<String> = (0..nterms)
        .map(|i| {
            format!(
                "NULLIF({}, 0)",
                d.strpos(lb, &format!("lower(${})", 6 + 2 * i))
            )
        })
        .collect();
    // The first match in the body, NULL when only the subject matched.
    let at = match d {
        Dialect::Postgres => format!("LEAST({})", positions.join(", ")),
        Dialect::Sqlite => {
            let big = "1000000000";
            let args: Vec<String> = positions
                .iter()
                .map(|p| format!("COALESCE({p}, {big})"))
                .chain([big.to_string()])
                .collect();
            format!("NULLIF(min({}), {big})", args.join(", "))
        }
    };
    let start = format!("{}(1, COALESCE({at}, 1) - 40)", d.greatest());
    let len = match d {
        Dialect::Sqlite => "length(m.body)",
        Dialect::Postgres => "CAST(char_length(m.body) AS BIGINT)",
    };
    let mut sql = format!(
        "SELECT {cols}, substr(m.body, {start}, {n}) AS body, CAST({start} AS BIGINT) AS ex_start, {len} AS body_chars \
         FROM messages m{joins} WHERE m.id IN ({WINDOW_IDS})",
        cols = msg_cols!(),
        joins = msg_joins!(),
        n = SNIPPET_CHARS,
    );
    for i in 0..nterms {
        let n = 5 + 2 * i;
        sql.push_str(&format!(
            " AND (m.subject {op} ${n} ESCAPE '\\' OR m.body {op} ${n} ESCAPE '\\')",
            op = d.ilike()
        ));
    }
    sql.push_str(" ORDER BY m.id DESC LIMIT $4");
    sql
}

/// A row of [`search_sql`].
#[derive(sqlx::FromRow)]
pub(crate) struct HitRow {
    #[sqlx(flatten)]
    msg: MsgRow,
    ex_start: i64,
    body_chars: i64,
}

impl HitRow {
    pub(crate) fn into_hit(self) -> Result<SearchHit> {
        let (start, total) = (self.ex_start, self.body_chars);
        let mut message = self.msg.into_message()?;
        let excerpt = std::mem::take(&mut message.body);
        let shown = excerpt.chars().count() as i64;
        Ok(SearchHit {
            message,
            excerpt,
            cut_before: start > 1,
            cut_after: start - 1 + shown < total,
        })
    }
}

/// The lower end of a window of at most `n` messages below some id, from
/// the counts and lowest ids of the index ranges it was built from (each
/// read at most `n` rows): `a` the org's own messages, `b` its deliveries,
/// `v` their union cut to `n`.
pub(crate) fn window_bounds(
    n: i64,
    a: (i64, Option<i64>),
    b: (i64, Option<i64>),
    v: (i64, Option<i64>),
) -> Window {
    let Some(mut lo) = v.1 else {
        return Window {
            lo: None,
            count: 0,
            exhausted: true,
        };
    };
    // A range that stopped at `n` rows may have more ids below its lowest
    // one; the window cannot reach below that.
    for (count, min) in [a, b] {
        if count >= n {
            lo = lo.max(min.unwrap_or(lo));
        }
    }
    Window {
        lo: Some(lo),
        count: v.0,
        exhausted: a.0 < n && b.0 < n && v.0 < n,
    }
}

/// Visible ids `[lo, before)` that one step of a search or an archive
/// query looks at.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Window {
    /// `None`: nothing visible below `before`.
    pub lo: Option<i64>,
    /// Ids counted in the window (at most `n`).
    pub count: i64,
    /// Nothing visible lies below `lo`.
    pub exhausted: bool,
}

/// Messages one step of a search or an archive query looks at.
pub(crate) const STEP_MESSAGES: i64 = 5_000;

/// A `LIKE` pattern matching `addr` as one element of a comma-separated
/// address list wrapped in commas.
pub(crate) fn addr_pattern(addr: &str) -> String {
    let inner = like_pattern(addr);
    format!("%,{},%", &inner[1..inner.len() - 1])
}

pub(crate) fn line_count(body: &str) -> i64 {
    body.lines().count() as i64
}

/// `meta` is stored as a JSON object, or `''` when empty.
pub(crate) fn meta_to_db(m: &Meta) -> String {
    if m.is_empty() {
        String::new()
    } else {
        serde_json::to_string(m).unwrap_or_default()
    }
}

fn meta_from_db(s: &str) -> Result<Meta> {
    if s.is_empty() {
        return Ok(Meta::new());
    }
    serde_json::from_str(s).map_err(|e| StoreError::Invalid(format!("corrupt meta: {e}")))
}

fn opt_parse<T: FromStr<Err = String>>(s: Option<&str>) -> Result<Option<T>> {
    s.map(parse).transpose()
}

#[derive(sqlx::FromRow)]
pub(crate) struct OrgRow {
    id: i64,
    name: String,
    send_rate_per_min: i64,
    send_burst: i64,
    max_recipients: i64,
    max_thread_depth: i64,
    webhook_url: Option<String>,
    ack_timeout_min: i64,
    focus_max_min: i64,
    digest_interval_min: i64,
    created_at: i64,
    suspended_at: Option<i64>,
    stored_bytes: i64,
}

impl From<OrgRow> for Org {
    fn from(r: OrgRow) -> Org {
        Org {
            id: r.id,
            name: r.name,
            send_rate_per_min: r.send_rate_per_min,
            send_burst: r.send_burst,
            max_recipients: r.max_recipients,
            max_thread_depth: r.max_thread_depth,
            webhook_url: r.webhook_url,
            ack_timeout_min: r.ack_timeout_min,
            focus_max_min: r.focus_max_min,
            digest_interval_min: r.digest_interval_min,
            created_at: r.created_at,
            suspended_at: r.suspended_at,
            stored_bytes: r.stored_bytes,
        }
    }
}

#[derive(sqlx::FromRow)]
pub(crate) struct SeatRow {
    id: i64,
    org_id: i64,
    name: String,
    kind: String,
    parent_id: Option<i64>,
    parent_name: Option<String>,
    display_name: Option<String>,
    role: Option<String>,
    harness: Option<String>,
    model: Option<String>,
    permission_mode: Option<String>,
    group_name: Option<String>,
    last_seq: i64,
    created_at: i64,
    deleted_at: Option<i64>,
    focus_until: Option<i64>,
}

impl SeatRow {
    pub(crate) fn into_seat(self) -> Result<Seat> {
        Ok(Seat {
            id: self.id,
            org_id: self.org_id,
            name: self.name,
            kind: parse::<SeatKind>(&self.kind)?,
            parent_id: self.parent_id,
            parent_name: self.parent_name,
            display_name: self.display_name,
            role: self.role,
            harness: self.harness,
            model: self.model,
            permission_mode: opt_parse::<PermissionMode>(self.permission_mode.as_deref())?,
            group: self.group_name,
            last_seq: self.last_seq,
            created_at: self.created_at,
            deleted_at: self.deleted_at,
            focus_until: self.focus_until,
        })
    }
}

#[derive(sqlx::FromRow)]
pub(crate) struct AuthRow {
    token_id: i64,
    scope: String,
    seat_id: Option<i64>,
    #[sqlx(flatten)]
    org: OrgRow,
    s_id: Option<i64>,
    s_org_id: Option<i64>,
    s_name: Option<String>,
    s_kind: Option<String>,
    s_parent_id: Option<i64>,
    s_parent_name: Option<String>,
    s_display_name: Option<String>,
    s_role: Option<String>,
    s_harness: Option<String>,
    s_model: Option<String>,
    s_permission_mode: Option<String>,
    s_group_name: Option<String>,
    s_last_seq: Option<i64>,
    s_created_at: Option<i64>,
    s_deleted_at: Option<i64>,
    s_focus_until: Option<i64>,
}

impl AuthRow {
    /// `None` if the token's seat no longer exists.
    pub(crate) fn into_auth(self) -> Result<Option<TokenAuth>> {
        let seat = match (self.seat_id, self.s_id) {
            (None, _) => None,
            (Some(_), Some(id)) if self.s_deleted_at.is_none() => Some(Seat {
                id,
                org_id: self.s_org_id.unwrap_or_default(),
                name: self.s_name.unwrap_or_default(),
                kind: parse::<SeatKind>(self.s_kind.as_deref().unwrap_or_default())?,
                parent_id: self.s_parent_id,
                parent_name: self.s_parent_name,
                display_name: self.s_display_name,
                role: self.s_role,
                harness: self.s_harness,
                model: self.s_model,
                permission_mode: opt_parse::<PermissionMode>(self.s_permission_mode.as_deref())?,
                group: self.s_group_name,
                last_seq: self.s_last_seq.unwrap_or_default(),
                created_at: self.s_created_at.unwrap_or_default(),
                deleted_at: None,
                focus_until: self.s_focus_until,
            }),
            (Some(_), _) => return Ok(None),
        };
        Ok(Some(TokenAuth {
            token_id: self.token_id,
            scope: parse(&self.scope)?,
            org: self.org.into(),
            seat,
        }))
    }
}

#[derive(sqlx::FromRow)]
pub(crate) struct ListRow {
    id: i64,
    org_id: i64,
    name: String,
    description: Option<String>,
    post_policy: String,
    managed: bool,
    member_count: i64,
    created_at: i64,
}

impl ListRow {
    pub(crate) fn into_list(self) -> Result<List> {
        Ok(List {
            id: self.id,
            org_id: self.org_id,
            name: self.name,
            description: self.description,
            post_policy: parse::<PostPolicy>(&self.post_policy)?,
            managed: self.managed,
            member_count: self.member_count,
            created_at: self.created_at,
        })
    }
}

#[derive(sqlx::FromRow)]
pub(crate) struct MemberRow {
    address: String,
    seat_id: Option<i64>,
    mode: String,
}

impl MemberRow {
    pub(crate) fn into_member(self) -> Result<Member> {
        Ok(Member {
            address: self.address,
            seat_id: self.seat_id,
            mode: parse::<DeliveryMode>(&self.mode)?,
        })
    }
}

#[derive(sqlx::FromRow)]
pub(crate) struct LinkRow {
    peer_org_id: i64,
    peer_name: String,
    state: String,
}

impl LinkRow {
    pub(crate) fn into_link(self) -> Result<Link> {
        Ok(Link {
            peer_org_id: self.peer_org_id,
            peer_name: self.peer_name,
            state: parse::<LinkState>(&self.state)?,
        })
    }
}

#[derive(sqlx::FromRow)]
pub(crate) struct MsgRow {
    id: i64,
    org_id: i64,
    org_name: String,
    sender_seat_id: i64,
    sender_name: String,
    sender_kind: String,
    thread_id: i64,
    parent_id: Option<i64>,
    depth: i64,
    subject: String,
    body: String,
    lines: i64,
    to_addrs: String,
    cc_addrs: String,
    priority: String,
    ack_required: bool,
    meta: String,
    created_at: i64,
    expired_at: Option<i64>,
}

impl MsgRow {
    pub(crate) fn into_message(self) -> Result<MessageRec> {
        Ok(MessageRec {
            id: self.id,
            org_id: self.org_id,
            org_name: self.org_name,
            sender_seat_id: self.sender_seat_id,
            sender_name: self.sender_name,
            sender_kind: parse::<SeatKind>(&self.sender_kind)?,
            thread_id: self.thread_id,
            parent_id: self.parent_id,
            depth: self.depth,
            subject: self.subject,
            body: self.body,
            lines: self.lines,
            to_addrs: split_addrs(&self.to_addrs),
            cc_addrs: split_addrs(&self.cc_addrs),
            priority: parse::<Priority>(&self.priority)?,
            ack_required: self.ack_required,
            meta: meta_from_db(&self.meta)?,
            created_at: self.created_at,
            expired_at: self.expired_at,
        })
    }
}

/// A message header with the size of its body in bytes.
#[derive(sqlx::FromRow)]
pub(crate) struct SizedRow {
    #[sqlx(flatten)]
    pub(crate) msg: MsgRow,
    pub(crate) body_bytes: Option<i64>,
}

#[derive(sqlx::FromRow)]
pub(crate) struct DelivRow {
    d_id: i64,
    d_org_id: i64,
    d_org_name: String,
    message_id: i64,
    seat_id: i64,
    seat_name: String,
    class: String,
    mode: String,
    seq: i64,
    mod_seq: i64,
    notified_at: Option<i64>,
    notified_via: Option<String>,
    read_at: Option<i64>,
    acked_at: Option<i64>,
}

impl DelivRow {
    pub(crate) fn into_delivery(self) -> Result<DeliveryRec> {
        Ok(DeliveryRec {
            id: self.d_id,
            org_id: self.d_org_id,
            org_name: self.d_org_name,
            message_id: self.message_id,
            seat_id: self.seat_id,
            seat_name: self.seat_name,
            class: parse::<DeliveryClass>(&self.class)?,
            mode: parse::<DeliveryMode>(&self.mode)?,
            seq: self.seq,
            mod_seq: self.mod_seq,
            notified_at: self.notified_at,
            notified_via: self.notified_via,
            read_at: self.read_at,
            acked_at: self.acked_at,
        })
    }
}

#[derive(sqlx::FromRow)]
pub(crate) struct EntryRow {
    #[sqlx(flatten)]
    d: DelivRow,
    #[sqlx(flatten)]
    m: MsgRow,
}

impl EntryRow {
    pub(crate) fn into_entry(self) -> Result<Entry> {
        Ok(Entry {
            delivery: self.d.into_delivery()?,
            message: self.m.into_message()?,
        })
    }
}

#[derive(sqlx::FromRow)]
pub(crate) struct TokenRow {
    id: i64,
    scope: String,
    seat_name: Option<String>,
    label: Option<String>,
    created_at: i64,
}

impl TokenRow {
    pub(crate) fn into_token(self) -> Result<TokenRec> {
        Ok(TokenRec {
            id: self.id,
            scope: parse(&self.scope)?,
            seat_name: self.seat_name,
            label: self.label,
            created_at: self.created_at,
        })
    }
}

#[derive(sqlx::FromRow)]
pub(crate) struct PendingRow {
    seat_id: i64,
    seat_name: String,
    kind: String,
    last_seq: i64,
    to_n: i64,
    cc_n: i64,
    list_n: i64,
    urgent_n: i64,
    digest_n: i64,
    digest_since: Option<i64>,
    focus_until: Option<i64>,
}

impl From<PendingRow> for PendingRec {
    fn from(r: PendingRow) -> PendingRec {
        PendingRec {
            seat_id: r.seat_id,
            user: r.kind == "user",
            seat_name: r.seat_name,
            last_seq: r.last_seq,
            to: r.to_n,
            cc: r.cc_n,
            list: r.list_n,
            urgent: r.urgent_n,
            digest: r.digest_n,
            digest_since: r.digest_since,
            focus_until: r.focus_until,
        }
    }
}

#[derive(sqlx::FromRow)]
pub(crate) struct PresenceRow {
    seat: String,
    state: String,
    wake_failed: bool,
    harness: Option<String>,
    node: Option<String>,
    since: i64,
    updated_at: i64,
    attention: Option<String>,
    attention_hint: Option<String>,
    terminal_url: Option<String>,
}

impl PresenceRow {
    pub(crate) fn into_rec(self) -> Result<PresenceRec> {
        Ok(PresenceRec {
            seat: self.seat,
            state: parse(&self.state)?,
            wake_failed: self.wake_failed,
            harness: self.harness,
            node: self.node,
            since: self.since,
            updated_at: self.updated_at,
            attention: self.attention,
            attention_hint: self.attention_hint,
            terminal_url: self.terminal_url,
        })
    }
}

/// Comma-separated ids (as stored in `claim_keys.ids`).
pub(crate) fn parse_ids(s: &str) -> Vec<i64> {
    s.split(',').filter_map(|x| x.trim().parse().ok()).collect()
}

/// `$first, $first+1, ...` for `n` values.
pub(crate) fn placeholders(first: usize, n: usize) -> String {
    (0..n)
        .map(|i| format!("${}", first + i))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Message ids follow creation times up to this much reordering.
pub(crate) const ID_TIME_MARGIN_MS: i64 = 5 * 60 * 1000;

/// The `claim_select!` mode mask of a filter.
pub(crate) fn mode_mask(f: &ClaimFilter) -> i64 {
    match (f.digest_only, f.include_digest) {
        (true, _) => 2,
        (false, true) => 3,
        (false, false) => 1,
    }
}

pub(crate) fn collect<R, T>(rows: Vec<R>, f: impl Fn(R) -> Result<T>) -> Result<Vec<T>> {
    rows.into_iter().map(f).collect()
}

/// Generates `mod q` (the portable queries, for one sqlx database type) and
/// `impl Store for $store`.
macro_rules! impl_store_common {
    ($store:ty, $db:ty, $dialect:expr) => {
        #[allow(dead_code)]
        pub(crate) mod q {
            use $crate::common::*;
            use $crate::model::*;
            use $crate::{now_ms, Result, StoreError};
            use amail_proto::limits::MAX_SCAN_MESSAGES;
            use amail_proto::{LinkState, SeatKind, TokenScope};

            pub(crate) type Conn = <$db as sqlx::Database>::Connection;
            pub(crate) type Db = $db;
            const DIALECT: Dialect = $dialect;

            pub async fn org_by_id(c: &mut Conn, id: i64) -> Result<Option<Org>> {
                let r: Option<OrgRow> = sqlx::query_as("SELECT * FROM orgs WHERE id = $1 AND deleted_at IS NULL")
                    .bind(id)
                    .fetch_optional(&mut *c)
                    .await?;
                Ok(r.map(Org::from))
            }

            pub async fn org_by_name(c: &mut Conn, name: &str) -> Result<Option<Org>> {
                let r: Option<OrgRow> = sqlx::query_as("SELECT * FROM orgs WHERE name = $1 AND deleted_at IS NULL")
                    .bind(name)
                    .fetch_optional(&mut *c)
                    .await?;
                Ok(r.map(Org::from))
            }

            pub async fn seat_by_id(c: &mut Conn, id: i64) -> Result<Option<Seat>> {
                let r: Option<SeatRow> = sqlx::query_as(concat!(seat_select!(), " WHERE s.id = $1"))
                    .bind(id)
                    .fetch_optional(&mut *c)
                    .await?;
                r.map(SeatRow::into_seat).transpose()
            }

            pub async fn seat_by_name(c: &mut Conn, org_id: i64, name: &str) -> Result<Option<Seat>> {
                let r: Option<SeatRow> = sqlx::query_as(concat!(
                    seat_select!(),
                    " WHERE s.org_id = $1 AND s.name = $2 AND s.deleted_at IS NULL"
                ))
                .bind(org_id)
                .bind(name)
                .fetch_optional(&mut *c)
                .await?;
                r.map(SeatRow::into_seat).transpose()
            }

            pub async fn seats(c: &mut Conn, org_id: i64) -> Result<Vec<Seat>> {
                let rows: Vec<SeatRow> = sqlx::query_as(concat!(
                    seat_select!(),
                    " WHERE s.org_id = $1 AND s.deleted_at IS NULL ORDER BY s.name"
                ))
                .bind(org_id)
                .fetch_all(&mut *c)
                .await?;
                collect(rows, SeatRow::into_seat)
            }

            /// Serialize name allocation within the org (the org row is the
            /// lock on Postgres; SQLite has a single writer) and check that
            /// no live seat or list uses `name`.
            pub async fn reserve_name(c: &mut Conn, org_id: i64, name: &str) -> Result<()> {
                let n = sqlx::query("UPDATE orgs SET name_version = name_version + 1 WHERE id = $1")
                    .bind(org_id)
                    .execute(&mut *c)
                    .await?
                    .rows_affected();
                if n == 0 {
                    return Err(StoreError::NotFound("org".into()));
                }
                let taken: i64 = sqlx::query_scalar(
                    "SELECT COUNT(*) FROM (\
                       SELECT 1 AS x FROM seats WHERE org_id = $1 AND name = $2 AND deleted_at IS NULL \
                       UNION ALL SELECT 1 AS x FROM lists WHERE org_id = $1 AND name = $2) t",
                )
                .bind(org_id)
                .bind(name)
                .fetch_one(&mut *c)
                .await?;
                if taken > 0 {
                    return Err(StoreError::Conflict(format!("name {name:?} is already in use")));
                }
                Ok(())
            }

            pub async fn insert_seat(
                c: &mut Conn,
                org_id: i64,
                kind: SeatKind,
                parent_id: Option<i64>,
                s: &NewSeat,
            ) -> Result<Seat> {
                reserve_name(c, org_id, &s.name).await?;
                let id: i64 = sqlx::query_scalar(
                    "INSERT INTO seats (org_id, name, kind, parent_id, display_name, role, harness, model, \
                     permission_mode, group_name, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
                     RETURNING id",
                )
                .bind(org_id)
                .bind(&s.name)
                .bind(kind.as_str())
                .bind(parent_id)
                .bind(&s.display_name)
                .bind(&s.role)
                .bind(&s.harness)
                .bind(&s.model)
                .bind(s.permission_mode.map(|m| m.as_str()))
                .bind(&s.group)
                .bind(now_ms())
                .fetch_one(&mut *c)
                .await?;
                seat_by_id(c, id).await?.ok_or_else(|| StoreError::NotFound("seat".into()))
            }

            pub async fn insert_token(c: &mut Conn, t: &NewToken) -> Result<()> {
                sqlx::query(
                    "INSERT INTO tokens (org_id, scope, seat_id, hash, label, created_at) \
                     VALUES ($1, $2, $3, $4, $5, $6)",
                )
                .bind(t.org_id)
                .bind(t.scope.as_str())
                .bind(t.seat_id)
                .bind(&t.hash)
                .bind(&t.label)
                .bind(now_ms())
                .execute(&mut *c)
                .await?;
                Ok(())
            }

            pub async fn create_org(c: &mut Conn, name: &str, user_seat: &str, hash: &str) -> Result<(Org, Seat)> {
                let id: i64 = sqlx::query_scalar("INSERT INTO orgs (name, created_at) VALUES ($1, $2) RETURNING id")
                    .bind(name)
                    .bind(now_ms())
                    .fetch_one(&mut *c)
                    .await
                    .map_err(|e| match StoreError::from(e) {
                        StoreError::Conflict(_) => StoreError::Conflict(format!("org {name:?} already exists")),
                        other => other,
                    })?;
                // Requests made while no org had this name do not carry
                // over, and are kept (until they expire) so that the org that
                // made them cannot tell that the name was registered.
                let seat = NewSeat { name: user_seat.to_string(), ..Default::default() };
                let seat = insert_seat(c, id, SeatKind::User, None, &seat).await?;
                let token = NewToken {
                    org_id: id,
                    scope: TokenScope::User,
                    seat_id: Some(seat.id),
                    hash: hash.to_string(),
                    label: Some("initial user token".into()),
                };
                insert_token(c, &token).await?;
                let org = org_by_id(c, id).await?.ok_or_else(|| StoreError::NotFound("org".into()))?;
                Ok((org, seat))
            }

            pub async fn update_org(c: &mut Conn, org_id: i64, p: &OrgSettings) -> Result<Org> {
                sqlx::query(
                    "UPDATE orgs SET send_rate_per_min = COALESCE($1, send_rate_per_min), \
                     send_burst = COALESCE($2, send_burst), max_recipients = COALESCE($3, max_recipients), \
                     max_thread_depth = COALESCE($4, max_thread_depth), \
                     webhook_url = CASE WHEN $6 THEN $7 ELSE webhook_url END, \
                     ack_timeout_min = COALESCE($8, ack_timeout_min), \
                     focus_max_min = COALESCE($9, focus_max_min), \
                     digest_interval_min = COALESCE($10, digest_interval_min) WHERE id = $5",
                )
                .bind(p.send_rate_per_min)
                .bind(p.send_burst)
                .bind(p.max_recipients)
                .bind(p.max_thread_depth)
                .bind(org_id)
                .bind(p.webhook_url.is_some())
                .bind(p.webhook_url.clone().flatten())
                .bind(p.ack_timeout_min)
                .bind(p.focus_max_min)
                .bind(p.digest_interval_min)
                .execute(&mut *c)
                .await?;
                org_by_id(c, org_id).await?.ok_or_else(|| StoreError::NotFound("org".into()))
            }

            pub async fn token_auth(c: &mut Conn, hash: &str) -> Result<Option<TokenAuth>> {
                let row: Option<AuthRow> = sqlx::query_as(concat!(
                    auth_select!(),
                    " WHERE t.hash = $1 AND t.revoked_at IS NULL"
                ))
                .bind(hash)
                .fetch_optional(&mut *c)
                .await?;
                row.map(AuthRow::into_auth).transpose().map(Option::flatten)
            }

            pub async fn session_auth(c: &mut Conn, hash: &str, now: i64) -> Result<Option<TokenAuth>> {
                let row: Option<AuthRow> = sqlx::query_as(concat!(
                    auth_select!(),
                    " JOIN web_sessions w ON w.token_id = t.id \
                     WHERE w.hash = $1 AND w.expires_at > $2 AND t.revoked_at IS NULL"
                ))
                .bind(hash)
                .bind(now)
                .fetch_optional(&mut *c)
                .await?;
                row.map(AuthRow::into_auth).transpose().map(Option::flatten)
            }

            /// Store a browser session; the token's oldest sessions beyond
            /// [`amail_proto::limits::MAX_SESSIONS_PER_TOKEN`] end. (Expired sessions are
            /// dropped by [`cleanup`].)
            pub async fn create_session(c: &mut Conn, hash: &str, token_id: i64, expires_at: i64) -> Result<()> {
                let now = now_ms();
                sqlx::query(
                    "INSERT INTO web_sessions (hash, token_id, created_at, expires_at) VALUES ($1, $2, $3, $4)",
                )
                .bind(hash)
                .bind(token_id)
                .bind(now)
                .bind(expires_at)
                .execute(&mut *c)
                .await?;
                sqlx::query(
                    "DELETE FROM web_sessions WHERE token_id = $1 AND hash <> $3 AND hash NOT IN \
                     (SELECT hash FROM web_sessions WHERE token_id = $1 AND hash <> $3 ORDER BY created_at DESC, hash LIMIT $2)",
                )
                .bind(token_id)
                .bind(amail_proto::limits::MAX_SESSIONS_PER_TOKEN - 1)
                .bind(hash)
                .execute(&mut *c)
                .await?;
                Ok(())
            }

            pub async fn delete_session(c: &mut Conn, hash: &str) -> Result<bool> {
                let n = sqlx::query("DELETE FROM web_sessions WHERE hash = $1")
                    .bind(hash)
                    .execute(&mut *c)
                    .await?
                    .rows_affected();
                Ok(n > 0)
            }

            pub async fn user_seat(c: &mut Conn, org_id: i64) -> Result<Option<Seat>> {
                let r: Option<SeatRow> = sqlx::query_as(concat!(
                    seat_select!(),
                    " WHERE s.org_id = $1 AND s.kind = 'user' AND s.deleted_at IS NULL"
                ))
                .bind(org_id)
                .fetch_optional(&mut *c)
                .await?;
                r.map(SeatRow::into_seat).transpose()
            }

            /// From the seats' counters (kept by triggers on `deliveries`).
            pub async fn counts(c: &mut Conn, org_id: i64, seat_id: Option<i64>) -> Result<Vec<CountRec>> {
                let rows: Vec<(i64, String, i64, i64, i64, Option<i64>)> = sqlx::query_as(
                    "SELECT s.id, s.name, s.last_seq, s.unread, \
                     s.pending_to + s.pending_cc + s.pending_list AS pending, s.focus_until \
                     FROM seats s WHERE s.org_id = $1 AND s.deleted_at IS NULL AND ($2 IS NULL OR s.id = $2) \
                     ORDER BY s.name",
                )
                .bind(org_id)
                .bind(seat_id)
                .fetch_all(&mut *c)
                .await?;
                Ok(rows
                    .into_iter()
                    .map(|(seat_id, seat_name, last_seq, unread, pending, focus_until)| {
                        CountRec { seat_id, seat_name, last_seq, unread, pending, focus_until }
                    })
                    .collect())
            }

            /// Deliveries to the user seat that wait for the user: ack
            /// requested and not given, or unread `to` mail (directly or
            /// through a list in To, immediate mode) the user has not
            /// answered (sent nothing later in the thread). Only the seat's
            /// newest [`amail_proto::limits::MAX_ATTENTION_SCAN`] deliveries
            /// are looked at; each is checked with index lookups.
            pub async fn attention(c: &mut Conn, user_seat_id: i64, limit: i64) -> Result<Vec<Entry>> {
                let rows: Vec<EntryRow> = sqlx::query_as(concat!(
                    "SELECT ", deliv_cols!(), ", ", msg_cols!(), ", '' AS body FROM \
                     (SELECT * FROM deliveries WHERE seat_id = $1 ORDER BY seq DESC LIMIT $3) d",
                    deliv_joins!(),
                    " JOIN messages m ON m.id = d.message_id",
                    msg_joins!(),
                    " WHERE d.mode <> 'archive' AND m.sender_seat_id <> $1 AND ( \
                       (m.ack_required AND d.acked_at IS NULL) \
                       OR (d.class = 'to' AND d.mode = 'immediate' AND d.read_at IS NULL AND ( \
                           SELECT r.id FROM messages r WHERE r.thread_id = m.thread_id AND r.org_id = ds.org_id \
                           AND r.sender_seat_id = $1 AND r.id > m.id \
                           ORDER BY r.thread_id, r.org_id, r.sender_seat_id, r.id LIMIT 1) IS NULL)) \
                     ORDER BY d.seq LIMIT $2"
                ))
                .bind(user_seat_id)
                .bind(limit)
                .bind(amail_proto::limits::MAX_ATTENTION_SCAN)
                .fetch_all(&mut *c)
                .await?;
                collect(rows, EntryRow::into_entry)
            }

            /// The first message id created at or after `t` (ids follow
            /// creation times up to [`ID_TIME_MARGIN_MS`]), from the time index.
            async fn id_at(c: &mut Conn, t: i64) -> Result<Option<i64>> {
                Ok(sqlx::query_scalar::<_, i64>(
                    "SELECT id FROM messages WHERE created_at >= $1 ORDER BY created_at, id LIMIT 1",
                )
                .bind(t)
                .fetch_optional(&mut *c)
                .await?)
            }

            /// Messages the org sent or received from minute `since` on (see
            /// `org_activity`).
            pub async fn activity(c: &mut Conn, org_id: i64, since: i64) -> Result<i64> {
                let n: Option<i64> = sqlx::query_scalar(
                    "SELECT CAST(SUM(messages) AS BIGINT) FROM org_activity WHERE org_id = $1 AND minute >= $2",
                )
                .bind(org_id)
                .bind(since)
                .fetch_one(&mut *c)
                .await?;
                Ok(n.unwrap_or(0))
            }

            /// Add `(org, minute, messages)` counts, in key order (two
            /// instances adding at once cannot deadlock).
            pub async fn add_activity(c: &mut Conn, counts: &[(i64, i64, i64)]) -> Result<()> {
                let mut v = counts.to_vec();
                v.sort_unstable();
                for chunk in v.chunks(300) {
                    let rows: Vec<String> = (0..chunk.len()).map(|i| format!("({})", placeholders(i * 3 + 1, 3))).collect();
                    let sql = format!(
                        "INSERT INTO org_activity (org_id, minute, messages) VALUES {} \
                         ON CONFLICT (org_id, minute) DO UPDATE SET messages = org_activity.messages + excluded.messages",
                        rows.join(", ")
                    );
                    let mut q = sqlx::query(&sql);
                    for (org, minute, n) in chunk {
                        q = q.bind(*org).bind(*minute).bind(*n);
                    }
                    q.execute(&mut *c).await?;
                }
                Ok(())
            }

            /// The window of the (at most) `n` newest messages visible to
            /// the org below `before`, read from the org's two indexes
            /// (at most `n` rows from each).
            pub async fn scan_window(c: &mut Conn, org_id: i64, before: i64, n: i64) -> Result<Window> {
                type R = (i64, Option<i64>, i64, Option<i64>, i64, Option<i64>);
                let r: R = sqlx::query_as(
                    "WITH a AS (SELECT id FROM messages WHERE org_id = $1 AND id < $2 ORDER BY id DESC LIMIT $3), \
                     b AS (SELECT message_id AS id FROM deliveries WHERE org_id = $1 AND message_id < $2 \
                           ORDER BY message_id DESC LIMIT $3), \
                     v AS (SELECT id FROM (SELECT id FROM a UNION SELECT id FROM b) u ORDER BY id DESC LIMIT $3) \
                     SELECT (SELECT COUNT(*) FROM a), (SELECT MIN(id) FROM a), \
                            (SELECT COUNT(*) FROM b), (SELECT MIN(id) FROM b), \
                            (SELECT COUNT(*) FROM v), (SELECT MIN(id) FROM v)",
                )
                .bind(org_id)
                .bind(before)
                .bind(n)
                .fetch_one(&mut *c)
                .await?;
                Ok(window_bounds(n, (r.0, r.1), (r.2, r.3), (r.4, r.5)))
            }

            /// The window of the (at most) `n` newest messages of a thread
            /// below `before`, of all orgs (visibility is checked after).
            async fn thread_window(c: &mut Conn, thread_id: i64, before: i64, n: i64) -> Result<Window> {
                let r: (i64, Option<i64>) = sqlx::query_as(
                    "SELECT COUNT(*), MIN(id) FROM (SELECT id FROM messages WHERE thread_id = $1 AND id < $2 \
                     ORDER BY id DESC LIMIT $3) t",
                )
                .bind(thread_id)
                .bind(before)
                .bind(n)
                .fetch_one(&mut *c)
                .await?;
                Ok(match r.1 {
                    None => Window { lo: None, count: 0, exhausted: true },
                    Some(lo) => Window { lo: Some(lo), count: r.0, exhausted: r.0 < n },
                })
            }

            /// Body sizes of the messages visible to the org in `[lo, before)`,
            /// newest first (the size is read without the body on both
            /// backends).
            async fn window_sizes(c: &mut Conn, org_id: i64, before: i64, lo: i64) -> Result<Vec<(i64, i64)>> {
                let sql = format!(
                    "SELECT m.id, CAST(COALESCE(octet_length(m.body), 0) AS BIGINT) FROM messages m \
                     WHERE m.id IN ({WINDOW_IDS}) ORDER BY m.id DESC"
                );
                Ok(sqlx::query_as(&sql)
                    .bind(org_id)
                    .bind(before)
                    .bind(lo)
                    .fetch_all(&mut *c)
                    .await?)
            }

            /// Search: steps over the org's window, newest first, until the
            /// page is full, `q.scan` messages were looked at, or
            /// `q.time_limit` has passed. A step looks at up to
            /// [`STEP_MESSAGES`] messages with up to
            /// [`amail_proto::limits::SEARCH_STEP_BYTES`] of body text in
            /// all (divided by the number of terms, at least one message),
            /// so that one step takes a short, bounded time whatever the
            /// sizes of the bodies.
            pub async fn search(c: &mut Conn, org_id: i64, q: &SearchQuery) -> Result<SearchResult> {
                let started = std::time::Instant::now();
                let sql = search_sql(DIALECT, q.terms.len());
                let step_bytes = amail_proto::limits::SEARCH_STEP_BYTES / q.terms.len().max(1) as i64;
                let mut out = SearchResult::default();
                let mut before = q.before_id.unwrap_or(i64::MAX);
                let mut seen = 0;
                // Messages per step: grows back to STEP_MESSAGES while the
                // bodies are small, shrinks when they are large.
                let mut step = STEP_MESSAGES;
                while seen < q.scan {
                    if seen > 0 && q.time_limit.is_some_and(|t| started.elapsed() >= t) {
                        break;
                    }
                    let mut w = scan_window(c, org_id, before, step.min(q.scan - seen)).await?;
                    let Some(mut lo) = w.lo else {
                        return Ok(out);
                    };
                    // Cut the window to the byte budget.
                    let sizes = window_sizes(c, org_id, before, lo).await?;
                    let mut total = 0;
                    let mut kept = 0;
                    for (_, n) in &sizes {
                        if kept > 0 && total + n > step_bytes {
                            break;
                        }
                        total += n;
                        kept += 1;
                    }
                    if kept < sizes.len() {
                        lo = sizes[kept - 1].0;
                        w = Window { lo: Some(lo), count: kept as i64, exhausted: false };
                        step = (kept as i64 * 2).clamp(16, STEP_MESSAGES);
                    } else {
                        step = (step * 2).min(STEP_MESSAGES);
                    }
                    let mut query = sqlx::query_as::<_, HitRow>(&sql)
                        .bind(org_id)
                        .bind(before)
                        .bind(lo)
                        .bind(q.limit - out.hits.len() as i64);
                    for t in &q.terms {
                        query = query.bind(like_pattern(t)).bind(t.clone());
                    }
                    for r in query.fetch_all(&mut *c).await? {
                        out.hits.push(r.into_hit()?);
                    }
                    if out.hits.len() as i64 >= q.limit {
                        out.next_before_id = out.hits.last().map(|h| h.message.id);
                        return Ok(out);
                    }
                    if w.exhausted {
                        return Ok(out);
                    }
                    seen += w.count;
                    before = lo;
                }
                out.next_before_id = Some(before);
                Ok(out)
            }

            /// Live seats of the org with the given names (missing ones are skipped).
            pub async fn seats_by_names(c: &mut Conn, org_id: i64, names: &[String]) -> Result<Vec<Seat>> {
                if names.is_empty() {
                    return Ok(Vec::new());
                }
                let placeholders: Vec<String> = (0..names.len()).map(|i| format!("${}", i + 2)).collect();
                let sql = format!(
                    "{} WHERE s.org_id = $1 AND s.deleted_at IS NULL AND s.name IN ({})",
                    seat_select!(),
                    placeholders.join(", ")
                );
                let mut q = sqlx::query_as::<_, SeatRow>(&sql).bind(org_id);
                for n in names {
                    q = q.bind(n);
                }
                collect(q.fetch_all(&mut *c).await?, SeatRow::into_seat)
            }

            async fn resolve_parent(c: &mut Conn, org_id: i64, name: &str) -> Result<i64> {
                match seat_by_name(c, org_id, name).await? {
                    Some(p) => Ok(p.id),
                    None => Err(StoreError::Invalid(format!("unknown parent seat {name:?}"))),
                }
            }

            pub async fn create_seat(c: &mut Conn, org_id: i64, s: &NewSeat) -> Result<Seat> {
                let parent_id = match &s.parent {
                    Some(p) => Some(resolve_parent(c, org_id, p).await?),
                    None => None,
                };
                insert_seat(c, org_id, SeatKind::Agent, parent_id, s).await
            }

            pub async fn update_seat(c: &mut Conn, org_id: i64, name: &str, p: &SeatUpdate) -> Result<Seat> {
                let seat = seat_by_name(c, org_id, name)
                    .await?
                    .ok_or_else(|| StoreError::NotFound(format!("seat {name:?}")))?;
                let parent_id = match &p.parent {
                    None => seat.parent_id,
                    Some(None) => None,
                    Some(Some(pname)) => {
                        let pid = resolve_parent(c, org_id, pname).await?;
                        // Walk up from the new parent; reaching the seat means a cycle.
                        let mut cur = Some(pid);
                        while let Some(id) = cur {
                            if id == seat.id {
                                return Err(StoreError::Invalid("parent would create a cycle".into()));
                            }
                            cur = seat_by_id(c, id).await?.and_then(|s| s.parent_id);
                        }
                        Some(pid)
                    }
                };
                let pick = |new: &Option<Option<String>>, old: &Option<String>| match new {
                    None => old.clone(),
                    Some(v) => v.clone(),
                };
                sqlx::query(
                    "UPDATE seats SET parent_id = $1, display_name = $2, role = $3, harness = $4, model = $5, \
                     permission_mode = $6, group_name = $7 WHERE id = $8",
                )
                .bind(parent_id)
                .bind(pick(&p.display_name, &seat.display_name))
                .bind(pick(&p.role, &seat.role))
                .bind(pick(&p.harness, &seat.harness))
                .bind(pick(&p.model, &seat.model))
                .bind(match p.permission_mode {
                    None => seat.permission_mode,
                    Some(v) => v,
                }.map(|m| m.as_str()))
                .bind(pick(&p.group, &seat.group))
                .bind(seat.id)
                .execute(&mut *c)
                .await?;
                seat_by_id(c, seat.id).await?.ok_or_else(|| StoreError::NotFound("seat".into()))
            }

            pub async fn delete_seat(c: &mut Conn, org_id: i64, name: &str) -> Result<()> {
                let seat = seat_by_name(c, org_id, name)
                    .await?
                    .ok_or_else(|| StoreError::NotFound(format!("seat {name:?}")))?;
                if seat.kind == SeatKind::User {
                    return Err(StoreError::Invalid("the user seat cannot be deleted".into()));
                }
                let now = now_ms();
                sqlx::query("UPDATE seats SET deleted_at = $1 WHERE id = $2")
                    .bind(now)
                    .bind(seat.id)
                    .execute(&mut *c)
                    .await?;
                sqlx::query("UPDATE seats SET parent_id = $1 WHERE parent_id = $2")
                    .bind(seat.parent_id)
                    .bind(seat.id)
                    .execute(&mut *c)
                    .await?;
                sqlx::query("DELETE FROM list_members WHERE seat_id = $1")
                    .bind(seat.id)
                    .execute(&mut *c)
                    .await?;
                sqlx::query("UPDATE tokens SET revoked_at = $1 WHERE seat_id = $2 AND revoked_at IS NULL")
                    .bind(now)
                    .bind(seat.id)
                    .execute(&mut *c)
                    .await?;
                Ok(())
            }

            pub async fn list_by_name(c: &mut Conn, org_id: i64, name: &str) -> Result<Option<List>> {
                let r: Option<ListRow> =
                    sqlx::query_as(concat!(list_select!(), " WHERE l.org_id = $1 AND l.name = $2"))
                        .bind(org_id)
                        .bind(name)
                        .fetch_optional(&mut *c)
                        .await?;
                r.map(ListRow::into_list).transpose()
            }

            pub async fn lists(c: &mut Conn, org_id: i64) -> Result<Vec<List>> {
                let rows: Vec<ListRow> =
                    sqlx::query_as(concat!(list_select!(), " WHERE l.org_id = $1 ORDER BY l.name"))
                        .bind(org_id)
                        .fetch_all(&mut *c)
                        .await?;
                collect(rows, ListRow::into_list)
            }

            pub async fn create_list(c: &mut Conn, org_id: i64, l: &NewList) -> Result<List> {
                reserve_name(c, org_id, &l.name).await?;
                let policy = l.post_policy.unwrap_or(amail_proto::PostPolicy::Anyone);
                sqlx::query(
                    "INSERT INTO lists (org_id, name, description, post_policy, managed, created_at) \
                     VALUES ($1, $2, $3, $4, $5, $6)",
                )
                .bind(org_id)
                .bind(&l.name)
                .bind(&l.description)
                .bind(policy.as_str())
                .bind(l.managed)
                .bind(now_ms())
                .execute(&mut *c)
                .await?;
                list_by_name(c, org_id, &l.name).await?.ok_or_else(|| StoreError::NotFound("list".into()))
            }

            pub async fn delete_list(c: &mut Conn, org_id: i64, name: &str) -> Result<()> {
                let list = list_by_name(c, org_id, name)
                    .await?
                    .ok_or_else(|| StoreError::NotFound(format!("list {name:?}")))?;
                sqlx::query("DELETE FROM list_members WHERE list_id = $1")
                    .bind(list.id)
                    .execute(&mut *c)
                    .await?;
                sqlx::query("DELETE FROM lists WHERE id = $1").bind(list.id).execute(&mut *c).await?;
                Ok(())
            }

            pub async fn list_members(c: &mut Conn, list_id: i64) -> Result<Vec<Member>> {
                let rows: Vec<MemberRow> = sqlx::query_as(
                    "SELECT address, seat_id, mode FROM list_members WHERE list_id = $1 ORDER BY address",
                )
                .bind(list_id)
                .fetch_all(&mut *c)
                .await?;
                collect(rows, MemberRow::into_member)
            }

            pub async fn put_member(c: &mut Conn, org_id: i64, list_id: i64, m: &Member) -> Result<()> {
                sqlx::query(
                    "INSERT INTO list_members (org_id, list_id, address, seat_id, mode, created_at) \
                     VALUES ($1, $2, $3, $4, $5, $6) \
                     ON CONFLICT (list_id, address) DO UPDATE SET mode = excluded.mode, seat_id = excluded.seat_id",
                )
                .bind(org_id)
                .bind(list_id)
                .bind(&m.address)
                .bind(m.seat_id)
                .bind(m.mode.as_str())
                .bind(now_ms())
                .execute(&mut *c)
                .await?;
                Ok(())
            }

            pub async fn remove_member(c: &mut Conn, list_id: i64, address: &str) -> Result<bool> {
                let n = sqlx::query("DELETE FROM list_members WHERE list_id = $1 AND address = $2")
                    .bind(list_id)
                    .bind(address)
                    .execute(&mut *c)
                    .await?
                    .rows_affected();
                Ok(n > 0)
            }

            pub async fn link_state(c: &mut Conn, org_id: i64, peer_id: i64) -> Result<Option<LinkState>> {
                let s: Option<String> =
                    sqlx::query_scalar("SELECT state FROM links WHERE org_id = $1 AND peer_org_id = $2")
                        .bind(org_id)
                        .bind(peer_id)
                        .fetch_optional(&mut *c)
                        .await?;
                s.map(|s| parse(&s)).transpose()
            }

            async fn set_link(c: &mut Conn, org_id: i64, peer_id: i64, state: LinkState) -> Result<()> {
                sqlx::query(
                    "INSERT INTO links (org_id, peer_org_id, state, created_at) VALUES ($1, $2, $3, $4) \
                     ON CONFLICT (org_id, peer_org_id) DO UPDATE SET state = excluded.state",
                )
                .bind(org_id)
                .bind(peer_id)
                .bind(state.as_str())
                .bind(now_ms())
                .execute(&mut *c)
                .await?;
                Ok(())
            }

            /// Drop `org_id`'s request to the name of `peer_id` made while no
            /// org had that name: a link with the org itself replaces it.
            async fn drop_unknown_link(c: &mut Conn, org_id: i64, peer_id: i64) -> Result<()> {
                sqlx::query(
                    "DELETE FROM link_requests WHERE org_id = $1 \
                     AND peer_name = (SELECT name FROM orgs WHERE id = $2)",
                )
                .bind(org_id)
                .bind(peer_id)
                .execute(&mut *c)
                .await?;
                Ok(())
            }

            pub async fn request_link(c: &mut Conn, org_id: i64, peer_id: i64) -> Result<LinkState> {
                if org_id == peer_id {
                    return Err(StoreError::Invalid("an org cannot link to itself".into()));
                }
                drop_unknown_link(c, org_id, peer_id).await?;
                match link_state(c, org_id, peer_id).await? {
                    None => {
                        set_link(c, org_id, peer_id, LinkState::Requested).await?;
                        set_link(c, peer_id, org_id, LinkState::Invited).await?;
                        Ok(LinkState::Requested)
                    }
                    Some(LinkState::Invited) => {
                        set_link(c, org_id, peer_id, LinkState::Active).await?;
                        set_link(c, peer_id, org_id, LinkState::Active).await?;
                        Ok(LinkState::Active)
                    }
                    Some(s) => Ok(s),
                }
            }

            pub async fn accept_link(c: &mut Conn, org_id: i64, peer_id: i64) -> Result<()> {
                match link_state(c, org_id, peer_id).await? {
                    Some(LinkState::Invited) => {
                        drop_unknown_link(c, org_id, peer_id).await?;
                        set_link(c, org_id, peer_id, LinkState::Active).await?;
                        set_link(c, peer_id, org_id, LinkState::Active).await
                    }
                    Some(LinkState::Active) => Ok(()),
                    _ => Err(StoreError::NotFound("no pending link request from that org".into())),
                }
            }

            pub async fn revoke_link(c: &mut Conn, org_id: i64, peer_id: i64) -> Result<bool> {
                let n = sqlx::query(
                    "DELETE FROM links WHERE (org_id = $1 AND peer_org_id = $2) OR (org_id = $2 AND peer_org_id = $1)",
                )
                .bind(org_id)
                .bind(peer_id)
                .execute(&mut *c)
                .await?
                .rows_affected();
                Ok(n > 0)
            }

            pub async fn links(c: &mut Conn, org_id: i64) -> Result<Vec<Link>> {
                let rows: Vec<LinkRow> = sqlx::query_as(
                    "SELECT l.peer_org_id, o.name AS peer_name, l.state FROM links l \
                     JOIN orgs o ON o.id = l.peer_org_id WHERE l.org_id = $1 \
                     UNION ALL SELECT CAST(0 AS BIGINT), r.peer_name, 'requested' \
                     FROM link_requests r WHERE r.org_id = $1 AND NOT EXISTS (SELECT 1 FROM links l2 \
                     JOIN orgs o2 ON o2.id = l2.peer_org_id WHERE l2.org_id = $1 AND o2.name = r.peer_name) \
                     ORDER BY 2",
                )
                .bind(org_id)
                .fetch_all(&mut *c)
                .await?;
                collect(rows, LinkRow::into_link)
            }

            pub async fn request_unknown_link(c: &mut Conn, org_id: i64, name: &str) -> Result<()> {
                sqlx::query(
                    "INSERT INTO link_requests (org_id, peer_name, created_at) VALUES ($1, $2, $3) \
                     ON CONFLICT (org_id, peer_name) DO NOTHING",
                )
                .bind(org_id)
                .bind(name)
                .bind(now_ms())
                .execute(&mut *c)
                .await?;
                Ok(())
            }

            pub async fn revoke_unknown_link(c: &mut Conn, org_id: i64, name: &str) -> Result<bool> {
                let n = sqlx::query("DELETE FROM link_requests WHERE org_id = $1 AND peer_name = $2")
                    .bind(org_id)
                    .bind(name)
                    .execute(&mut *c)
                    .await?
                    .rows_affected();
                Ok(n > 0)
            }

            pub async fn message(c: &mut Conn, id: i64) -> Result<Option<MessageRec>> {
                let r: Option<MsgRow> =
                    sqlx::query_as(concat!("SELECT ", msg_cols!(), ", m.body FROM messages m", msg_joins!(), " WHERE m.id = $1"))
                        .bind(id)
                        .fetch_optional(&mut *c)
                        .await?;
                r.map(MsgRow::into_message).transpose()
            }

            pub async fn org_sees_message(c: &mut Conn, org_id: i64, message_id: i64) -> Result<bool> {
                let r: Option<i64> = sqlx::query_scalar(
                    "SELECT CAST(1 AS BIGINT) FROM messages WHERE id = $1 AND org_id = $2 \
                     UNION ALL SELECT CAST(1 AS BIGINT) FROM deliveries WHERE message_id = $1 AND org_id = $2 LIMIT 1",
                )
                .bind(message_id)
                .bind(org_id)
                .fetch_optional(&mut *c)
                .await?;
                Ok(r.is_some())
            }

            /// One lookup in `thread_seats` and one in `messages_thread_sender`.
            /// (Each as a subquery ordered like its index, so that both
            /// planners read the index whatever their statistics say.)
            pub async fn seat_in_thread(c: &mut Conn, seat_id: i64, thread_id: i64) -> Result<bool> {
                let r: (Option<i64>, Option<i64>) = sqlx::query_as(
                    "SELECT (SELECT t.seat_id FROM thread_seats t, seats s WHERE s.id = $2 \
                             AND t.thread_id = $1 AND t.org_id = s.org_id AND t.seat_id = $2 \
                             ORDER BY t.thread_id, t.org_id, t.seat_id LIMIT 1), \
                            (SELECT m.id FROM messages m, seats s WHERE s.id = $2 \
                             AND m.thread_id = $1 AND m.org_id = s.org_id AND m.sender_seat_id = $2 \
                             ORDER BY m.thread_id, m.org_id, m.sender_seat_id, m.id LIMIT 1)",
                )
                .bind(thread_id)
                .bind(seat_id)
                .fetch_one(&mut *c)
                .await?;
                Ok(r.0.is_some() || r.1.is_some())
            }

            /// Whether the org sent or received a message of the thread: one
            /// lookup in `thread_seats` and one in `messages_thread_sender`.
            pub async fn org_sees_thread(c: &mut Conn, org_id: i64, thread_id: i64) -> Result<bool> {
                let r: (Option<i64>, Option<i64>) = sqlx::query_as(
                    "SELECT (SELECT seat_id FROM thread_seats WHERE thread_id = $1 AND org_id = $2 \
                             ORDER BY thread_id, org_id, seat_id LIMIT 1), \
                            (SELECT id FROM messages WHERE thread_id = $1 AND org_id = $2 \
                             ORDER BY thread_id, org_id, sender_seat_id, id LIMIT 1)",
                )
                .bind(thread_id)
                .bind(org_id)
                .fetch_one(&mut *c)
                .await?;
                Ok(r.0.is_some() || r.1.is_some())
            }

            pub async fn delivery(c: &mut Conn, message_id: i64, seat_id: i64) -> Result<Option<DeliveryRec>> {
                let r: Option<DelivRow> = sqlx::query_as(concat!(
                    "SELECT ", deliv_cols!(), " FROM deliveries d", deliv_joins!(),
                    " WHERE d.message_id = $1 AND d.seat_id = $2"
                ))
                .bind(message_id)
                .bind(seat_id)
                .fetch_optional(&mut *c)
                .await?;
                r.map(DelivRow::into_delivery).transpose()
            }

            pub async fn thread(c: &mut Conn, thread_id: i64, org_id: i64, limit: i64, body_bytes: usize) -> Result<Vec<ThreadMessage>> {
                // A thread the org cannot see costs what a missing one does.
                if !org_sees_thread(c, org_id, thread_id).await? {
                    return Ok(Vec::new());
                }
                let w = thread_window(c, thread_id, i64::MAX, MAX_SCAN_MESSAGES).await?;
                let Some(lo) = w.lo else {
                    return Ok(Vec::new());
                };
                // Headers and body sizes first (the size is read without
                // the body on both backends).
                let rows: Vec<SizedRow> = sqlx::query_as(concat!(
                    "SELECT ", msg_cols!(), ", '' AS body, CAST(octet_length(m.body) AS BIGINT) AS body_bytes \
                     FROM messages m", msg_joins!(),
                    " WHERE m.thread_id = $1 AND m.id >= $3 AND (m.org_id = $2 OR EXISTS \
                     (SELECT 1 FROM deliveries d WHERE d.message_id = m.id AND d.org_id = $2)) ORDER BY m.id DESC LIMIT $4"
                ))
                .bind(thread_id)
                .bind(org_id)
                .bind(lo)
                .bind(limit)
                .fetch_all(&mut *c)
                .await?;
                // Bodies newest first within the budget.
                let mut left = body_bytes as i64;
                let mut out = Vec::with_capacity(rows.len());
                let mut want = Vec::new();
                for r in rows {
                    let size = r.body_bytes.unwrap_or(0);
                    let m = r.msg.into_message()?;
                    let fits = size <= left;
                    if fits {
                        left -= size;
                        if size > 0 {
                            want.push(m.id);
                        }
                    }
                    out.push(ThreadMessage { message: m, body_omitted: !fits });
                }
                if !want.is_empty() {
                    let sql = format!("SELECT id, body FROM messages WHERE id IN ({})", placeholders(1, want.len()));
                    let mut query = sqlx::query_as::<_, (i64, String)>(&sql);
                    for id in &want {
                        query = query.bind(*id);
                    }
                    for (id, body) in query.fetch_all(&mut *c).await? {
                        if let Some(t) = out.iter_mut().find(|t| t.message.id == id) {
                            t.message.body = body;
                        }
                    }
                }
                out.reverse();
                Ok(out)
            }

            /// With `unread_only`, the query names `read_at IS NULL` itself, so
            /// that it reads the `deliveries_unread` index and its cost
            /// follows the page size, not the seat's read mail.
            pub async fn inbox(c: &mut Conn, seat_id: i64, q: &InboxQuery) -> Result<Vec<Entry>> {
                let unread = if q.unread_only { " AND d.read_at IS NULL" } else { "" };
                let rows: Vec<EntryRow> = if let Some(before) = q.before_seq {
                    let sql = format!("{} WHERE d.seat_id = $1 AND d.seq < $2{unread} ORDER BY d.seq DESC LIMIT $3", entry_select!());
                    let mut rows: Vec<EntryRow> = sqlx::query_as(&sql)
                        .bind(seat_id)
                        .bind(before)
                        .bind(q.limit)
                        .fetch_all(&mut *c)
                        .await?;
                    rows.reverse();
                    rows
                } else {
                    let sql = format!("{} WHERE d.seat_id = $1 AND d.seq > $2{unread} ORDER BY d.seq LIMIT $3", entry_select!());
                    sqlx::query_as(&sql)
                        .bind(seat_id)
                        .bind(q.after_seq.unwrap_or(0))
                        .bind(q.limit)
                        .fetch_all(&mut *c)
                        .await?
                };
                collect(rows, EntryRow::into_entry)
            }

            /// Advance a seat's counter by `n`; returns the new value.
            pub async fn bump_seq(c: &mut Conn, seat_id: i64, n: i64) -> Result<i64> {
                Ok(sqlx::query_scalar("UPDATE seats SET last_seq = last_seq + $1 WHERE id = $2 RETURNING last_seq")
                    .bind(n)
                    .bind(seat_id)
                    .fetch_one(&mut *c)
                    .await?)
            }

            /// Apply a single-delivery state change (`set` is an UPDATE that
            /// binds `now`, message id, seat id and returns the delivery id
            /// if it changed anything), then give the delivery a new mod_seq.
            async fn touch(
                c: &mut Conn,
                set: &'static str,
                message_id: i64,
                seat_id: i64,
            ) -> Result<Option<(DeliveryRec, Option<SeqBump>)>> {
                let changed: Option<i64> = sqlx::query_scalar(set)
                    .bind(now_ms())
                    .bind(message_id)
                    .bind(seat_id)
                    .fetch_optional(&mut *c)
                    .await?;
                let bump = match changed {
                    Some(id) => {
                        let seq = bump_seq(c, seat_id, 1).await?;
                        sqlx::query("UPDATE deliveries SET mod_seq = $1 WHERE id = $2")
                            .bind(seq)
                            .bind(id)
                            .execute(&mut *c)
                            .await?;
                        Some(seq)
                    }
                    None => None,
                };
                Ok(delivery(c, message_id, seat_id).await?.map(|d| {
                    let b = bump.map(|seq| SeqBump { org_id: d.org_id, seat_id, seq });
                    (d, b)
                }))
            }

            pub async fn mark_read(c: &mut Conn, message_id: i64, seat_id: i64) -> Result<Option<(DeliveryRec, Option<SeqBump>)>> {
                touch(
                    c,
                    "UPDATE deliveries SET read_at = $1 WHERE message_id = $2 AND seat_id = $3 \
                     AND read_at IS NULL RETURNING id",
                    message_id,
                    seat_id,
                )
                .await
            }

            pub async fn ack(c: &mut Conn, message_id: i64, seat_id: i64) -> Result<Option<(DeliveryRec, Option<SeqBump>)>> {
                touch(
                    c,
                    "UPDATE deliveries SET acked_at = $1, read_at = COALESCE(read_at, $1) \
                     WHERE message_id = $2 AND seat_id = $3 AND acked_at IS NULL RETURNING id",
                    message_id,
                    seat_id,
                )
                .await
            }

            pub async fn changes(c: &mut Conn, seat_id: i64, since: i64, limit: i64) -> Result<Vec<Entry>> {
                let rows: Vec<EntryRow> =
                    sqlx::query_as(concat!(entry_select!(), " WHERE d.seat_id = $1 AND d.mod_seq > $2 ORDER BY d.mod_seq LIMIT $3"))
                        .bind(seat_id)
                        .bind(since)
                        .bind(limit)
                        .fetch_all(&mut *c)
                        .await?;
                collect(rows, EntryRow::into_entry)
            }

            pub async fn recipients(c: &mut Conn, message_id: i64) -> Result<Vec<DeliveryRec>> {
                let rows: Vec<DelivRow> = sqlx::query_as(concat!(
                    "SELECT ", deliv_cols!(), " FROM deliveries d", deliv_joins!(),
                    " WHERE d.message_id = $1 ORDER BY dorg.name, ds.name"
                ))
                .bind(message_id)
                .fetch_all(&mut *c)
                .await?;
                collect(rows, DelivRow::into_delivery)
            }

            /// Ids of the org's seats called `name` (a deleted seat's name
            /// can be taken again).
            async fn own_seat_ids(c: &mut Conn, org_id: i64, name: &str) -> Result<Vec<i64>> {
                Ok(sqlx::query_scalar("SELECT id FROM seats WHERE org_id = $1 AND name = $2")
                    .bind(org_id)
                    .bind(name.to_string())
                    .fetch_all(&mut *c)
                    .await?)
            }

            /// The archive. Every query starts from the org's own data: one
            /// of its seats' sent mail or mailbox when `from` or `to` names
            /// one, else the org's messages (or a thread's) newest first in
            /// steps, at most [`MAX_SCAN_MESSAGES`] per request and no further
            /// step once `q.time_limit` has passed. Built with
            /// only the conditions the query has, so that both planners can
            /// use the id order and the indexes (SQLite cannot see through
            /// `$n IS NULL OR ...`).
            pub async fn archive(c: &mut Conn, org_id: i64, q: &ArchiveQuery) -> Result<ArchivePage> {
                let empty = Ok(ArchivePage::default());
                // A time window as an id range (ids grow with creation time;
                // a message's id and timestamp are taken within one send, so
                // the margin covers any reordering).
                let lo_t = match q.since {
                    Some(t) => match id_at(c, t - ID_TIME_MARGIN_MS).await? {
                        Some(x) => Some(x),
                        None => return empty,
                    },
                    None => None,
                };
                let hi_t = match q.until {
                    Some(t) => id_at(c, t + ID_TIME_MARGIN_MS).await?,
                    None => None,
                };
                let upper = q.before_id.unwrap_or(i64::MAX).min(hi_t.unwrap_or(i64::MAX));
                let me: String = sqlx::query_scalar("SELECT name FROM orgs WHERE id = $1")
                    .bind(org_id)
                    .fetch_one(&mut *c)
                    .await?;
                let own = |a: &Option<(String, String)>| a.as_ref().filter(|(o, _)| *o == me).map(|(_, n)| n.clone());
                let own_from = match own(&q.from) {
                    Some(n) => Some(own_seat_ids(c, org_id, &n).await?),
                    None => None,
                };
                let own_to = match own(&q.to) {
                    Some(n) => Some(own_seat_ids(c, org_id, &n).await?),
                    None => None,
                };
                if own_from.as_ref().is_some_and(Vec::is_empty) || own_to.as_ref().is_some_and(Vec::is_empty) {
                    return empty;
                }
                let filters = |b: &mut sqlx::QueryBuilder<'_, Db>| {
                    // `+ 0`: filter by time, but scan by id.
                    if let Some(t) = q.since {
                        b.push(" AND m.created_at + 0 >= ").push_bind(t);
                    }
                    if let Some(t) = q.until {
                        b.push(" AND m.created_at + 0 < ").push_bind(t);
                    }
                    if let Some(x) = lo_t {
                        b.push(" AND m.id >= ").push_bind(x);
                    }
                    if let Some(t) = q.thread_id {
                        b.push(" AND m.thread_id = ").push_bind(t);
                    }
                    match (&own_from, &q.from) {
                        (Some(ids), _) => {
                            b.push(" AND m.sender_seat_id IN (");
                            let mut l = b.separated(", ");
                            for id in ids {
                                l.push_bind(*id);
                            }
                            b.push(")");
                        }
                        (None, Some((o, n))) => {
                            b.push(" AND m.sender_seat_id IN (SELECT fs.id FROM seats fs JOIN orgs fo ON fo.id = fs.org_id WHERE fo.name = ")
                                .push_bind(o.clone())
                                .push(" AND fs.name = ")
                                .push_bind(n.clone())
                                .push(")");
                        }
                        _ => {}
                    }
                    match (&own_to, &q.to) {
                        (Some(ids), _) => {
                            b.push(" AND EXISTS (SELECT 1 FROM deliveries td WHERE td.message_id = m.id AND td.seat_id IN (");
                            let mut l = b.separated(", ");
                            for id in ids {
                                l.push_bind(*id);
                            }
                            b.push("))");
                        }
                        // Another org's seat: only messages that name it.
                        // Which of that org's seats a list reached is that
                        // org's business.
                        (None, Some((o, n))) => {
                            b.push(" AND (',' || m.to_addrs || ',' || m.cc_addrs || ',') LIKE ")
                                .push_bind(addr_pattern(&format!("{n}@{o}")))
                                .push(" ESCAPE '\\'");
                        }
                        _ => {}
                    }
                };
                let select = concat!("SELECT ", msg_cols!(), ", '' AS body FROM ");
                let page = |mut v: Vec<MessageRec>, limit: i64| {
                    v.sort_by(|a, b| b.id.cmp(&a.id));
                    v.dedup_by_key(|m| m.id);
                    v.truncate(limit as usize);
                    let next_before_id = (v.len() as i64 == limit).then(|| v.last().map(|m| m.id)).flatten();
                    ArchivePage { messages: v, next_before_id }
                };
                // One of the org's seats: its sent mail, or its mailbox.
                if let Some(ids) = own_from.as_ref().or(own_to.as_ref()) {
                    let mut all = Vec::new();
                    for id in ids {
                        let mut b = sqlx::QueryBuilder::<Db>::new(select);
                        if own_from.is_some() {
                            b.push("messages m").push(msg_joins!()).push(" WHERE m.sender_seat_id = ").push_bind(*id);
                        } else {
                            b.push("deliveries sd JOIN messages m ON m.id = sd.message_id")
                                .push(msg_joins!())
                                .push(" WHERE sd.seat_id = ")
                                .push_bind(*id);
                        }
                        b.push(" AND m.id < ").push_bind(upper);
                        filters(&mut b);
                        // A seat's deliveries are numbered in message order.
                        b.push(if own_from.is_some() { " ORDER BY m.id DESC LIMIT " } else { " ORDER BY sd.seq DESC LIMIT " })
                            .push_bind(q.limit);
                        let rows: Vec<MsgRow> = b.build_query_as().fetch_all(&mut *c).await?;
                        all.extend(collect(rows, MsgRow::into_message)?);
                    }
                    return Ok(page(all, q.limit));
                }
                // Steps over the org's messages (or the thread's).
                let mut out: Vec<MessageRec> = Vec::new();
                let mut before = upper;
                let mut seen = 0;
                let started = std::time::Instant::now();
                while seen < MAX_SCAN_MESSAGES {
                    if seen > 0 && q.time_limit.is_some_and(|t| started.elapsed() >= t) {
                        break;
                    }
                    let n = STEP_MESSAGES.min(MAX_SCAN_MESSAGES - seen);
                    let w = match q.thread_id {
                        Some(t) => thread_window(c, t, before, n).await?,
                        None => scan_window(c, org_id, before, n).await?,
                    };
                    let Some(lo) = w.lo else {
                        return Ok(ArchivePage { messages: out, next_before_id: None });
                    };
                    let mut b = sqlx::QueryBuilder::<Db>::new(select);
                    b.push("messages m").push(msg_joins!()).push(" WHERE m.id < ").push_bind(before);
                    b.push(" AND m.id >= ").push_bind(lo);
                    match q.thread_id {
                        Some(t) => {
                            b.push(" AND m.thread_id = ").push_bind(t);
                            b.push(" AND (m.org_id = ").push_bind(org_id);
                            b.push(" OR EXISTS (SELECT 1 FROM deliveries d WHERE d.message_id = m.id AND d.org_id = ")
                                .push_bind(org_id)
                                .push("))");
                        }
                        None => {
                            b.push(" AND m.id IN (SELECT id FROM messages WHERE org_id = ").push_bind(org_id);
                            b.push(" AND id >= ").push_bind(lo).push(" AND id < ").push_bind(before);
                            b.push(" UNION SELECT message_id FROM deliveries WHERE org_id = ").push_bind(org_id);
                            b.push(" AND message_id >= ").push_bind(lo).push(" AND message_id < ").push_bind(before);
                            b.push(")");
                        }
                    }
                    filters(&mut b);
                    b.push(" ORDER BY m.id DESC LIMIT ").push_bind(q.limit - out.len() as i64);
                    let rows: Vec<MsgRow> = b.build_query_as().fetch_all(&mut *c).await?;
                    out.extend(collect(rows, MsgRow::into_message)?);
                    if out.len() as i64 >= q.limit {
                        let next_before_id = out.last().map(|m| m.id);
                        return Ok(ArchivePage { messages: out, next_before_id });
                    }
                    // Nothing older can match: the window reached the start
                    // of the time range, or the org's (thread's) first message.
                    if w.exhausted || lo_t.is_some_and(|t| lo <= t) {
                        return Ok(ArchivePage { messages: out, next_before_id: None });
                    }
                    seen += w.count;
                    before = lo;
                }
                Ok(ArchivePage { messages: out, next_before_id: Some(before) })
            }

            pub async fn seat_seqs(c: &mut Conn, org_id: i64) -> Result<Vec<(i64, String, i64)>> {
                Ok(sqlx::query_as(
                    "SELECT id, name, last_seq FROM seats WHERE org_id = $1 AND deleted_at IS NULL ORDER BY name",
                )
                .bind(org_id)
                .fetch_all(&mut *c)
                .await?)
            }

            pub async fn tokens(c: &mut Conn, org_id: i64) -> Result<Vec<TokenRec>> {
                let rows: Vec<TokenRow> = sqlx::query_as(
                    "SELECT t.id, t.scope, s.name AS seat_name, t.label, t.created_at FROM tokens t \
                     LEFT JOIN seats s ON s.id = t.seat_id \
                     WHERE t.org_id = $1 AND t.revoked_at IS NULL ORDER BY t.id",
                )
                .bind(org_id)
                .fetch_all(&mut *c)
                .await?;
                collect(rows, TokenRow::into_token)
            }

            pub async fn revoke_token(c: &mut Conn, org_id: i64, id: i64) -> Result<bool> {
                sqlx::query("DELETE FROM web_sessions WHERE token_id = (SELECT id FROM tokens WHERE id = $1 AND org_id = $2)")
                    .bind(id)
                    .bind(org_id)
                    .execute(&mut *c)
                    .await?;
                let n = sqlx::query(
                    "UPDATE tokens SET revoked_at = $1 WHERE org_id = $2 AND id = $3 AND revoked_at IS NULL",
                )
                .bind(now_ms())
                .bind(org_id)
                .bind(id)
                .execute(&mut *c)
                .await?
                .rows_affected();
                Ok(n > 0)
            }

            /// From the seats' counters (kept by triggers on `deliveries`).
            pub async fn pending(c: &mut Conn, org_id: i64, seat_id: Option<i64>) -> Result<Vec<PendingRec>> {
                let rows: Vec<PendingRow> = sqlx::query_as(
                    "SELECT s.id AS seat_id, s.name AS seat_name, s.kind, s.last_seq, s.focus_until, \
                     s.pending_to AS to_n, s.pending_cc AS cc_n, s.pending_list AS list_n, \
                     s.pending_urgent AS urgent_n, s.pending_digest AS digest_n, \
                     s.pending_digest_since AS digest_since \
                     FROM seats s WHERE s.org_id = $1 AND s.deleted_at IS NULL AND ($2 IS NULL OR s.id = $2) \
                     ORDER BY s.name",
                )
                .bind(org_id)
                .bind(seat_id)
                .fetch_all(&mut *c)
                .await?;
                Ok(rows.into_iter().map(PendingRec::from).collect())
            }

            /// Ids of claimable deliveries. `sql` is `claim_select!()`,
            /// possibly with a locking suffix. `key`: see `claim_select!`.
            pub async fn claim_candidates(c: &mut Conn, sql: &'static str, seat_id: i64, f: &ClaimFilter, key: Option<&str>) -> Result<Vec<i64>> {
                use amail_proto::DeliveryClass as C;
                Ok(sqlx::query_scalar(sql)
                    .bind(seat_id)
                    .bind(mode_mask(f))
                    .bind(f.has(C::To))
                    .bind(f.has(C::Cc))
                    .bind(f.has(C::List))
                    .bind(f.urgent_only)
                    .bind(f.limit)
                    .bind(key)
                    .fetch_all(&mut *c)
                    .await?)
            }

            /// The deliveries an earlier claim with `key` took, if there was one.
            pub async fn claimed_with_key(c: &mut Conn, seat_id: i64, key: &str) -> Result<Option<Vec<i64>>> {
                let ids: Option<String> = sqlx::query_scalar("SELECT ids FROM claim_keys WHERE seat_id = $1 AND key = $2")
                    .bind(seat_id)
                    .bind(key)
                    .fetch_optional(&mut *c)
                    .await?;
                Ok(ids.map(|s| parse_ids(&s)))
            }

            /// Entries (with bodies) of the seat's deliveries with these ids, by seq.
            pub async fn entries_by_ids(c: &mut Conn, seat_id: i64, ids: &[i64]) -> Result<Vec<Entry>> {
                if ids.is_empty() {
                    return Ok(Vec::new());
                }
                let sql = format!(
                    "{} WHERE d.seat_id = $1 AND d.id IN ({}) ORDER BY d.seq",
                    entry_select_body!(),
                    placeholders(2, ids.len())
                );
                let mut q = sqlx::query_as::<_, EntryRow>(&sql).bind(seat_id);
                for id in ids {
                    q = q.bind(*id);
                }
                collect(q.fetch_all(&mut *c).await?, EntryRow::into_entry)
            }

            // --- presence ----------------------------------------------------

            pub async fn presence(c: &mut Conn, org_id: i64, seat: Option<&str>) -> Result<Vec<PresenceRec>> {
                let rows: Vec<PresenceRow> = sqlx::query_as(
                    "SELECT s.name AS seat, p.state, p.wake_failed, p.harness, p.node, p.since, p.updated_at, \
                     p.attention, p.attention_hint, p.terminal_url \
                     FROM presence p JOIN seats s ON s.id = p.seat_id \
                     WHERE p.org_id = $1 AND s.deleted_at IS NULL AND ($2 IS NULL OR s.name = $2) ORDER BY s.name",
                )
                .bind(org_id)
                .bind(seat)
                .fetch_all(&mut *c)
                .await?;
                collect(rows, PresenceRow::into_rec)
            }

            /// Store reports (`since` of an unchanged state is kept); returns
            /// the entries whose state changed or that were absent (no row,
            /// or older than `ttl_ms`), as stored.
            pub async fn put_presence(c: &mut Conn, org_id: i64, recs: &[PresenceRec], ttl_ms: i64) -> Result<Vec<PresenceRec>> {
                let now = now_ms();
                let names: Vec<String> = recs.iter().map(|r| r.seat.clone()).collect();
                let ids: std::collections::HashMap<String, i64> = seats_by_names(c, org_id, &names)
                    .await?
                    .into_iter()
                    .map(|s| (s.name, s.id))
                    .collect();
                if ids.is_empty() {
                    return Ok(Vec::new());
                }
                let prev: std::collections::HashMap<String, PresenceRec> = presence(c, org_id, None)
                    .await?
                    .into_iter()
                    .filter(|p| ids.contains_key(&p.seat))
                    .map(|p| (p.seat.clone(), p))
                    .collect();
                let mut stored = Vec::new();
                let mut changed = Vec::new();
                for r in recs {
                    let Some(&seat_id) = ids.get(&r.seat) else { continue };
                    let old = prev.get(&r.seat).filter(|p| now - p.updated_at <= ttl_ms);
                    let mut rec = r.clone();
                    rec.updated_at = now;
                    rec.since = match old {
                        Some(o) if o.state == r.state => o.since,
                        _ => r.since.min(now),
                    };
                    if old.is_none_or(|o| !o.same_as(&rec)) {
                        changed.push(rec.clone());
                    }
                    stored.push((seat_id, rec));
                }
                for chunk in stored.chunks(500) {
                    let rows: Vec<String> = (0..chunk.len()).map(|i| format!("({})", placeholders(i * 11 + 1, 11))).collect();
                    let sql = format!(
                        "INSERT INTO presence (seat_id, org_id, state, wake_failed, harness, node, since, updated_at, \
                         attention, attention_hint, terminal_url) VALUES {} \
                         ON CONFLICT (seat_id) DO UPDATE SET state = excluded.state, wake_failed = excluded.wake_failed, \
                         harness = excluded.harness, node = excluded.node, since = excluded.since, \
                         updated_at = excluded.updated_at, attention = excluded.attention, \
                         attention_hint = excluded.attention_hint, terminal_url = excluded.terminal_url",
                        rows.join(", ")
                    );
                    let mut q = sqlx::query(&sql);
                    for (seat_id, r) in chunk {
                        q = q
                            .bind(*seat_id)
                            .bind(org_id)
                            .bind(r.state.as_str())
                            .bind(r.wake_failed)
                            .bind(r.harness.clone())
                            .bind(r.node.clone())
                            .bind(r.since)
                            .bind(r.updated_at)
                            .bind(r.attention.clone())
                            .bind(r.attention_hint.clone())
                            .bind(r.terminal_url.clone());
                    }
                    q.execute(&mut *c).await?;
                }
                Ok(changed)
            }

            // --- instances ---------------------------------------------------

            pub async fn heartbeat_instance(c: &mut Conn, id: &str, url: Option<&str>, now: i64) -> Result<()> {
                sqlx::query(
                    "INSERT INTO server_instances (id, url, started_at, heartbeat_at) VALUES ($1, $2, $3, $3) \
                     ON CONFLICT (id) DO UPDATE SET url = excluded.url, heartbeat_at = excluded.heartbeat_at",
                )
                .bind(id)
                .bind(url)
                .bind(now)
                .execute(&mut *c)
                .await?;
                Ok(())
            }

            pub async fn remove_instance(c: &mut Conn, id: &str) -> Result<()> {
                for sql in [
                    "DELETE FROM node_streams WHERE instance_id = $1",
                    "DELETE FROM mirror_watch WHERE instance_id = $1",
                    "DELETE FROM server_instances WHERE id = $1",
                ] {
                    sqlx::query(sql).bind(id).execute(&mut *c).await?;
                }
                Ok(())
            }

            pub async fn set_node_streams(c: &mut Conn, instance: &str, counts: &[(i64, i64)]) -> Result<()> {
                sqlx::query("DELETE FROM node_streams WHERE instance_id = $1")
                    .bind(instance)
                    .execute(&mut *c)
                    .await?;
                for chunk in counts.chunks(500) {
                    let rows: Vec<String> = (0..chunk.len()).map(|i| format!("($1, ${}, ${})", i * 2 + 2, i * 2 + 3)).collect();
                    let sql = format!("INSERT INTO node_streams (instance_id, org_id, streams) VALUES {}", rows.join(", "));
                    let mut q = sqlx::query(&sql).bind(instance);
                    for (org, n) in chunk {
                        q = q.bind(*org).bind(*n);
                    }
                    q.execute(&mut *c).await?;
                }
                Ok(())
            }

            pub async fn node_streams(c: &mut Conn, except: &str, fresh_since: i64) -> Result<Vec<(i64, i64)>> {
                Ok(sqlx::query_as(
                    "SELECT n.org_id, CAST(SUM(n.streams) AS BIGINT) FROM node_streams n \
                     JOIN server_instances i ON i.id = n.instance_id \
                     WHERE n.instance_id <> $1 AND i.heartbeat_at >= $2 GROUP BY n.org_id",
                )
                .bind(except)
                .bind(fresh_since)
                .fetch_all(&mut *c)
                .await?)
            }

            pub async fn watch_mirror(c: &mut Conn, org_id: i64, seat: &str, instance: &str, expires_at: Option<i64>) -> Result<()> {
                match expires_at {
                    Some(t) => sqlx::query(
                        "INSERT INTO mirror_watch (org_id, seat, instance_id, expires_at) VALUES ($1, $2, $3, $4) \
                         ON CONFLICT (org_id, seat, instance_id) DO UPDATE SET expires_at = excluded.expires_at",
                    )
                    .bind(org_id)
                    .bind(seat)
                    .bind(instance)
                    .bind(t),
                    None => sqlx::query("DELETE FROM mirror_watch WHERE org_id = $1 AND seat = $2 AND instance_id = $3")
                        .bind(org_id)
                        .bind(seat)
                        .bind(instance),
                }
                .execute(&mut *c)
                .await?;
                Ok(())
            }

            pub async fn mirror_watchers(c: &mut Conn, org_id: i64, seat: &str, now: i64) -> Result<Vec<(String, Option<String>)>> {
                Ok(sqlx::query_as(
                    "SELECT w.instance_id, i.url FROM mirror_watch w JOIN server_instances i ON i.id = w.instance_id \
                     WHERE w.org_id = $1 AND w.seat = $2 AND w.expires_at > $3",
                )
                .bind(org_id)
                .bind(seat)
                .bind(now)
                .fetch_all(&mut *c)
                .await?)
            }

            /// Drop expired claim keys, watches, instances and browser
            /// sessions, and revoked tokens with their sessions.
            pub async fn cleanup(c: &mut Conn, now: i64) -> Result<()> {
                sqlx::query("DELETE FROM web_sessions WHERE expires_at <= $1").bind(now).execute(&mut *c).await?;
                sqlx::query("DELETE FROM operator_sessions WHERE expires_at <= $1").bind(now).execute(&mut *c).await?;
                sqlx::query(
                    "DELETE FROM web_sessions WHERE token_id IN (SELECT id FROM tokens WHERE revoked_at IS NOT NULL)",
                )
                .execute(&mut *c)
                .await?;
                sqlx::query("DELETE FROM tokens WHERE revoked_at IS NOT NULL").execute(&mut *c).await?;
                sqlx::query("DELETE FROM claim_keys WHERE created_at < $1").bind(now - 600_000).execute(&mut *c).await?;
                sqlx::query("DELETE FROM mirror_watch WHERE expires_at < $1").bind(now).execute(&mut *c).await?;
                sqlx::query(
                    "DELETE FROM node_streams WHERE instance_id IN \
                     (SELECT id FROM server_instances WHERE heartbeat_at < $1)",
                )
                .bind(now - 300_000)
                .execute(&mut *c)
                .await?;
                sqlx::query("DELETE FROM server_instances WHERE heartbeat_at < $1").bind(now - 300_000).execute(&mut *c).await?;
                // Link requests that were not accepted in time, to existing
                // orgs (both rows of the pair) and to unknown names alike.
                let cutoff = now - amail_proto::limits::LINK_REQUEST_DAYS * 86_400_000;
                sqlx::query("DELETE FROM link_requests WHERE created_at < $1").bind(cutoff).execute(&mut *c).await?;
                sqlx::query("DELETE FROM links WHERE state IN ('requested', 'invited') AND created_at < $1")
                    .bind(cutoff)
                    .execute(&mut *c)
                    .await?;
                sqlx::query("DELETE FROM org_activity WHERE minute < $1")
                    .bind((now - 2 * 86_400_000) / 60_000)
                    .execute(&mut *c)
                    .await?;
                Ok(())
            }

            /// The ids of `tokens` that are live, and the hashes of
            /// `sessions` that are live (unexpired, with a live token).
            pub async fn live_credentials(c: &mut Conn, tokens: &[i64], sessions: &[String], now: i64) -> Result<(Vec<i64>, Vec<String>)> {
                let mut live_tokens = Vec::new();
                for chunk in tokens.chunks(500) {
                    let sql = format!(
                        "SELECT id FROM tokens WHERE revoked_at IS NULL AND id IN ({})",
                        placeholders(1, chunk.len())
                    );
                    let mut q = sqlx::query_scalar::<_, i64>(&sql);
                    for id in chunk {
                        q = q.bind(*id);
                    }
                    live_tokens.extend(q.fetch_all(&mut *c).await?);
                }
                let mut live_sessions = Vec::new();
                for chunk in sessions.chunks(500) {
                    let sql = format!(
                        "SELECT w.hash FROM web_sessions w JOIN tokens t ON t.id = w.token_id \
                         WHERE t.revoked_at IS NULL AND w.expires_at > $1 AND w.hash IN ({})",
                        placeholders(2, chunk.len())
                    );
                    let mut q = sqlx::query_scalar::<_, String>(&sql).bind(now);
                    for h in chunk {
                        q = q.bind(h.clone());
                    }
                    live_sessions.extend(q.fetch_all(&mut *c).await?);
                }
                Ok((live_tokens, live_sessions))
            }

            pub async fn row_counts(c: &mut Conn) -> Result<RowCounts> {
                let (tokens, revoked_tokens, web_sessions): (i64, i64, i64) = sqlx::query_as(
                    "SELECT (SELECT COUNT(*) FROM tokens), \
                     (SELECT COUNT(*) FROM tokens WHERE revoked_at IS NOT NULL), \
                     (SELECT COUNT(*) FROM web_sessions)",
                )
                .fetch_one(&mut *c)
                .await?;
                Ok(RowCounts { tokens, revoked_tokens, web_sessions })
            }

            // --- attention ---------------------------------------------------

            /// Set or clear a seat's focus; bumps its counter so that event
            /// subscribers (nodes) notice.
            pub async fn set_focus(c: &mut Conn, seat_id: i64, until: Option<i64>) -> Result<SeqBump> {
                let (org_id, seq): (i64, i64) = sqlx::query_as(
                    "UPDATE seats SET focus_until = $2, last_seq = last_seq + 1 WHERE id = $1 RETURNING org_id, last_seq",
                )
                .bind(seat_id)
                .bind(until)
                .fetch_one(&mut *c)
                .await?;
                Ok(SeqBump { org_id, seat_id, seq })
            }

            pub async fn system_seat_get(c: &mut Conn, org_id: i64) -> Result<Option<Seat>> {
                let r: Option<SeatRow> = sqlx::query_as(concat!(seat_select!(), " WHERE s.org_id = $1 AND s.kind = 'system'"))
                    .bind(org_id)
                    .fetch_optional(&mut *c)
                    .await?;
                r.map(SeatRow::into_seat).transpose()
            }

            /// The org's system seat, created (deleted, see the migration) if needed.
            pub async fn system_seat(c: &mut Conn, org_id: i64) -> Result<Seat> {
                if let Some(s) = system_seat_get(c, org_id).await? {
                    return Ok(s);
                }
                let now = now_ms();
                sqlx::query(
                    "INSERT INTO seats (org_id, name, kind, created_at, deleted_at) VALUES ($1, 'amail', 'system', $2, $2) \
                     ON CONFLICT DO NOTHING",
                )
                .bind(org_id)
                .bind(now)
                .execute(&mut *c)
                .await?;
                system_seat_get(c, org_id).await?.ok_or_else(|| StoreError::NotFound("system seat".into()))
            }

            pub async fn recipient_states(c: &mut Conn, ids: &[i64]) -> Result<Vec<RecipientState>> {
                if ids.is_empty() {
                    return Ok(Vec::new());
                }
                let sql = format!(
                    "SELECT s.id, s.name, s.kind, s.focus_until FROM seats s WHERE s.id IN ({})",
                    placeholders(1, ids.len())
                );
                let mut q = sqlx::query_as::<_, (i64, String, String, Option<i64>)>(&sql);
                for id in ids {
                    q = q.bind(*id);
                }
                q.fetch_all(&mut *c)
                    .await?
                    .into_iter()
                    .map(|(seat_id, name, kind, focus_until)| {
                        Ok(RecipientState { seat_id, name, kind: parse(&kind)?, focus_until })
                    })
                    .collect()
            }

            pub async fn due_acks(c: &mut Conn, now: i64, limit: i64) -> Result<Vec<AckDue>> {
                let rows: Vec<(i64, i64, i64, i64, i64, i64)> = sqlx::query_as(
                    "SELECT id, org_id, sender_seat_id, ack_stage, created_at, ack_due FROM messages \
                     WHERE ack_due IS NOT NULL AND ack_due <= $1 ORDER BY ack_due LIMIT $2",
                )
                .bind(now)
                .bind(limit)
                .fetch_all(&mut *c)
                .await?;
                Ok(rows
                    .into_iter()
                    .map(|(message_id, org_id, sender_seat_id, stage, created_at, due)| AckDue {
                        message_id, org_id, sender_seat_id, stage, created_at, due,
                    })
                    .collect())
            }

            pub async fn advance_ack(c: &mut Conn, message_id: i64, from_stage: i64, to_stage: i64, next_due: Option<i64>) -> Result<bool> {
                let r: Option<i64> = sqlx::query_scalar(
                    "UPDATE messages SET ack_stage = $3, ack_due = $4 \
                     WHERE id = $1 AND ack_stage = $2 AND ack_due IS NOT NULL RETURNING id",
                )
                .bind(message_id)
                .bind(from_stage)
                .bind(to_stage)
                .bind(next_due)
                .fetch_optional(&mut *c)
                .await?;
                Ok(r.is_some())
            }

            pub async fn laggards(c: &mut Conn, message_id: i64) -> Result<Vec<Laggard>> {
                let rows: Vec<(i64, i64, String, Option<i64>)> = sqlx::query_as(
                    "SELECT s.id, s.org_id, s.name, s.parent_id FROM deliveries d \
                     JOIN seats s ON s.id = d.seat_id JOIN messages m ON m.id = d.message_id \
                     WHERE d.message_id = $1 AND d.acked_at IS NULL AND d.seat_id <> m.sender_seat_id \
                     AND d.mode <> 'archive' ORDER BY s.name",
                )
                .bind(message_id)
                .fetch_all(&mut *c)
                .await?;
                Ok(rows
                    .into_iter()
                    .map(|(seat_id, org_id, name, parent_id)| Laggard { seat_id, org_id, name, parent_id })
                    .collect())
            }

            // --- public mode: invites, operator, org lifecycle, retention ----

            pub async fn create_invite(c: &mut Conn, hash: &str, note: Option<&str>, expires_at: Option<i64>) -> Result<i64> {
                Ok(sqlx::query_scalar(
                    "INSERT INTO invites (hash, note, created_at, expires_at) VALUES ($1, $2, $3, $4) RETURNING id",
                )
                .bind(hash)
                .bind(note)
                .bind(now_ms())
                .bind(expires_at)
                .fetch_one(&mut *c)
                .await?)
            }

            pub async fn invites(c: &mut Conn) -> Result<Vec<InviteRec>> {
                #[allow(clippy::type_complexity)]
                let rows: Vec<(i64, Option<String>, i64, Option<i64>, Option<i64>, Option<String>, Option<i64>, i64)> =
                    sqlx::query_as(
                        "SELECT id, note, created_at, expires_at, used_at, used_by, revoked_at, name_conflicts \
                         FROM invites ORDER BY id",
                    )
                    .fetch_all(&mut *c)
                    .await?;
                Ok(rows
                    .into_iter()
                    .map(|(id, note, created_at, expires_at, used_at, used_by, revoked_at, name_conflicts)| InviteRec {
                        id, note, created_at, expires_at, used_at, used_by, revoked_at, name_conflicts,
                    })
                    .collect())
            }

            pub async fn revoke_invite(c: &mut Conn, id: i64) -> Result<bool> {
                let n = sqlx::query(
                    "UPDATE invites SET revoked_at = $1 WHERE id = $2 AND revoked_at IS NULL AND used_at IS NULL",
                )
                .bind(now_ms())
                .bind(id)
                .execute(&mut *c)
                .await?
                .rows_affected();
                Ok(n > 0)
            }

            /// Use the invite and create the org with its user seat and user
            /// token. The invite is taken by one conditional update, so two
            /// registrations cannot both use it; if creating the org then
            /// fails, the whole job (and the use) is rolled back.
            pub async fn register(
                c: &mut Conn,
                invite_hash: &str,
                org: &str,
                user_seat: &str,
                token_hash: &str,
            ) -> Result<std::result::Result<(Org, Seat), InviteError>> {
                let now = now_ms();
                // The invite is checked first, so that without a valid one
                // nobody learns which org names are taken.
                let valid: Option<i64> = sqlx::query_scalar(
                    "SELECT id FROM invites WHERE hash = $1 AND used_at IS NULL \
                     AND revoked_at IS NULL AND (expires_at IS NULL OR expires_at > $2) \
                     AND name_conflicts < $3",
                )
                .bind(invite_hash)
                .bind(now)
                .bind(amail_proto::limits::MAX_INVITE_NAME_CONFLICTS)
                .fetch_optional(&mut *c)
                .await?;
                let Some(invite_id) = valid else {
                    return Ok(Err(InviteError::Invalid));
                };
                if org_by_name(c, org).await?.is_some() {
                    // Counted, so that one invite can test only a few names.
                    sqlx::query("UPDATE invites SET name_conflicts = name_conflicts + 1 WHERE id = $1")
                        .bind(invite_id)
                        .execute(&mut *c)
                        .await?;
                    return Ok(Err(InviteError::OrgExists));
                }
                let used: Option<i64> = sqlx::query_scalar(
                    "UPDATE invites SET used_at = $1, used_by = $2 WHERE hash = $3 AND used_at IS NULL \
                     AND revoked_at IS NULL AND (expires_at IS NULL OR expires_at > $1) RETURNING id",
                )
                .bind(now)
                .bind(org)
                .bind(invite_hash)
                .fetch_optional(&mut *c)
                .await?;
                if used.is_none() {
                    return Ok(Err(InviteError::Invalid));
                }
                create_org(c, org, user_seat, token_hash).await.map(Ok)
            }

            pub async fn create_operator_token(c: &mut Conn, hash: &str, label: Option<&str>) -> Result<i64> {
                Ok(sqlx::query_scalar(
                    "INSERT INTO operator_tokens (hash, label, created_at) VALUES ($1, $2, $3) RETURNING id",
                )
                .bind(hash)
                .bind(label)
                .bind(now_ms())
                .fetch_one(&mut *c)
                .await?)
            }

            pub async fn operator_tokens(c: &mut Conn) -> Result<Vec<OperatorTokenRec>> {
                let rows: Vec<(i64, Option<String>, i64)> = sqlx::query_as(
                    "SELECT id, label, created_at FROM operator_tokens WHERE revoked_at IS NULL ORDER BY id",
                )
                .fetch_all(&mut *c)
                .await?;
                Ok(rows.into_iter().map(|(id, label, created_at)| OperatorTokenRec { id, label, created_at }).collect())
            }

            pub async fn revoke_operator_token(c: &mut Conn, id: i64) -> Result<bool> {
                let n = sqlx::query("UPDATE operator_tokens SET revoked_at = $1 WHERE id = $2 AND revoked_at IS NULL")
                    .bind(now_ms())
                    .bind(id)
                    .execute(&mut *c)
                    .await?
                    .rows_affected();
                Ok(n > 0)
            }

            pub async fn operator_auth(c: &mut Conn, hash: &str) -> Result<Option<i64>> {
                Ok(sqlx::query_scalar("SELECT id FROM operator_tokens WHERE hash = $1 AND revoked_at IS NULL")
                    .bind(hash)
                    .fetch_optional(&mut *c)
                    .await?)
            }

            pub async fn operator_session_auth(c: &mut Conn, hash: &str, now: i64) -> Result<Option<i64>> {
                Ok(sqlx::query_scalar(
                    "SELECT t.id FROM operator_sessions w JOIN operator_tokens t ON t.id = w.token_id \
                     WHERE w.hash = $1 AND w.expires_at > $2 AND t.revoked_at IS NULL",
                )
                .bind(hash)
                .bind(now)
                .fetch_optional(&mut *c)
                .await?)
            }

            /// Like [`create_session`], for operator sessions.
            pub async fn create_operator_session(c: &mut Conn, hash: &str, token_id: i64, expires_at: i64) -> Result<()> {
                sqlx::query(
                    "INSERT INTO operator_sessions (hash, token_id, created_at, expires_at) VALUES ($1, $2, $3, $4)",
                )
                .bind(hash)
                .bind(token_id)
                .bind(now_ms())
                .bind(expires_at)
                .execute(&mut *c)
                .await?;
                sqlx::query(
                    "DELETE FROM operator_sessions WHERE token_id = $1 AND hash <> $3 AND hash NOT IN \
                     (SELECT hash FROM operator_sessions WHERE token_id = $1 AND hash <> $3 ORDER BY created_at DESC, hash LIMIT $2)",
                )
                .bind(token_id)
                .bind(amail_proto::limits::MAX_SESSIONS_PER_TOKEN - 1)
                .bind(hash)
                .execute(&mut *c)
                .await?;
                Ok(())
            }

            pub async fn delete_operator_session(c: &mut Conn, hash: &str) -> Result<bool> {
                let n = sqlx::query("DELETE FROM operator_sessions WHERE hash = $1")
                    .bind(hash)
                    .execute(&mut *c)
                    .await?
                    .rows_affected();
                Ok(n > 0)
            }

            pub async fn orgs(c: &mut Conn) -> Result<Vec<Org>> {
                let rows: Vec<OrgRow> = sqlx::query_as("SELECT * FROM orgs WHERE deleted_at IS NULL ORDER BY name").fetch_all(&mut *c).await?;
                Ok(rows.into_iter().map(Org::from).collect())
            }

            pub async fn set_org_suspended(c: &mut Conn, org_id: i64, at: Option<i64>) -> Result<bool> {
                let n = sqlx::query("UPDATE orgs SET suspended_at = $1 WHERE id = $2")
                    .bind(at)
                    .bind(org_id)
                    .execute(&mut *c)
                    .await?
                    .rows_affected();
                Ok(n > 0)
            }

            /// Delete the org and everything it owns, in one transaction:
            /// its seats, tokens and browser sessions, lists, links, its
            /// deliveries (of any org's mail) and the mail it sent that no
            /// other org received. External list members naming its seats
            /// are removed.
            ///
            /// Mail it sent that other orgs received stays with them. The
            /// org's row is then kept, marked deleted and renamed
            /// `deleted:<id>` (so the name is free), with the seats that
            /// sent such mail (deleted, with only their names); that mail
            /// shows as from `name@<org> (deleted org)`, and the addresses
            /// naming the org in the mail it sent or received are rewritten
            /// the same way, so that a new org of that name is never taken
            /// for the old one. Its acknowledgement reminders stop.
            ///
            /// Storage counts (see `message_bytes`): each org that keeps
            /// such mail is charged its full bytes and the org's own
            /// deliveries of it; the senders of mail the org received are no
            /// longer charged for its deliveries; rewritten addresses change
            /// the count of whoever is charged for the message.
            pub async fn delete_org(c: &mut Conn, org_id: i64) -> Result<bool> {
                let Some(org) = org_by_id(c, org_id).await? else {
                    return Ok(false);
                };
                let now = now_ms();
                sqlx::query(
                    "DELETE FROM list_members WHERE seat_id IS NULL \
                     AND substr(address, length(address) - length($1)) = '@' || $1",
                )
                .bind(&org.name)
                .execute(&mut *c)
                .await?;
                // Addresses naming the org, in the mail it sent or received
                // (the only mail that can name it).
                let pattern = like_pattern(&format!("@{}", org.name));
                let addrs: Vec<(i64, i64, String, String)> = sqlx::query_as(
                    "SELECT id, org_id, to_addrs, cc_addrs FROM messages \
                     WHERE (org_id = $1 OR id IN (SELECT message_id FROM deliveries WHERE org_id = $1)) \
                     AND (to_addrs LIKE $2 ESCAPE '\\' OR cc_addrs LIKE $2 ESCAPE '\\')",
                )
                .bind(org_id)
                .bind(&pattern)
                .fetch_all(&mut *c)
                .await?;
                let suffix = format!("@{}", org.name);
                let mark = |list: &str| -> String {
                    let v: Vec<String> = split_addrs(list)
                        .into_iter()
                        .map(|a| if a.ends_with(&suffix) { format!("{a} (deleted org)") } else { a })
                        .collect();
                    join_addrs(&v)
                };
                // Changes of the storage counts of other orgs, applied at
                // the end (in org id order, after all changes to messages).
                let mut charge: std::collections::BTreeMap<i64, i64> = Default::default();
                let mut live = Default::default();
                for (id, sender, to, cc) in addrs {
                    let (to2, cc2) = (mark(&to), mark(&cc));
                    if to2 != to || cc2 != cc {
                        let delta = (to2.len() + cc2.len()) as i64 - (to.len() + cc.len()) as i64;
                        for payer in payers(c, &mut live, id, sender, org_id).await? {
                            *charge.entry(payer).or_default() += delta;
                        }
                        sqlx::query("UPDATE messages SET to_addrs = $1, cc_addrs = $2 WHERE id = $3")
                            .bind(to2)
                            .bind(cc2)
                            .bind(id)
                            .execute(&mut *c)
                            .await?;
                    }
                }
                // Deleting its deliveries changes its seats' counters
                // (triggers): take the seats in id order first, as sending
                // does, so that a concurrent send cannot deadlock with this.
                if matches!(DIALECT, Dialect::Postgres) {
                    sqlx::query("SELECT id FROM seats WHERE org_id = $1 ORDER BY id FOR NO KEY UPDATE")
                        .bind(org_id)
                        .execute(&mut *c)
                        .await?;
                }
                // Its deliveries go; their senders are no longer charged
                // for them (counted on the rows actually deleted, so a send
                // committed meanwhile on Postgres is counted right).
                let per_delivery = amail_proto::limits::STORED_BYTES_PER_DELIVERY;
                let gone: Vec<(i64, i64)> = match DIALECT {
                    Dialect::Postgres => {
                        sqlx::query_as(
                            "WITH d AS (DELETE FROM deliveries WHERE org_id = $1 RETURNING message_id) \
                             SELECT m.org_id, CAST(COUNT(*) AS BIGINT) FROM d JOIN messages m ON m.id = d.message_id \
                             WHERE m.org_id <> $1 GROUP BY m.org_id",
                        )
                        .bind(org_id)
                        .fetch_all(&mut *c)
                        .await?
                    }
                    // The only writer: nothing changes between the two.
                    Dialect::Sqlite => {
                        let n = sqlx::query_as(
                            "SELECT m.org_id, CAST(COUNT(*) AS BIGINT) FROM deliveries d \
                             JOIN messages m ON m.id = d.message_id \
                             WHERE d.org_id = $1 AND m.org_id <> $1 GROUP BY m.org_id",
                        )
                        .bind(org_id)
                        .fetch_all(&mut *c)
                        .await?;
                        sqlx::query("DELETE FROM deliveries WHERE org_id = $1")
                            .bind(org_id)
                            .execute(&mut *c)
                            .await?;
                        n
                    }
                };
                // Mail of orgs deleted earlier was charged to the orgs that
                // keep it, this one included; its share goes with it.
                for (sender, n) in gone {
                    *charge.entry(sender).or_default() -= n * per_delivery;
                }
                // After its deliveries are gone, the mail it sent that no
                // other org received has no deliveries left.
                let steps: [&str; 19] = [
                    "DELETE FROM web_sessions WHERE token_id IN (SELECT id FROM tokens WHERE org_id = $1)",
                    "DELETE FROM tokens WHERE org_id = $1",
                    "DELETE FROM claim_keys WHERE seat_id IN (SELECT id FROM seats WHERE org_id = $1)",
                    "DELETE FROM send_budget WHERE seat_id IN (SELECT id FROM seats WHERE org_id = $1)",
                    "DELETE FROM presence WHERE org_id = $1",
                    "DELETE FROM mirror_watch WHERE org_id = $1",
                    "DELETE FROM node_streams WHERE org_id = $1",
                    "UPDATE messages SET ack_due = NULL WHERE org_id = $1 AND ack_due IS NOT NULL",
                    "DELETE FROM thread_seats WHERE org_id = $1",
                    "DELETE FROM org_activity WHERE org_id = $1",
                    "UPDATE messages SET parent_id = NULL WHERE parent_id IN (SELECT id FROM messages \
                     WHERE org_id = $1 AND NOT EXISTS (SELECT 1 FROM deliveries d WHERE d.message_id = messages.id))",
                    "DELETE FROM messages WHERE org_id = $1 \
                     AND NOT EXISTS (SELECT 1 FROM deliveries d WHERE d.message_id = messages.id)",
                    "DELETE FROM list_members WHERE org_id = $1",
                    "DELETE FROM lists WHERE org_id = $1",
                    "DELETE FROM links WHERE org_id = $1 OR peer_org_id = $1",
                    "DELETE FROM link_requests WHERE org_id = $1",
                    "UPDATE seats SET parent_id = NULL WHERE org_id = $1",
                    "DELETE FROM seats WHERE org_id = $1 \
                     AND NOT EXISTS (SELECT 1 FROM messages m WHERE m.sender_seat_id = seats.id)",
                    "UPDATE seats SET display_name = NULL, role = NULL, harness = NULL, model = NULL, \
                     permission_mode = NULL, group_name = NULL, focus_until = NULL WHERE org_id = $1",
                ];
                for sql in steps {
                    sqlx::query(sql).bind(org_id).execute(&mut *c).await?;
                }
                sqlx::query("UPDATE seats SET deleted_at = $1 WHERE org_id = $2 AND deleted_at IS NULL")
                    .bind(now)
                    .bind(org_id)
                    .execute(&mut *c)
                    .await?;
                // Mail of orgs deleted earlier that only this org had kept.
                const GONE: [&str; 4] = [
                    "UPDATE messages SET parent_id = NULL WHERE parent_id IN (SELECT id FROM messages \
                     WHERE org_id IN (SELECT id FROM orgs WHERE deleted_at IS NOT NULL) \
                     AND NOT EXISTS (SELECT 1 FROM deliveries d WHERE d.message_id = messages.id))",
                    "DELETE FROM messages WHERE org_id IN (SELECT id FROM orgs WHERE deleted_at IS NOT NULL) \
                     AND NOT EXISTS (SELECT 1 FROM deliveries d WHERE d.message_id = messages.id)",
                    "DELETE FROM seats WHERE org_id IN (SELECT id FROM orgs WHERE deleted_at IS NOT NULL) \
                     AND NOT EXISTS (SELECT 1 FROM messages m WHERE m.sender_seat_id = seats.id)",
                    "DELETE FROM orgs WHERE deleted_at IS NOT NULL \
                     AND NOT EXISTS (SELECT 1 FROM seats s WHERE s.org_id = orgs.id)",
                ];
                for sql in GONE {
                    sqlx::query(sql).execute(&mut *c).await?;
                }
                // The mail it sent that other orgs keep is charged to them:
                // each its full bytes and its own deliveries of it. On
                // Postgres, a send of this org still in progress commits
                // first; later ones are refused (the org is marked deleted).
                if DIALECT == Dialect::Postgres {
                    sqlx::query("SELECT id FROM orgs WHERE id = $1 FOR UPDATE")
                        .bind(org_id)
                        .execute(&mut *c)
                        .await?;
                }
                let holders: Vec<(i64, i64)> = sqlx::query_as(concat!(
                    "SELECT h.org_id, CAST(SUM(", message_bytes_sql!(), " + $3 * h.n) AS BIGINT) \
                     FROM (SELECT d.org_id, d.message_id, COUNT(*) AS n FROM deliveries d \
                           WHERE d.message_id IN (SELECT id FROM messages WHERE org_id = $1) \
                           GROUP BY d.org_id, d.message_id) h \
                     JOIN messages m ON m.id = h.message_id GROUP BY h.org_id"
                ))
                .bind(org_id)
                .bind(amail_proto::limits::STORED_BYTES_PER_MESSAGE)
                .bind(per_delivery)
                .fetch_all(&mut *c)
                .await?;
                for (holder, n) in holders {
                    *charge.entry(holder).or_default() += n;
                }
                for (payer, n) in charge {
                    if payer != org_id && n != 0 {
                        sqlx::query("UPDATE orgs SET stored_bytes = stored_bytes + $1 WHERE id = $2 AND deleted_at IS NULL")
                            .bind(n)
                            .bind(payer)
                            .execute(&mut *c)
                            .await?;
                    }
                }
                let kept: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM messages WHERE org_id = $1)")
                    .bind(org_id)
                    .fetch_one(&mut *c)
                    .await?;
                if kept {
                    sqlx::query(
                        "UPDATE orgs SET deleted_at = $1, deleted_name = name, name = $2, \
                         webhook_url = NULL, stored_bytes = 0 WHERE id = $3",
                    )
                    .bind(now)
                    .bind(format!("deleted:{org_id}"))
                    .bind(org_id)
                    .execute(&mut *c)
                    .await?;
                } else {
                    sqlx::query("DELETE FROM orgs WHERE id = $1").bind(org_id).execute(&mut *c).await?;
                }
                Ok(true)
            }

            /// Clear the bodies of up to `limit` messages created before
            /// `cutoff` (oldest first); returns how many. The bytes of the
            /// cleared bodies are subtracted from their orgs' stored bytes.
            pub async fn expire_bodies(c: &mut Conn, cutoff: i64, limit: i64) -> Result<u64> {
                // Marking the messages first returns each body's size once,
                // also when another instance expires the same messages.
                let now = now_ms();
                let rows: Vec<(i64, i64, i64)> = sqlx::query_as(
                    "UPDATE messages SET expired_at = $1 WHERE id IN (SELECT id FROM messages \
                     WHERE created_at < $2 AND expired_at IS NULL ORDER BY created_at LIMIT $3) \
                     AND expired_at IS NULL RETURNING id, org_id, CAST(octet_length(body) AS BIGINT)",
                )
                .bind(now)
                .bind(cutoff)
                .bind(limit)
                .fetch_all(&mut *c)
                .await?;
                if rows.is_empty() {
                    return Ok(0);
                }
                for chunk in rows.chunks(500) {
                    let sql = format!(
                        "UPDATE messages SET body = '' WHERE id IN ({})",
                        placeholders(1, chunk.len())
                    );
                    let mut q = sqlx::query(&sql);
                    for (id, ..) in chunk {
                        q = q.bind(*id);
                    }
                    q.execute(&mut *c).await?;
                }
                // Each body's bytes go back to whoever is charged for it
                // (see `message_bytes`), in org id order.
                let mut by_sender: std::collections::BTreeMap<i64, Vec<(i64, i64)>> = Default::default();
                for (id, org, n) in &rows {
                    by_sender.entry(*org).or_default().push((*id, *n));
                }
                let mut freed: std::collections::BTreeMap<i64, i64> = Default::default();
                let mut live = Default::default();
                for (sender, msgs) in &by_sender {
                    for (id, n) in msgs {
                        for payer in payers(c, &mut live, *id, *sender, 0).await? {
                            *freed.entry(payer).or_default() += n;
                        }
                    }
                }
                for (org, n) in freed {
                    let done = sqlx::query(
                        "UPDATE orgs SET stored_bytes = stored_bytes - $1 WHERE id = $2 AND deleted_at IS NULL",
                    )
                    .bind(n)
                    .bind(org)
                    .execute(&mut *c)
                    .await?
                    .rows_affected();
                    // Postgres: the sender was deleted after `payers` looked;
                    // its mail is now charged to the orgs that keep it.
                    if done == 0 && let Some(msgs) = by_sender.get(&org) {
                        for (id, n) in msgs {
                            sqlx::query(
                                "UPDATE orgs SET stored_bytes = stored_bytes - $1 WHERE deleted_at IS NULL \
                                 AND id IN (SELECT org_id FROM deliveries WHERE message_id = $2)",
                            )
                            .bind(n)
                            .bind(id)
                            .execute(&mut *c)
                            .await?;
                        }
                    }
                }
                Ok(rows.len() as u64)
            }

            /// The orgs charged for message `id` sent by org `sender` (see
            /// `message_bytes`), without org `except`: the sender while it
            /// exists, else each org that keeps deliveries of the message.
            /// `live` caches whether each sender exists.
            async fn payers(
                c: &mut Conn,
                live: &mut std::collections::HashMap<i64, bool>,
                id: i64,
                sender: i64,
                except: i64,
            ) -> Result<Vec<i64>> {
                if sender == except {
                    return Ok(Vec::new());
                }
                let exists = match live.get(&sender) {
                    Some(b) => *b,
                    None => {
                        let deleted: Option<Option<i64>> =
                            sqlx::query_scalar("SELECT deleted_at FROM orgs WHERE id = $1")
                                .bind(sender)
                                .fetch_optional(&mut *c)
                                .await?;
                        let b = matches!(deleted, Some(None));
                        live.insert(sender, b);
                        b
                    }
                };
                if exists {
                    return Ok(vec![sender]);
                }
                Ok(sqlx::query_scalar(
                    "SELECT DISTINCT org_id FROM deliveries WHERE message_id = $1 AND org_id <> $2",
                )
                .bind(id)
                .bind(except)
                .fetch_all(&mut *c)
                .await?)
            }

            pub async fn operator_stats(c: &mut Conn, now: i64) -> Result<OperatorStats> {
                // A time as an id bound (with a margin, see `archive`), so the
                // counts read the (org_id, id) index from there.
                let first_id = |t: i64| {
                    sqlx::query_scalar::<_, i64>(
                        "SELECT id FROM messages WHERE created_at >= $1 ORDER BY created_at, id LIMIT 1",
                    )
                    .bind(t - ID_TIME_MARGIN_MS)
                };
                let hour = now - 3_600_000;
                let day = now - 86_400_000;
                let hour_lo = first_id(hour).fetch_optional(&mut *c).await?.unwrap_or(i64::MAX);
                let day_lo = first_id(day).fetch_optional(&mut *c).await?.unwrap_or(i64::MAX);
                #[allow(clippy::type_complexity)]
                let rows: Vec<(i64, String, i64, Option<i64>, i64, i64, i64, i64, Option<i64>, i64)> = sqlx::query_as(
                    "SELECT o.id, o.name, o.created_at, o.suspended_at, \
                     (SELECT COUNT(*) FROM seats s WHERE s.org_id = o.id AND s.kind = 'agent' AND s.deleted_at IS NULL), \
                     (SELECT COUNT(*) FROM messages m WHERE m.org_id = o.id), \
                     (SELECT COUNT(*) FROM messages m WHERE m.org_id = o.id AND m.id >= $1 AND m.created_at >= $2), \
                     (SELECT COUNT(*) FROM messages m WHERE m.org_id = o.id AND m.id >= $3 AND m.created_at >= $4), \
                     (SELECT m.created_at FROM messages m WHERE m.org_id = o.id ORDER BY m.id DESC LIMIT 1), \
                     o.stored_bytes \
                     FROM orgs o WHERE o.deleted_at IS NULL",
                )
                .bind(hour_lo)
                .bind(hour)
                .bind(day_lo)
                .bind(day)
                .fetch_all(&mut *c)
                .await?;
                let mut orgs: Vec<OrgStat> = rows
                    .into_iter()
                    .map(|(id, name, created_at, suspended_at, seats, messages, messages_hour, messages_day, last_activity, stored_bytes)| OrgStat {
                        id, name, created_at, suspended_at, seats, messages, messages_hour, messages_day, last_activity, stored_bytes,
                    })
                    .collect();
                orgs.sort_by(|a, b| {
                    (b.messages_hour, b.messages_day, b.seats)
                        .cmp(&(a.messages_hour, a.messages_day, a.seats))
                        .then_with(|| a.name.cmp(&b.name))
                });
                let messages_5min: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM messages WHERE created_at >= $1")
                    .bind(now - 300_000)
                    .fetch_one(&mut *c)
                    .await?;
                let node_streams = node_streams(c, "", now - 30_000).await?;
                Ok(OperatorStats { orgs, messages_5min, node_streams })
            }

            /// Issue `new` (a user token of the org) and revoke `old`, whose
            /// browser sessions end. Returns the new id.
            pub async fn rotate_token(c: &mut Conn, org_id: i64, old: i64, new: &NewToken) -> Result<i64> {
                let id: i64 = sqlx::query_scalar(
                    "INSERT INTO tokens (org_id, scope, seat_id, hash, label, created_at) \
                     VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
                )
                .bind(new.org_id)
                .bind(new.scope.as_str())
                .bind(new.seat_id)
                .bind(&new.hash)
                .bind(&new.label)
                .bind(now_ms())
                .fetch_one(&mut *c)
                .await?;
                sqlx::query("DELETE FROM web_sessions WHERE token_id = $1")
                    .bind(old)
                    .execute(&mut *c)
                    .await?;
                if !revoke_token(c, org_id, old).await? {
                    return Err(StoreError::NotFound(format!("token {old}")));
                }
                Ok(id)
            }

            /// Revoke every user token of the org (which ends their browser
            /// sessions) and issue a new one.
            pub async fn reset_user_tokens(c: &mut Conn, org_id: i64, hash: &str) -> Result<()> {
                let user = user_seat(c, org_id)
                    .await?
                    .ok_or_else(|| StoreError::NotFound("user seat".into()))?;
                sqlx::query("UPDATE tokens SET revoked_at = $1 WHERE org_id = $2 AND scope = 'user' AND revoked_at IS NULL")
                    .bind(now_ms())
                    .bind(org_id)
                    .execute(&mut *c)
                    .await?;
                insert_token(c, &NewToken {
                    org_id,
                    scope: TokenScope::User,
                    seat_id: Some(user.id),
                    hash: hash.to_string(),
                    label: Some("reset by the operator".into()),
                })
                .await
            }

            /// Entries (with bodies) whose mod_seq is in `(lo, hi]`, by seq.
            pub async fn entries_in_range(c: &mut Conn, seat_id: i64, lo: i64, hi: i64) -> Result<Vec<Entry>> {
                let rows: Vec<EntryRow> = sqlx::query_as(concat!(
                    entry_select_body!(),
                    " WHERE d.seat_id = $1 AND d.mod_seq > $2 AND d.mod_seq <= $3 ORDER BY d.seq"
                ))
                .bind(seat_id)
                .bind(lo)
                .bind(hi)
                .fetch_all(&mut *c)
                .await?;
                collect(rows, EntryRow::into_entry)
            }
        }

        #[async_trait::async_trait]
        impl $crate::Store for $store {
            fn backend(&self) -> &'static str {
                Self::BACKEND
            }

            async fn create_org(&self, name: &str, user_seat: &str, token_hash: &str) -> $crate::Result<($crate::Org, $crate::Seat)> {
                let (name, user_seat, hash) = (name.to_string(), user_seat.to_string(), token_hash.to_string());
                run!(self.write, |c| q::create_org(c, &name, &user_seat, &hash).await)
            }

            async fn org_by_name(&self, name: &str) -> $crate::Result<Option<$crate::Org>> {
                let name = name.to_string();
                run!(self.read, |c| q::org_by_name(c, &name).await)
            }

            async fn update_org(&self, org_id: i64, patch: &$crate::OrgSettings) -> $crate::Result<$crate::Org> {
                let patch = patch.clone();
                run!(self.write, |c| q::update_org(c, org_id, &patch).await)
            }

            async fn create_token(&self, token: &$crate::NewToken) -> $crate::Result<()> {
                let token = token.clone();
                run!(self.write, |c| q::insert_token(c, &token).await)
            }

            async fn token_auth(&self, hash: &str) -> $crate::Result<Option<$crate::TokenAuth>> {
                let hash = hash.to_string();
                run!(self.read, |c| q::token_auth(c, &hash).await)
            }

            async fn create_seat(&self, org_id: i64, seat: &$crate::NewSeat) -> $crate::Result<$crate::Seat> {
                let seat = seat.clone();
                run!(self.write, |c| q::create_seat(c, org_id, &seat).await)
            }

            async fn seat_by_name(&self, org_id: i64, name: &str) -> $crate::Result<Option<$crate::Seat>> {
                let name = name.to_string();
                run!(self.read, |c| q::seat_by_name(c, org_id, &name).await)
            }

            async fn seats_by_names(&self, org_id: i64, names: &[String]) -> $crate::Result<Vec<$crate::Seat>> {
                let names = names.to_vec();
                run!(self.read, |c| q::seats_by_names(c, org_id, &names).await)
            }

            async fn seats(&self, org_id: i64) -> $crate::Result<Vec<$crate::Seat>> {
                run!(self.read, |c| q::seats(c, org_id).await)
            }

            async fn update_seat(&self, org_id: i64, name: &str, patch: &$crate::SeatUpdate) -> $crate::Result<$crate::Seat> {
                let (name, patch) = (name.to_string(), patch.clone());
                run!(self.write, |c| q::update_seat(c, org_id, &name, &patch).await)
            }

            async fn delete_seat(&self, org_id: i64, name: &str) -> $crate::Result<()> {
                let name = name.to_string();
                run!(self.write, |c| q::delete_seat(c, org_id, &name).await)
            }

            async fn create_list(&self, org_id: i64, list: &$crate::NewList) -> $crate::Result<$crate::List> {
                let list = list.clone();
                run!(self.write, |c| q::create_list(c, org_id, &list).await)
            }

            async fn list_by_name(&self, org_id: i64, name: &str) -> $crate::Result<Option<$crate::List>> {
                let name = name.to_string();
                run!(self.read, |c| q::list_by_name(c, org_id, &name).await)
            }

            async fn lists(&self, org_id: i64) -> $crate::Result<Vec<$crate::List>> {
                run!(self.read_query, |c| q::lists(c, org_id).await)
            }

            async fn delete_list(&self, org_id: i64, name: &str) -> $crate::Result<()> {
                let name = name.to_string();
                run!(self.write, |c| q::delete_list(c, org_id, &name).await)
            }

            async fn list_members(&self, list_id: i64) -> $crate::Result<Vec<$crate::Member>> {
                run!(self.read, |c| q::list_members(c, list_id).await)
            }

            async fn put_member(&self, org_id: i64, list_id: i64, member: &$crate::Member) -> $crate::Result<()> {
                let member = member.clone();
                run!(self.write, |c| q::put_member(c, org_id, list_id, &member).await)
            }

            async fn remove_member(&self, list_id: i64, address: &str) -> $crate::Result<bool> {
                let address = address.to_string();
                run!(self.write, |c| q::remove_member(c, list_id, &address).await)
            }

            async fn request_link(&self, org_id: i64, peer_id: i64) -> $crate::Result<amail_proto::LinkState> {
                run!(self.write, |c| q::request_link(c, org_id, peer_id).await)
            }

            async fn accept_link(&self, org_id: i64, peer_id: i64) -> $crate::Result<()> {
                run!(self.write, |c| q::accept_link(c, org_id, peer_id).await)
            }

            async fn revoke_link(&self, org_id: i64, peer_id: i64) -> $crate::Result<bool> {
                run!(self.write, |c| q::revoke_link(c, org_id, peer_id).await)
            }

            async fn links(&self, org_id: i64) -> $crate::Result<Vec<$crate::Link>> {
                run!(self.read, |c| q::links(c, org_id).await)
            }

            async fn request_unknown_link(&self, org_id: i64, name: &str) -> $crate::Result<()> {
                let name = name.to_string();
                run!(self.write, |c| q::request_unknown_link(c, org_id, &name).await)
            }

            async fn revoke_unknown_link(&self, org_id: i64, name: &str) -> $crate::Result<bool> {
                let name = name.to_string();
                run!(self.write, |c| q::revoke_unknown_link(c, org_id, &name).await)
            }

            async fn link_active(&self, org_id: i64, peer_id: i64) -> $crate::Result<bool> {
                let s = run!(self.read, |c| q::link_state(c, org_id, peer_id).await)?;
                Ok(s == Some(amail_proto::LinkState::Active))
            }

            async fn send(&self, msg: &$crate::NewMessage) -> $crate::Result<$crate::Sent> {
                if msg.deliveries.is_empty() {
                    return Err(StoreError::Invalid("a message needs at least one delivery".into()));
                }
                self.send_impl(msg).await
            }

            async fn message(&self, id: i64) -> $crate::Result<Option<$crate::MessageRec>> {
                run!(self.read, |c| q::message(c, id).await)
            }

            async fn org_sees_message(&self, org_id: i64, message_id: i64) -> $crate::Result<bool> {
                run!(self.read, |c| q::org_sees_message(c, org_id, message_id).await)
            }

            async fn seat_in_thread(&self, seat_id: i64, thread_id: i64) -> $crate::Result<bool> {
                run!(self.read, |c| q::seat_in_thread(c, seat_id, thread_id).await)
            }

            async fn org_sees_thread(&self, org_id: i64, thread_id: i64) -> $crate::Result<bool> {
                run!(self.read, |c| q::org_sees_thread(c, org_id, thread_id).await)
            }

            async fn delivery(&self, message_id: i64, seat_id: i64) -> $crate::Result<Option<$crate::DeliveryRec>> {
                run!(self.read, |c| q::delivery(c, message_id, seat_id).await)
            }

            async fn thread(&self, thread_id: i64, org_id: i64, limit: i64, body_bytes: usize) -> $crate::Result<Vec<$crate::ThreadMessage>> {
                run!(self.read_query, |c| q::thread(c, thread_id, org_id, limit, body_bytes).await)
            }

            async fn inbox(&self, seat_id: i64, query: &$crate::InboxQuery) -> $crate::Result<Vec<$crate::Entry>> {
                let query = query.clone();
                run!(self.read, |c| q::inbox(c, seat_id, &query).await)
            }

            async fn mark_read(&self, message_id: i64, seat_id: i64) -> $crate::Result<Option<($crate::DeliveryRec, Option<$crate::SeqBump>)>> {
                // Avoid a write when there is nothing to change.
                match run!(self.read, |c| q::delivery(c, message_id, seat_id).await)? {
                    None => return Ok(None),
                    Some(d) if d.read_at.is_some() => return Ok(Some((d, None))),
                    Some(_) => {}
                }
                run!(self.write, |c| q::mark_read(c, message_id, seat_id).await)
            }

            async fn ack(&self, message_id: i64, seat_id: i64) -> $crate::Result<Option<($crate::DeliveryRec, Option<$crate::SeqBump>)>> {
                run!(self.write, |c| q::ack(c, message_id, seat_id).await)
            }

            async fn claim(&self, seat_id: i64, f: &$crate::ClaimFilter) -> $crate::Result<(Vec<$crate::Entry>, Option<$crate::SeqBump>)> {
                if f.classes.is_empty() || f.limit <= 0 {
                    return Ok((Vec::new(), None));
                }
                // A retry of a keyed claim that went through.
                if let Some(key) = f.key.clone() {
                    let done = run!(self.read, |c| match q::claimed_with_key(c, seat_id, &key).await? {
                        Some(ids) => Ok(Some(q::entries_by_ids(c, seat_id, &ids).await?)),
                        None => Ok(None),
                    })?;
                    if let Some(entries) = done {
                        return Ok((entries, None));
                    }
                }
                // Cheap unlocked check first: most periodic claims find nothing.
                let f2 = f.clone();
                let any = run!(self.read, |c| q::claim_candidates(c, claim_select!(), seat_id, &f2, None).await)?;
                if any.is_empty() {
                    return Ok((Vec::new(), None));
                }
                self.claim_impl(seat_id, f).await
            }

            async fn changes(&self, seat_id: i64, since: i64, limit: i64) -> $crate::Result<Vec<$crate::Entry>> {
                run!(self.read, |c| q::changes(c, seat_id, since, limit).await)
            }

            async fn recipients(&self, message_id: i64) -> $crate::Result<Vec<$crate::DeliveryRec>> {
                run!(self.read_query, |c| q::recipients(c, message_id).await)
            }

            async fn archive(&self, org_id: i64, query: &$crate::ArchiveQuery) -> $crate::Result<$crate::ArchivePage> {
                let query = query.clone();
                run!(self.read_query, |c| q::archive(c, org_id, &query).await)
            }

            async fn seat_seqs(&self, org_id: i64) -> $crate::Result<Vec<(i64, String, i64)>> {
                run!(self.read, |c| q::seat_seqs(c, org_id).await)
            }

            async fn tokens(&self, org_id: i64) -> $crate::Result<Vec<$crate::TokenRec>> {
                run!(self.read, |c| q::tokens(c, org_id).await)
            }

            async fn revoke_token(&self, org_id: i64, id: i64) -> $crate::Result<bool> {
                run!(self.write, |c| q::revoke_token(c, org_id, id).await)
            }

            async fn pending(&self, org_id: i64, seat_id: Option<i64>) -> $crate::Result<Vec<$crate::PendingRec>> {
                run!(self.read, |c| q::pending(c, org_id, seat_id).await)
            }

            async fn session_auth(&self, hash: &str) -> $crate::Result<Option<$crate::TokenAuth>> {
                let hash = hash.to_string();
                let now = now_ms();
                run!(self.read, |c| q::session_auth(c, &hash, now).await)
            }

            async fn create_session(&self, hash: &str, token_id: i64, expires_at: i64) -> $crate::Result<()> {
                let hash = hash.to_string();
                run!(self.write, |c| q::create_session(c, &hash, token_id, expires_at).await)
            }

            async fn delete_session(&self, hash: &str) -> $crate::Result<bool> {
                let hash = hash.to_string();
                run!(self.write, |c| q::delete_session(c, &hash).await)
            }

            async fn user_seat(&self, org_id: i64) -> $crate::Result<Option<$crate::Seat>> {
                run!(self.read, |c| q::user_seat(c, org_id).await)
            }

            async fn counts(&self, org_id: i64, seat_id: Option<i64>) -> $crate::Result<Vec<$crate::CountRec>> {
                run!(self.read, |c| q::counts(c, org_id, seat_id).await)
            }

            async fn attention(&self, user_seat_id: i64, limit: i64) -> $crate::Result<Vec<$crate::Entry>> {
                run!(self.read_query, |c| q::attention(c, user_seat_id, limit).await)
            }

            async fn activity(&self, org_id: i64, since_minute: i64) -> $crate::Result<i64> {
                run!(self.read, |c| q::activity(c, org_id, since_minute).await)
            }

            async fn add_activity(&self, counts: &[(i64, i64, i64)]) -> $crate::Result<()> {
                let counts = counts.to_vec();
                run!(self.write, |c| q::add_activity(c, &counts).await)
            }

            async fn search(&self, org_id: i64, query: &$crate::SearchQuery) -> $crate::Result<$crate::SearchResult> {
                if query.terms.is_empty() || query.limit < 1 {
                    return Ok($crate::SearchResult::default());
                }
                let query = query.clone();
                run!(self.read_query, |c| q::search(c, org_id, &query).await)
            }

            async fn presence(&self, org_id: i64, seat: Option<&str>) -> $crate::Result<Vec<$crate::PresenceRec>> {
                let seat = seat.map(str::to_string);
                run!(self.read, |c| q::presence(c, org_id, seat.as_deref()).await)
            }

            async fn put_presence(&self, org_id: i64, recs: &[$crate::PresenceRec], ttl_ms: i64) -> $crate::Result<Vec<$crate::PresenceRec>> {
                let recs = recs.to_vec();
                run!(self.write, |c| q::put_presence(c, org_id, &recs, ttl_ms).await)
            }

            async fn take_send_token(&self, seat_id: i64, per_min: i64, burst: i64) -> $crate::Result<std::result::Result<(), u64>> {
                self.take_send_token_impl(seat_id, per_min, burst).await
            }

            async fn refund_send_token(&self, seat_id: i64, burst: i64) -> $crate::Result<()> {
                self.refund_send_token_impl(seat_id, burst).await
            }

            async fn heartbeat_instance(&self, id: &str, url: Option<&str>) -> $crate::Result<()> {
                let (id, url) = (id.to_string(), url.map(str::to_string));
                let now = now_ms();
                run!(self.write, |c| q::heartbeat_instance(c, &id, url.as_deref(), now).await)
            }

            async fn remove_instance(&self, id: &str) -> $crate::Result<()> {
                let id = id.to_string();
                run!(self.write, |c| q::remove_instance(c, &id).await)
            }

            async fn set_node_streams(&self, instance: &str, counts: &[(i64, i64)]) -> $crate::Result<()> {
                let (instance, counts) = (instance.to_string(), counts.to_vec());
                run!(self.write, |c| q::set_node_streams(c, &instance, &counts).await)
            }

            async fn node_streams(&self, except: &str, fresh_since: i64) -> $crate::Result<Vec<(i64, i64)>> {
                let except = except.to_string();
                run!(self.read, |c| q::node_streams(c, &except, fresh_since).await)
            }

            async fn watch_mirror(&self, org_id: i64, seat: &str, instance: &str, expires_at: Option<i64>) -> $crate::Result<()> {
                let (seat, instance) = (seat.to_string(), instance.to_string());
                run!(self.write, |c| q::watch_mirror(c, org_id, &seat, &instance, expires_at).await)
            }

            async fn mirror_watchers(&self, org_id: i64, seat: &str) -> $crate::Result<Vec<(String, Option<String>)>> {
                let seat = seat.to_string();
                let now = now_ms();
                run!(self.read, |c| q::mirror_watchers(c, org_id, &seat, now).await)
            }

            async fn live_credentials(&self, tokens: &[i64], sessions: &[String]) -> $crate::Result<(Vec<i64>, Vec<String>)> {
                let (tokens, sessions) = (tokens.to_vec(), sessions.to_vec());
                let now = now_ms();
                run!(self.read, |c| q::live_credentials(c, &tokens, &sessions, now).await)
            }

            async fn row_counts(&self) -> $crate::Result<$crate::RowCounts> {
                run!(self.read, |c| q::row_counts(c).await)
            }

            async fn cleanup(&self) -> $crate::Result<()> {
                let now = now_ms();
                run!(self.write, |c| q::cleanup(c, now).await)
            }

            async fn set_focus(&self, seat_id: i64, until: Option<i64>) -> $crate::Result<$crate::SeqBump> {
                run!(self.write, |c| q::set_focus(c, seat_id, until).await)
            }

            async fn system_seat(&self, org_id: i64) -> $crate::Result<$crate::Seat> {
                if let Some(s) = run!(self.read, |c| q::system_seat_get(c, org_id).await)? {
                    return Ok(s);
                }
                run!(self.write, |c| q::system_seat(c, org_id).await)
            }

            async fn recipient_states(&self, seat_ids: &[i64]) -> $crate::Result<Vec<$crate::RecipientState>> {
                let ids = seat_ids.to_vec();
                run!(self.read, |c| q::recipient_states(c, &ids).await)
            }

            async fn due_acks(&self, now: i64, limit: i64) -> $crate::Result<Vec<$crate::AckDue>> {
                run!(self.read, |c| q::due_acks(c, now, limit).await)
            }

            async fn advance_ack(&self, message_id: i64, from: i64, to: i64, next_due: Option<i64>) -> $crate::Result<bool> {
                run!(self.write, |c| q::advance_ack(c, message_id, from, to, next_due).await)
            }

            async fn laggards(&self, message_id: i64) -> $crate::Result<Vec<$crate::Laggard>> {
                run!(self.read, |c| q::laggards(c, message_id).await)
            }

            async fn create_invite(&self, hash: &str, note: Option<&str>, expires_at: Option<i64>) -> $crate::Result<i64> {
                let (hash, note) = (hash.to_string(), note.map(str::to_string));
                run!(self.write, |c| q::create_invite(c, &hash, note.as_deref(), expires_at).await)
            }

            async fn invites(&self) -> $crate::Result<Vec<$crate::InviteRec>> {
                run!(self.read, |c| q::invites(c).await)
            }

            async fn revoke_invite(&self, id: i64) -> $crate::Result<bool> {
                run!(self.write, |c| q::revoke_invite(c, id).await)
            }

            async fn register(
                &self,
                invite_hash: &str,
                org: &str,
                user_seat: &str,
                token_hash: &str,
            ) -> $crate::Result<std::result::Result<($crate::Org, $crate::Seat), $crate::InviteError>> {
                let (i, o, u, t) = (invite_hash.to_string(), org.to_string(), user_seat.to_string(), token_hash.to_string());
                run!(self.write, |c| q::register(c, &i, &o, &u, &t).await)
            }

            async fn create_operator_token(&self, hash: &str, label: Option<&str>) -> $crate::Result<i64> {
                let (hash, label) = (hash.to_string(), label.map(str::to_string));
                run!(self.write, |c| q::create_operator_token(c, &hash, label.as_deref()).await)
            }

            async fn operator_tokens(&self) -> $crate::Result<Vec<$crate::OperatorTokenRec>> {
                run!(self.read, |c| q::operator_tokens(c).await)
            }

            async fn revoke_operator_token(&self, id: i64) -> $crate::Result<bool> {
                run!(self.write, |c| q::revoke_operator_token(c, id).await)
            }

            async fn operator_auth(&self, hash: &str) -> $crate::Result<Option<i64>> {
                let hash = hash.to_string();
                run!(self.read, |c| q::operator_auth(c, &hash).await)
            }

            async fn operator_session_auth(&self, hash: &str) -> $crate::Result<Option<i64>> {
                let hash = hash.to_string();
                let now = now_ms();
                run!(self.read, |c| q::operator_session_auth(c, &hash, now).await)
            }

            async fn create_operator_session(&self, hash: &str, token_id: i64, expires_at: i64) -> $crate::Result<()> {
                let hash = hash.to_string();
                run!(self.write, |c| q::create_operator_session(c, &hash, token_id, expires_at).await)
            }

            async fn delete_operator_session(&self, hash: &str) -> $crate::Result<bool> {
                let hash = hash.to_string();
                run!(self.write, |c| q::delete_operator_session(c, &hash).await)
            }

            async fn orgs(&self) -> $crate::Result<Vec<$crate::Org>> {
                run!(self.read, |c| q::orgs(c).await)
            }

            async fn set_org_suspended(&self, org_id: i64, at: Option<i64>) -> $crate::Result<bool> {
                run!(self.write, |c| q::set_org_suspended(c, org_id, at).await)
            }

            async fn delete_org(&self, org_id: i64) -> $crate::Result<bool> {
                run!(self.write, |c| q::delete_org(c, org_id).await)
            }

            async fn expire_bodies(&self, cutoff: i64, limit: i64) -> $crate::Result<u64> {
                run!(self.write, |c| q::expire_bodies(c, cutoff, limit).await)
            }

            async fn operator_stats(&self) -> $crate::Result<$crate::OperatorStats> {
                let now = now_ms();
                run!(self.read, |c| q::operator_stats(c, now).await)
            }

            async fn rotate_token(&self, org_id: i64, old: i64, new: &$crate::NewToken) -> $crate::Result<i64> {
                let new = new.clone();
                run!(self.write, |c| q::rotate_token(c, org_id, old, &new).await)
            }

            async fn reset_user_tokens(&self, org_id: i64, hash: &str) -> $crate::Result<()> {
                let hash = hash.to_string();
                run!(self.write, |c| q::reset_user_tokens(c, org_id, &hash).await)
            }
        }
    };
}
