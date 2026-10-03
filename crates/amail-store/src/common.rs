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

macro_rules! msg_cols {
    () => {
        "m.id, m.org_id, mo.name AS org_name, m.sender_seat_id, ms.name AS sender_name, \
         ms.kind AS sender_kind, m.thread_id, m.parent_id, m.depth, m.subject, m.lines, \
         m.to_addrs, m.cc_addrs, m.priority, m.ack_required, m.meta, m.created_at"
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

/// `SELECT` of delivery + message with body.
macro_rules! entry_select_body {
    () => {
        concat!(
            "SELECT ",
            deliv_cols!(),
            ", ",
            msg_cols!(),
            ", m.body FROM deliveries d",
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
/// the key first).
macro_rules! claim_select {
    () => {
        "SELECT d.id FROM deliveries d JOIN messages m ON m.id = d.message_id \
         WHERE d.seat_id = $1 AND d.notified_at IS NULL \
         AND ((($2 & 1) <> 0 AND d.mode = 'immediate') OR (($2 & 2) <> 0 AND d.mode = 'digest')) \
         AND (($3 AND d.class = 'to') OR ($4 AND d.class = 'cc') OR ($5 AND d.class = 'list')) \
         AND (NOT $6 OR m.priority = 'urgent') \
         AND NOT EXISTS (SELECT 1 FROM claim_keys k WHERE k.seat_id = $1 AND k.key = $8) \
         ORDER BY d.seq LIMIT $7"
    };
}

/// Columns and joins of [`AuthRow`]: a token (`t`) with its org and seat.
macro_rules! auth_select {
    () => {
        "SELECT t.id AS token_id, t.scope, t.seat_id, o.id, o.name, o.send_rate_per_min, o.send_burst, \
         o.max_recipients, o.max_thread_depth, o.webhook_url, o.ack_timeout_min, o.focus_max_min, \
         o.digest_interval_min, o.created_at, s.id AS s_id, \
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

/// Visibility of message `m` to org `$1`: sent by it or delivered to it.
macro_rules! visible_to_org {
    () => {
        "(m.org_id = $1 OR EXISTS (SELECT 1 FROM deliveries vd WHERE vd.message_id = m.id AND vd.org_id = $1))"
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

fn split_addrs(s: &str) -> Vec<String> {
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

/// The query part shared by both backends' search: visibility, the page
/// (`before_id`) and, in scan mode, the window's lower bound. Binds `$1` org,
/// `$2` before_id, `$3` lower bound, `$4` limit; terms start at `$5`.
pub(crate) fn search_sql(term_conds: &[String]) -> String {
    let mut sql = String::from(concat!(
        "SELECT ",
        msg_cols!(),
        ", m.body FROM messages m",
        msg_joins!(),
        " WHERE ",
        visible_to_org!(),
        " AND ($2 IS NULL OR m.id < $2) AND ($3 IS NULL OR m.id >= $3)"
    ));
    for c in term_conds {
        sql.push_str(" AND ");
        sql.push_str(c);
    }
    sql.push_str(" ORDER BY m.id DESC LIMIT $4");
    sql
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
        })
    }
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
    ($store:ty, $db:ty) => {
        #[allow(dead_code)]
        pub(crate) mod q {
            use $crate::common::*;
            use $crate::model::*;
            use $crate::{now_ms, Result, StoreError};
            use amail_proto::{LinkState, SeatKind, TokenScope};

            pub(crate) type Conn = <$db as sqlx::Database>::Connection;

            pub async fn org_by_id(c: &mut Conn, id: i64) -> Result<Option<Org>> {
                let r: Option<OrgRow> = sqlx::query_as("SELECT * FROM orgs WHERE id = $1")
                    .bind(id)
                    .fetch_optional(&mut *c)
                    .await?;
                Ok(r.map(Org::from))
            }

            pub async fn org_by_name(c: &mut Conn, name: &str) -> Result<Option<Org>> {
                let r: Option<OrgRow> = sqlx::query_as("SELECT * FROM orgs WHERE name = $1")
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

            pub async fn create_session(c: &mut Conn, hash: &str, token_id: i64, expires_at: i64) -> Result<()> {
                let now = now_ms();
                sqlx::query("DELETE FROM web_sessions WHERE expires_at <= $1")
                    .bind(now)
                    .execute(&mut *c)
                    .await?;
                sqlx::query(
                    "INSERT INTO web_sessions (hash, token_id, created_at, expires_at) VALUES ($1, $2, $3, $4)",
                )
                .bind(hash)
                .bind(token_id)
                .bind(now)
                .bind(expires_at)
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

            pub async fn counts(c: &mut Conn, org_id: i64, seat_id: Option<i64>) -> Result<Vec<CountRec>> {
                let rows: Vec<(i64, String, i64, i64, i64)> = sqlx::query_as(
                    "SELECT s.id, s.name, s.last_seq, \
                     (SELECT COUNT(*) FROM deliveries d WHERE d.seat_id = s.id AND d.read_at IS NULL \
                       AND d.mode <> 'archive') AS unread, \
                     (SELECT COUNT(*) FROM deliveries d WHERE d.seat_id = s.id AND d.notified_at IS NULL \
                       AND d.mode = 'immediate') AS pending \
                     FROM seats s WHERE s.org_id = $1 AND s.deleted_at IS NULL AND ($2 IS NULL OR s.id = $2) \
                     ORDER BY s.name",
                )
                .bind(org_id)
                .bind(seat_id)
                .fetch_all(&mut *c)
                .await?;
                Ok(rows
                    .into_iter()
                    .map(|(seat_id, seat_name, last_seq, unread, pending)| CountRec { seat_id, seat_name, last_seq, unread, pending })
                    .collect())
            }

            /// Deliveries to the user seat that wait for the user: ack
            /// requested and not given, or unread `to` mail (directly or
            /// through a list in To, immediate mode) the user has not
            /// answered (sent nothing later in the thread).
            pub async fn attention(c: &mut Conn, user_seat_id: i64, limit: i64) -> Result<Vec<Entry>> {
                let rows: Vec<EntryRow> = sqlx::query_as(concat!(
                    entry_select!(),
                    " WHERE d.seat_id = $1 AND d.mode <> 'archive' AND m.sender_seat_id <> $1 AND ( \
                       (m.ack_required AND d.acked_at IS NULL) \
                       OR (d.class = 'to' AND d.mode = 'immediate' AND d.read_at IS NULL AND NOT EXISTS ( \
                           SELECT 1 FROM messages r WHERE r.thread_id = m.thread_id \
                           AND r.sender_seat_id = $1 AND r.id > m.id))) \
                     ORDER BY d.seq LIMIT $2"
                ))
                .bind(user_seat_id)
                .bind(limit)
                .fetch_all(&mut *c)
                .await?;
                collect(rows, EntryRow::into_entry)
            }

            pub async fn message_count(c: &mut Conn, org_id: i64, since: i64) -> Result<i64> {
                Ok(sqlx::query_scalar(concat!(
                    "SELECT COUNT(*) FROM messages m WHERE m.created_at >= $2 AND ",
                    visible_to_org!()
                ))
                .bind(org_id)
                .bind(since)
                .fetch_one(&mut *c)
                .await?)
            }

            /// The `n` most recent messages visible to the org before
            /// `before_id`: `(count, oldest id)`.
            pub async fn scan_window(c: &mut Conn, org_id: i64, before_id: Option<i64>, n: i64) -> Result<(i64, Option<i64>)> {
                Ok(sqlx::query_as(concat!(
                    "SELECT COUNT(*), MIN(id) FROM (SELECT m.id FROM messages m WHERE ",
                    visible_to_org!(),
                    " AND ($2 IS NULL OR m.id < $2) ORDER BY m.id DESC LIMIT $3) w"
                ))
                .bind(org_id)
                .bind(before_id)
                .bind(n)
                .fetch_one(&mut *c)
                .await?)
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

            pub async fn request_link(c: &mut Conn, org_id: i64, peer_id: i64) -> Result<LinkState> {
                if org_id == peer_id {
                    return Err(StoreError::Invalid("an org cannot link to itself".into()));
                }
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
                     JOIN orgs o ON o.id = l.peer_org_id WHERE l.org_id = $1 ORDER BY o.name",
                )
                .bind(org_id)
                .fetch_all(&mut *c)
                .await?;
                collect(rows, LinkRow::into_link)
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

            pub async fn seat_in_thread(c: &mut Conn, seat_id: i64, thread_id: i64) -> Result<bool> {
                let r: Option<i64> = sqlx::query_scalar(
                    "SELECT CAST(1 AS BIGINT) FROM messages WHERE thread_id = $1 AND sender_seat_id = $2 \
                     UNION ALL SELECT CAST(1 AS BIGINT) FROM deliveries d JOIN messages m ON m.id = d.message_id \
                     WHERE m.thread_id = $1 AND d.seat_id = $2 LIMIT 1",
                )
                .bind(thread_id)
                .bind(seat_id)
                .fetch_optional(&mut *c)
                .await?;
                Ok(r.is_some())
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

            pub async fn thread(c: &mut Conn, thread_id: i64, org_id: i64) -> Result<Vec<MessageRec>> {
                let rows: Vec<MsgRow> = sqlx::query_as(concat!(
                    "SELECT ", msg_cols!(), ", m.body FROM messages m", msg_joins!(),
                    " WHERE m.thread_id = $1 AND (m.org_id = $2 OR EXISTS \
                     (SELECT 1 FROM deliveries d WHERE d.message_id = m.id AND d.org_id = $2)) ORDER BY m.id"
                ))
                .bind(thread_id)
                .bind(org_id)
                .fetch_all(&mut *c)
                .await?;
                collect(rows, MsgRow::into_message)
            }

            pub async fn inbox(c: &mut Conn, seat_id: i64, q: &InboxQuery) -> Result<Vec<Entry>> {
                let rows: Vec<EntryRow> = if let Some(before) = q.before_seq {
                    let mut rows: Vec<EntryRow> = sqlx::query_as(concat!(
                        entry_select!(),
                        " WHERE d.seat_id = $1 AND d.seq < $2 AND (NOT $3 OR d.read_at IS NULL) \
                         ORDER BY d.seq DESC LIMIT $4"
                    ))
                    .bind(seat_id)
                    .bind(before)
                    .bind(q.unread_only)
                    .bind(q.limit)
                    .fetch_all(&mut *c)
                    .await?;
                    rows.reverse();
                    rows
                } else {
                    sqlx::query_as(concat!(
                        entry_select!(),
                        " WHERE d.seat_id = $1 AND d.seq > $2 AND (NOT $3 OR d.read_at IS NULL) \
                         ORDER BY d.seq LIMIT $4"
                    ))
                    .bind(seat_id)
                    .bind(q.after_seq.unwrap_or(0))
                    .bind(q.unread_only)
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

            pub async fn archive(c: &mut Conn, org_id: i64, q: &ArchiveQuery) -> Result<Vec<MessageRec>> {
                let (from_org, from_name) = match &q.from {
                    Some((o, n)) => (Some(o.clone()), Some(n.clone())),
                    None => (None, None),
                };
                let rows: Vec<MsgRow> = sqlx::query_as(concat!(
                    "SELECT ", msg_cols!(), ", '' AS body FROM messages m", msg_joins!(),
                    " WHERE (m.org_id = $1 OR EXISTS \
                       (SELECT 1 FROM deliveries d WHERE d.message_id = m.id AND d.org_id = $1)) \
                     AND ($2 IS NULL OR m.created_at >= $2) \
                     AND ($3 IS NULL OR m.created_at < $3) \
                     AND ($4 IS NULL OR (mo.name = $4 AND ms.name = $5)) \
                     AND ($6 IS NULL OR m.thread_id = $6) \
                     AND ($7 IS NULL OR m.id < $7) \
                     AND ($9 IS NULL OR EXISTS (SELECT 1 FROM deliveries td \
                       JOIN seats ts ON ts.id = td.seat_id JOIN orgs tor ON tor.id = ts.org_id \
                       WHERE td.message_id = m.id AND tor.name = $9 AND ts.name = $10)) \
                     ORDER BY m.id DESC LIMIT $8"
                ))
                .bind(org_id)
                .bind(q.since)
                .bind(q.until)
                .bind(from_org)
                .bind(from_name)
                .bind(q.thread_id)
                .bind(q.before_id)
                .bind(q.limit)
                .bind(q.to.as_ref().map(|t| t.0.clone()))
                .bind(q.to.as_ref().map(|t| t.1.clone()))
                .fetch_all(&mut *c)
                .await?;
                collect(rows, MsgRow::into_message)
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

            pub async fn pending(c: &mut Conn, org_id: i64, seat_id: Option<i64>) -> Result<Vec<PendingRec>> {
                let rows: Vec<PendingRow> = sqlx::query_as(
                    "SELECT s.id AS seat_id, s.name AS seat_name, s.last_seq, s.focus_until, \
                     COALESCE(SUM(CASE WHEN d.mode = 'immediate' AND d.class = 'to' THEN 1 ELSE 0 END), 0) AS to_n, \
                     COALESCE(SUM(CASE WHEN d.mode = 'immediate' AND d.class = 'cc' THEN 1 ELSE 0 END), 0) AS cc_n, \
                     COALESCE(SUM(CASE WHEN d.mode = 'immediate' AND d.class = 'list' THEN 1 ELSE 0 END), 0) AS list_n, \
                     COALESCE(SUM(CASE WHEN d.mode = 'immediate' AND m.priority = 'urgent' THEN 1 ELSE 0 END), 0) AS urgent_n, \
                     COALESCE(SUM(CASE WHEN d.mode = 'digest' THEN 1 ELSE 0 END), 0) AS digest_n, \
                     MIN(CASE WHEN d.mode = 'digest' THEN m.created_at END) AS digest_since \
                     FROM seats s \
                     LEFT JOIN deliveries d ON d.seat_id = s.id AND d.notified_at IS NULL \
                       AND d.mode IN ('immediate', 'digest') \
                     LEFT JOIN messages m ON m.id = d.message_id \
                     WHERE s.org_id = $1 AND s.deleted_at IS NULL AND ($2 IS NULL OR s.id = $2) \
                     GROUP BY s.id, s.name, s.last_seq, s.focus_until ORDER BY s.name",
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

            /// Drop expired claim keys, watches and instances.
            pub async fn cleanup(c: &mut Conn, now: i64) -> Result<()> {
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
                Ok(())
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
                #[derive(sqlx::FromRow)]
                struct Row {
                    seat_id: i64,
                    kind: String,
                    focus_until: Option<i64>,
                    has: Option<i64>,
                    #[sqlx(flatten)]
                    p: PresenceRow,
                }
                let sql = format!(
                    "SELECT s.id AS seat_id, s.name AS seat, s.kind, s.focus_until, p.seat_id AS has, \
                     COALESCE(p.state, 'offline') AS state, COALESCE(p.wake_failed, FALSE) AS wake_failed, \
                     p.harness, p.node, COALESCE(p.since, 0) AS since, COALESCE(p.updated_at, 0) AS updated_at, \
                     p.attention, p.attention_hint, p.terminal_url \
                     FROM seats s LEFT JOIN presence p ON p.seat_id = s.id WHERE s.id IN ({})",
                    placeholders(1, ids.len())
                );
                let mut q = sqlx::query_as::<_, Row>(&sql);
                for id in ids {
                    q = q.bind(*id);
                }
                let mut out = Vec::new();
                for r in q.fetch_all(&mut *c).await? {
                    let has = r.has.is_some();
                    let p = r.p.into_rec()?;
                    out.push(RecipientState {
                        seat_id: r.seat_id,
                        name: p.seat.clone(),
                        kind: parse(&r.kind)?,
                        focus_until: r.focus_until,
                        presence: has.then_some(p),
                    });
                }
                Ok(out)
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
                run!(self.read, |c| q::lists(c, org_id).await)
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

            async fn delivery(&self, message_id: i64, seat_id: i64) -> $crate::Result<Option<$crate::DeliveryRec>> {
                run!(self.read, |c| q::delivery(c, message_id, seat_id).await)
            }

            async fn thread(&self, thread_id: i64, org_id: i64) -> $crate::Result<Vec<$crate::MessageRec>> {
                run!(self.read, |c| q::thread(c, thread_id, org_id).await)
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
                run!(self.read, |c| q::recipients(c, message_id).await)
            }

            async fn archive(&self, org_id: i64, query: &$crate::ArchiveQuery) -> $crate::Result<Vec<$crate::MessageRec>> {
                let query = query.clone();
                run!(self.read, |c| q::archive(c, org_id, &query).await)
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
                run!(self.read, |c| q::attention(c, user_seat_id, limit).await)
            }

            async fn message_count(&self, org_id: i64, since: i64) -> $crate::Result<i64> {
                run!(self.read, |c| q::message_count(c, org_id, since).await)
            }

            async fn search(&self, org_id: i64, query: &$crate::SearchQuery) -> $crate::Result<$crate::SearchResult> {
                if query.terms.is_empty() {
                    return Ok($crate::SearchResult::default());
                }
                let (scanned_to, exhausted) = match query.scan {
                    Some(n) => {
                        let before = query.before_id;
                        let (count, oldest) = run!(self.read, |c| q::scan_window(c, org_id, before, n).await)?;
                        (oldest, count < n)
                    }
                    None => (None, false),
                };
                if query.scan.is_some() && scanned_to.is_none() {
                    return Ok($crate::SearchResult { messages: Vec::new(), scanned_to: None, scan_exhausted: true });
                }
                let messages = self.search_impl(org_id, query, scanned_to).await?;
                Ok($crate::SearchResult { messages, scanned_to, scan_exhausted: exhausted })
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
        }
    };
}
