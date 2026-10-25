//! Tasks: the SQL shared by both backends, as `common.rs` does it for mail.
//! [`impl_task_queries!`] generates `mod tq` for one sqlx database type;
//! `impl_store_common!` invokes it and implements the task methods of
//! [`crate::Store`] on top of it.
//!
//! Every task write first takes the org's row (`UPDATE orgs ... WHERE id`),
//! which serializes the task writes of one org on Postgres (SQLite has one
//! writer): numbers, `take`, the cycle check and the `unblocked` events are
//! decided on a state no other write changes meanwhile.

use crate::model::*;
use crate::{Result, StoreError};
use amail_proto::{TaskEventKind, TaskPriority, TaskState};
use std::collections::HashMap;

/// Columns and joins of [`TaskRow`]; `$body` is `t.body` or `''`
/// (listings).
macro_rules! task_select {
    ($body:literal) => {
        concat!(
            "SELECT t.id, t.org_id, t.num, t.title, ",
            $body,
            " AS body, t.state, t.priority, t.rank, t.reporter_id, rs.name AS reporter, \
             t.assignee_id, asg.name AS assignee, t.team_id, tm.name AS team, t.parent_id, \
             pt.num AS parent_num, t.labels, t.due, t.waiting_on_id, ws.name AS waiting_on, \
             t.blocked_reason, t.thread_id, t.created_at, t.updated_at, t.closed_at, t.event_seq, \
             t.events_compacted FROM tasks t JOIN seats rs ON rs.id = t.reporter_id \
             LEFT JOIN seats asg ON asg.id = t.assignee_id LEFT JOIN teams tm ON tm.id = t.team_id \
             LEFT JOIN tasks pt ON pt.id = t.parent_id LEFT JOIN seats ws ON ws.id = t.waiting_on_id"
        )
    };
}

/// True for task `t` when one of its blockers is not closed.
pub(crate) const OPEN_BLOCKER: &str = "EXISTS (SELECT 1 FROM task_deps od JOIN tasks ob ON ob.id = od.blocker_id \
     WHERE od.task_id = t.id AND ob.state NOT IN ('done', 'cancelled'))";

/// Sort key of the shown state of `t` for [`TaskOrder::Personal`].
pub(crate) fn personal_key() -> String {
    format!(
        "CASE WHEN t.state = 'open' AND NOT {OPEN_BLOCKER} THEN 0 WHEN t.state = 'doing' THEN 1 \
         WHEN t.state = 'review' THEN 2 WHEN t.state = 'blocked' THEN 3 WHEN t.state = 'open' THEN 4 \
         ELSE 5 END"
    )
}

/// Sort key of a priority (urgent first).
pub(crate) const PRIORITY_KEY: &str = "CASE t.priority WHEN 'urgent' THEN 0 WHEN 'high' THEN 1 \
     WHEN 'normal' THEN 2 ELSE 3 END";

/// The shown state of `t` as text.
pub(crate) fn shown_state_sql() -> String {
    format!("CASE WHEN t.state = 'open' AND NOT {OPEN_BLOCKER} THEN 'ready' ELSE t.state END")
}

#[derive(sqlx::FromRow)]
pub(crate) struct TaskRow {
    id: i64,
    org_id: i64,
    num: i64,
    title: String,
    body: String,
    state: String,
    priority: String,
    rank: i64,
    reporter_id: i64,
    reporter: String,
    assignee_id: Option<i64>,
    assignee: Option<String>,
    team_id: Option<i64>,
    team: Option<String>,
    parent_id: Option<i64>,
    parent_num: Option<i64>,
    labels: String,
    due: Option<i64>,
    waiting_on_id: Option<i64>,
    waiting_on: Option<String>,
    blocked_reason: Option<String>,
    thread_id: Option<i64>,
    created_at: i64,
    updated_at: i64,
    closed_at: Option<i64>,
    event_seq: i64,
    events_compacted: i64,
}

impl TaskRow {
    pub(crate) fn into_rec(self) -> Result<TaskRec> {
        Ok(TaskRec {
            id: self.id,
            org_id: self.org_id,
            num: self.num,
            title: self.title,
            body: self.body,
            state: crate::common::parse::<TaskState>(&self.state)?,
            priority: crate::common::parse::<TaskPriority>(&self.priority)?,
            rank: self.rank,
            reporter_id: self.reporter_id,
            reporter: self.reporter,
            assignee_id: self.assignee_id,
            assignee: self.assignee,
            team_id: self.team_id,
            team: self.team,
            parent_id: self.parent_id,
            parent: self.parent_num,
            labels: labels_from_db(&self.labels),
            due: self.due,
            waiting_on_id: self.waiting_on_id,
            waiting_on: self.waiting_on,
            blocked_reason: self.blocked_reason,
            thread_id: self.thread_id,
            created_at: self.created_at,
            updated_at: self.updated_at,
            closed_at: self.closed_at,
            event_seq: self.event_seq,
            events_compacted: self.events_compacted,
            waits_on: Vec::new(),
        })
    }
}

#[derive(sqlx::FromRow)]
pub(crate) struct EventRow {
    task_id: i64,
    seq: i64,
    at: i64,
    actor: Option<String>,
    written_by: Option<String>,
    kind: String,
    field: Option<String>,
    from_value: Option<String>,
    to_value: Option<String>,
    note: Option<String>,
    message_id: Option<i64>,
}

impl EventRow {
    pub(crate) fn into_rec(self) -> Result<TaskEventRec> {
        Ok(TaskEventRec {
            task_id: self.task_id,
            seq: self.seq,
            at: self.at,
            actor: self.actor,
            written_by: self.written_by,
            kind: crate::common::parse::<TaskEventKind>(&self.kind)?,
            field: self.field,
            from: self.from_value,
            to: self.to_value,
            note: self.note,
            message_id: self.message_id,
        })
    }
}

pub(crate) const EVENT_SELECT: &str = "SELECT e.task_id, e.seq, e.at, s.name AS actor, e.written_by, e.kind, \
     e.field, e.from_value, e.to_value, e.note, e.message_id FROM task_events e \
     LEFT JOIN seats s ON s.id = e.actor_id";

/// Labels are stored as `,a,b,` (empty: none), so that one label is found
/// with `LIKE '%,a,%'`.
pub(crate) fn labels_to_db(v: &[String]) -> String {
    if v.is_empty() {
        String::new()
    } else {
        format!(",{},", v.join(","))
    }
}

pub(crate) fn labels_from_db(s: &str) -> Vec<String> {
    s.split(',')
        .filter(|x| !x.is_empty())
        .map(str::to_string)
        .collect()
}

/// The bytes a task's texts count against the storage quota (see
/// [`amail_proto::limits::STORED_BYTES_PER_TASK`]).
pub(crate) fn task_bytes(t: &TaskRec) -> i64 {
    amail_proto::limits::STORED_BYTES_PER_TASK
        + (t.title.len()
            + t.body.len()
            + labels_to_db(&t.labels).len()
            + t.blocked_reason.as_deref().map_or(0, str::len)) as i64
}

/// One event to write.
#[derive(Debug, Clone, Default)]
pub(crate) struct Ev {
    pub kind: Option<TaskEventKind>,
    pub field: Option<&'static str>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub note: Option<String>,
    pub message_id: Option<i64>,
}

impl Ev {
    pub(crate) fn new(kind: TaskEventKind) -> Self {
        Ev {
            kind: Some(kind),
            ..Default::default()
        }
    }
    pub(crate) fn field(mut self, f: &'static str) -> Self {
        self.field = Some(f);
        self
    }
    pub(crate) fn from(mut self, v: Option<String>) -> Self {
        self.from = v;
        self
    }
    pub(crate) fn to(mut self, v: Option<String>) -> Self {
        self.to = v;
        self
    }
    pub(crate) fn note(mut self, v: impl Into<String>) -> Self {
        self.note = Some(v.into());
        self
    }
    pub(crate) fn message(mut self, id: i64) -> Self {
        self.message_id = Some(id);
        self
    }
    /// The bytes this event counts against the storage quota.
    pub(crate) fn bytes(&self, written_by: Option<&str>) -> i64 {
        amail_proto::limits::STORED_BYTES_PER_TASK_EVENT
            + [&self.from, &self.to, &self.note]
                .iter()
                .map(|v| v.as_deref().map_or(0, str::len) as i64)
                .sum::<i64>()
            + written_by.map_or(0, str::len) as i64
    }
}

/// A time as events show it (RFC 3339).
pub(crate) fn ts(ms: i64) -> String {
    jiff::Timestamp::from_millisecond(ms)
        .map(|t| t.to_string())
        .unwrap_or_default()
}

/// `t12`.
pub(crate) fn tnum(n: i64) -> String {
    format!("t{n}")
}

/// The events of a change of fields, from the task before and after (in
/// a fixed order of fields).
pub(crate) fn field_events(a: &TaskRec, b: &TaskRec) -> (Vec<&'static str>, Vec<Ev>) {
    let mut fields = Vec::new();
    let mut evs = Vec::new();
    let mut ch = |name: &'static str, from: Option<String>, to: Option<String>| {
        if from != to {
            fields.push(name);
            let kind = if name == "assignee" {
                TaskEventKind::Assigned
            } else {
                TaskEventKind::Changed
            };
            let e = Ev::new(kind).field(name);
            evs.push(if name == "body" {
                e.from(from.map(|s| format!("{} bytes", s.len())))
                    .to(to.map(|s| format!("{} bytes", s.len())))
            } else {
                e.from(from).to(to)
            });
        }
    };
    ch("title", Some(a.title.clone()), Some(b.title.clone()));
    ch("body", Some(a.body.clone()), Some(b.body.clone()));
    ch(
        "state",
        Some(a.state.to_string()),
        Some(b.state.to_string()),
    );
    ch(
        "priority",
        Some(a.priority.to_string()),
        Some(b.priority.to_string()),
    );
    ch("rank", Some(a.rank.to_string()), Some(b.rank.to_string()));
    ch("assignee", a.assignee.clone(), b.assignee.clone());
    ch("team", a.team.clone(), b.team.clone());
    ch("parent", a.parent.map(tnum), b.parent.map(tnum));
    let labels = |t: &TaskRec| (!t.labels.is_empty()).then(|| t.labels.join(","));
    ch("labels", labels(a), labels(b));
    ch("due", a.due.map(ts), b.due.map(ts));
    ch("waiting_on", a.waiting_on.clone(), b.waiting_on.clone());
    ch(
        "blocked_reason",
        a.blocked_reason.clone(),
        b.blocked_reason.clone(),
    );
    (fields, evs)
}

/// Waits-on numbers per task id, from `(task id, blocker num)` rows.
pub(crate) fn group_waits(rows: Vec<(i64, i64)>) -> HashMap<i64, Vec<i64>> {
    let mut m: HashMap<i64, Vec<i64>> = HashMap::new();
    for (t, n) in rows {
        m.entry(t).or_default().push(n);
    }
    m
}

pub(crate) fn refused(code: &'static str, msg: impl Into<String>) -> StoreError {
    StoreError::Refused(code, msg.into())
}

/// Generates `mod tq`: the task queries for one sqlx database type.
macro_rules! impl_task_queries {
    ($db:ty, $dialect:expr) => {
        #[allow(dead_code)]
        pub(crate) mod tq {
            use $crate::common::{placeholders, like_pattern, Dialect};
            use $crate::model::*;
            use $crate::tasks::*;
            use $crate::{now_ms, Result, StoreError};
            use amail_proto::limits::*;
            use amail_proto::{TaskEventKind, TaskState};
            use std::collections::HashSet;

            pub(crate) type Conn = <$db as sqlx::Database>::Connection;
            type Db = $db;
            const DIALECT: Dialect = $dialect;
            const CLOSED: &str = "('done', 'cancelled')";

            /// Take the org's row for this write (see the module docs).
            pub async fn lock(c: &mut Conn, org_id: i64) -> Result<()> {
                let n = sqlx::query("UPDATE orgs SET org_task_seq = org_task_seq WHERE id = $1 AND deleted_at IS NULL")
                    .bind(org_id)
                    .execute(&mut *c)
                    .await?
                    .rows_affected();
                if n == 0 {
                    return Err(StoreError::NotFound("org".into()));
                }
                Ok(())
            }

            /// Fill in `waits_on`.
            pub async fn fill(c: &mut Conn, recs: &mut [TaskRec]) -> Result<()> {
                for chunk in recs.chunks_mut(500) {
                    let sql = format!(
                        "SELECT d.task_id, b.num FROM task_deps d JOIN tasks b ON b.id = d.blocker_id \
                         WHERE d.task_id IN ({}) AND b.state NOT IN {CLOSED} ORDER BY b.num",
                        placeholders(1, chunk.len())
                    );
                    let mut q = sqlx::query_as::<Db, (i64, i64)>(&sql);
                    for t in chunk.iter() {
                        q = q.bind(t.id);
                    }
                    let mut m = group_waits(q.fetch_all(&mut *c).await?);
                    for t in chunk.iter_mut() {
                        t.waits_on = m.remove(&t.id).unwrap_or_default();
                    }
                }
                Ok(())
            }

            pub async fn by_num(c: &mut Conn, org_id: i64, num: i64) -> Result<Option<TaskRec>> {
                let r: Option<TaskRow> = sqlx::query_as(concat!(task_select!("t.body"), " WHERE t.org_id = $1 AND t.num = $2"))
                    .bind(org_id)
                    .bind(num)
                    .fetch_optional(&mut *c)
                    .await?;
                let Some(r) = r else { return Ok(None) };
                let mut v = [r.into_rec()?];
                fill(c, &mut v).await?;
                let [t] = v;
                Ok(Some(t))
            }

            async fn must(c: &mut Conn, org_id: i64, num: i64) -> Result<TaskRec> {
                by_num(c, org_id, num)
                    .await?
                    .ok_or_else(|| StoreError::NotFound(format!("no task t{num}")))
            }

            /// Tasks by id (without bodies), by number.
            pub async fn by_ids(c: &mut Conn, ids: &[i64]) -> Result<Vec<TaskRec>> {
                let mut out = Vec::new();
                for chunk in ids.chunks(500) {
                    let sql = format!(
                        "{} WHERE t.id IN ({}) ORDER BY t.num",
                        task_select!("''"),
                        placeholders(1, chunk.len())
                    );
                    let mut q = sqlx::query_as::<Db, TaskRow>(&sql);
                    for id in chunk {
                        q = q.bind(*id);
                    }
                    for r in q.fetch_all(&mut *c).await? {
                        out.push(r.into_rec()?);
                    }
                }
                out.sort_by_key(|t| t.num);
                fill(c, &mut out).await?;
                Ok(out)
            }

            async fn ids(c: &mut Conn, sql: &'static str, id: i64) -> Result<Vec<i64>> {
                Ok(sqlx::query_scalar(sql).bind(id).fetch_all(&mut *c).await?)
            }

            /// Write one event on `task_id` and compact old ones; returns the
            /// bytes it adds (negative when compaction freed more).
            pub async fn event(c: &mut Conn, task_id: i64, org_id: i64, actor: Option<i64>, written_by: Option<&str>, at: i64, e: Ev) -> Result<i64> {
                let (seq, compacted): (i64, i64) = sqlx::query_as(
                    "UPDATE tasks SET event_seq = event_seq + 1 WHERE id = $1 RETURNING event_seq, events_compacted",
                )
                .bind(task_id)
                .fetch_one(&mut *c)
                .await?;
                let mut bytes = e.bytes(written_by);
                sqlx::query(
                    "INSERT INTO task_events (task_id, seq, org_id, at, actor_id, written_by, kind, field, \
                     from_value, to_value, note, message_id) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
                )
                .bind(task_id)
                .bind(seq)
                .bind(org_id)
                .bind(at)
                .bind(actor)
                .bind(written_by)
                .bind(e.kind.map(|k| k.as_str()).unwrap_or("changed"))
                .bind(e.field)
                .bind(&e.from)
                .bind(&e.to)
                .bind(&e.note)
                .bind(e.message_id)
                .execute(&mut *c)
                .await?;
                if seq - compacted > MAX_TASK_EVENTS {
                    let upto = seq - MAX_TASK_EVENTS;
                    let freed: i64 = sqlx::query_scalar(
                        "SELECT CAST(COALESCE(SUM($3 + COALESCE(octet_length(from_value), 0) \
                         + COALESCE(octet_length(to_value), 0) + COALESCE(octet_length(note), 0) \
                         + COALESCE(octet_length(written_by), 0)), 0) AS BIGINT) \
                         FROM task_events WHERE task_id = $1 AND seq <= $2",
                    )
                    .bind(task_id)
                    .bind(upto)
                    .bind(STORED_BYTES_PER_TASK_EVENT)
                    .fetch_one(&mut *c)
                    .await?;
                    sqlx::query("DELETE FROM task_events WHERE task_id = $1 AND seq <= $2")
                        .bind(task_id)
                        .bind(upto)
                        .execute(&mut *c)
                        .await?;
                    sqlx::query("UPDATE tasks SET events_compacted = $2 WHERE id = $1")
                        .bind(task_id)
                        .bind(upto)
                        .execute(&mut *c)
                        .await?;
                    bytes -= freed;
                }
                Ok(bytes)
            }

            /// Add `delta` to the org's task bytes; growth is refused when
            /// the org's stored and task bytes would exceed `quota`.
            pub async fn charge(c: &mut Conn, org_id: i64, delta: i64, quota: Option<i64>) -> Result<()> {
                if delta == 0 {
                    return Ok(());
                }
                let ok: Option<i64> = sqlx::query_scalar(
                    "UPDATE orgs SET task_bytes = task_bytes + $1 WHERE id = $2 \
                     AND ($1 <= 0 OR CAST($3 AS BIGINT) IS NULL OR stored_bytes + task_bytes + $1 <= $3) RETURNING id",
                )
                .bind(delta)
                .bind(org_id)
                .bind(quota)
                .fetch_optional(&mut *c)
                .await?;
                if ok.is_none() {
                    let stored: i64 = sqlx::query_scalar("SELECT stored_bytes + task_bytes FROM orgs WHERE id = $1")
                        .bind(org_id)
                        .fetch_one(&mut *c)
                        .await?;
                    return Err(StoreError::StorageQuota(stored));
                }
                Ok(())
            }

            async fn open_count(c: &mut Conn, org_id: i64) -> Result<i64> {
                Ok(sqlx::query_scalar(concat!(
                    "SELECT COUNT(*) FROM tasks WHERE org_id = $1 AND state NOT IN ('done', 'cancelled')"
                ))
                .bind(org_id)
                .fetch_one(&mut *c)
                .await?)
            }

            /// Ancestors of task `id` (itself not included), nearest first,
            /// at most `MAX_TASK_PARENT_DEPTH + 1` of them.
            async fn ancestors(c: &mut Conn, id: i64) -> Result<Vec<i64>> {
                let mut out = Vec::new();
                let mut cur = id;
                while out.len() <= MAX_TASK_PARENT_DEPTH {
                    let p: Option<Option<i64>> = sqlx::query_scalar("SELECT parent_id FROM tasks WHERE id = $1")
                        .bind(cur)
                        .fetch_optional(&mut *c)
                        .await?;
                    match p.flatten() {
                        Some(p) if !out.contains(&p) => {
                            out.push(p);
                            cur = p;
                        }
                        _ => break,
                    }
                }
                Ok(out)
            }

            /// Levels of descendants below task `id` (0: no children), at
            /// most `MAX_TASK_PARENT_DEPTH + 1`.
            async fn height(c: &mut Conn, id: i64) -> Result<usize> {
                let mut level = vec![id];
                let mut h = 0;
                while !level.is_empty() && h <= MAX_TASK_PARENT_DEPTH {
                    let sql = format!("SELECT id FROM tasks WHERE parent_id IN ({})", placeholders(1, level.len()));
                    let mut q = sqlx::query_scalar::<Db, i64>(&sql);
                    for x in &level {
                        q = q.bind(*x);
                    }
                    level = q.fetch_all(&mut *c).await?;
                    if !level.is_empty() {
                        h += 1;
                    }
                }
                Ok(h)
            }

            /// Check that task `id` (None: a new task) may get `parent` as its
            /// parent: no cycle, and no task deeper than the limit.
            async fn check_parent(c: &mut Conn, id: Option<i64>, parent: &TaskRec) -> Result<()> {
                let up = ancestors(c, parent.id).await?;
                if let Some(id) = id
                    && (parent.id == id || up.contains(&id))
                {
                    return Err(refused("parent_cycle", format!("t{} is below this task; parents form a tree", parent.num)));
                }
                let below = match id {
                    Some(id) => height(c, id).await?,
                    None => 0,
                };
                if up.len() + 1 + below > MAX_TASK_PARENT_DEPTH {
                    return Err(refused(
                        "parent_too_deep",
                        format!("tasks nest at most {MAX_TASK_PARENT_DEPTH} levels below a top task"),
                    ));
                }
                Ok(())
            }

            pub async fn create(c: &mut Conn, n: &NewTaskRec) -> Result<TaskRec> {
                let now = now_ms();
                let num: Option<i64> = sqlx::query_scalar(
                    "UPDATE orgs SET org_task_seq = org_task_seq + 1 WHERE id = $1 AND deleted_at IS NULL RETURNING org_task_seq",
                )
                .bind(n.org_id)
                .fetch_optional(&mut *c)
                .await?;
                let num = num.ok_or_else(|| StoreError::NotFound("org".into()))?;
                if num > MAX_TASKS {
                    return Err(refused("too_many_tasks", format!("an org has at most {MAX_TASKS} tasks on this server")));
                }
                if open_count(c, n.org_id).await? >= MAX_OPEN_TASKS {
                    return Err(refused(
                        "too_many_open_tasks",
                        format!("an org has at most {MAX_OPEN_TASKS} tasks that are not done or cancelled"),
                    ));
                }
                let team_id = match n.team_id {
                    Some(t) => Some(t),
                    None => {
                        let seat = n.assignee_id.unwrap_or(n.reporter_id);
                        let t: Option<Option<i64>> = sqlx::query_scalar("SELECT team_id FROM seats WHERE id = $1")
                            .bind(seat)
                            .fetch_optional(&mut *c)
                            .await?;
                        match t.flatten() {
                            Some(t) => Some(t),
                            None if n.assignee_id.is_some() => {
                                let t: Option<Option<i64>> = sqlx::query_scalar("SELECT team_id FROM seats WHERE id = $1")
                                    .bind(n.reporter_id)
                                    .fetch_optional(&mut *c)
                                    .await?;
                                t.flatten()
                            }
                            None => None,
                        }
                    }
                };
                let parent = match n.parent {
                    Some(p) => {
                        let p = must(c, n.org_id, p).await?;
                        check_parent(c, None, &p).await?;
                        Some(p.id)
                    }
                    None => None,
                };
                let rank: i64 = sqlx::query_scalar("SELECT CAST(COALESCE(MAX(rank), 0) + 1024 AS BIGINT) FROM tasks WHERE org_id = $1")
                    .bind(n.org_id)
                    .fetch_one(&mut *c)
                    .await?;
                let id: i64 = sqlx::query_scalar(
                    "INSERT INTO tasks (org_id, num, title, body, state, priority, rank, reporter_id, assignee_id, \
                     team_id, parent_id, labels, due, created_at, updated_at) \
                     VALUES ($1, $2, $3, $4, 'open', $5, $6, $7, $8, $9, $10, $11, $12, $13, $13) RETURNING id",
                )
                .bind(n.org_id)
                .bind(num)
                .bind(&n.title)
                .bind(&n.body)
                .bind(n.priority.unwrap_or(amail_proto::TaskPriority::Normal).as_str())
                .bind(rank)
                .bind(n.reporter_id)
                .bind(n.assignee_id)
                .bind(team_id)
                .bind(parent)
                .bind(labels_to_db(&n.labels))
                .bind(n.due)
                .bind(now)
                .fetch_one(&mut *c)
                .await?;
                let actor = Some(n.reporter_id);
                let wb = n.written_by.as_deref();
                let t = must(c, n.org_id, num).await?;
                let mut bytes = task_bytes(&t);
                bytes += event(c, id, n.org_id, actor, wb, now, Ev::new(TaskEventKind::Created).to(Some(t.title.clone()))).await?;
                if let Some(a) = &t.assignee {
                    bytes += event(c, id, n.org_id, actor, wb, now, Ev::new(TaskEventKind::Assigned).field("assignee").to(Some(a.clone()))).await?;
                }
                if let Some(th) = n.link_thread {
                    sqlx::query("INSERT INTO task_links (task_id, thread_id, org_id, seat_id, created_at) VALUES ($1, $2, $3, $4, $5)")
                        .bind(id)
                        .bind(th)
                        .bind(n.org_id)
                        .bind(actor)
                        .bind(now)
                        .execute(&mut *c)
                        .await?;
                    bytes += STORED_BYTES_PER_TASK_EVENT;
                    bytes += event(c, id, n.org_id, actor, wb, now, Ev::new(TaskEventKind::Linked).to(Some(th.to_string()))).await?;
                }
                charge(c, n.org_id, bytes, n.quota).await?;
                must(c, n.org_id, num).await
            }

            /// After task `closed` was closed: its unfinished dependents,
            /// and those whose last open blocker it was (with an
            /// `unblocked` event each).
            async fn after_close(c: &mut Conn, w: &TaskWrite, closed: &TaskRec, now: i64, bytes: &mut i64) -> Result<(Vec<TaskRec>, Vec<TaskRec>)> {
                let ids: Vec<i64> = sqlx::query_scalar(concat!(
                    "SELECT d.task_id FROM task_deps d JOIN tasks x ON x.id = d.task_id \
                     WHERE d.blocker_id = $1 AND x.state NOT IN ('done', 'cancelled')"
                ))
                .bind(closed.id)
                .fetch_all(&mut *c)
                .await?;
                let deps = by_ids(c, &ids).await?;
                let mut unblocked = Vec::new();
                for d in &deps {
                    if d.waits_on.is_empty() {
                        let e = Ev::new(TaskEventKind::Unblocked)
                            .to(Some(tnum(closed.num)))
                            .note(format!("t{} {}", closed.num, closed.state));
                        *bytes += event(c, d.id, w.org_id, w.actor_id, w.written_by.as_deref(), now, e).await?;
                        unblocked.push(d.clone());
                    }
                }
                Ok((deps, unblocked))
            }

            pub async fn change(c: &mut Conn, w: &TaskWrite, ch: &TaskChange) -> Result<TaskChanged> {
                lock(c, w.org_id).await?;
                let now = now_ms();
                let before = must(c, w.org_id, w.num).await?;
                let mut t = before.clone();
                if let Some(s) = ch.state {
                    if s == TaskState::Ready {
                        return Err(StoreError::Invalid("`ready` is derived and cannot be set; set `open`".into()));
                    }
                    if before.state.is_closed() && s != before.state && !matches!(s, TaskState::Open | TaskState::Doing) {
                        return Err(refused(
                            "invalid_transition",
                            format!("t{} is {}: it can only be reopened (open or doing)", before.num, before.state),
                        ));
                    }
                    if before.state.is_closed() && !s.is_closed() && open_count(c, w.org_id).await? >= MAX_OPEN_TASKS {
                        return Err(refused(
                            "too_many_open_tasks",
                            format!("an org has at most {MAX_OPEN_TASKS} tasks that are not done or cancelled"),
                        ));
                    }
                    t.state = s;
                }
                let blocked = t.state == TaskState::Blocked;
                let sets_block = matches!(ch.waiting_on_id, Some(Some(_))) || matches!(ch.blocked_reason, Some(Some(_)));
                if sets_block && !blocked {
                    return Err(refused("not_blocked", "waiting_on and blocked_reason go with the state `blocked`"));
                }
                if let Some(v) = &ch.title {
                    t.title = v.clone();
                }
                if let Some(v) = &ch.body {
                    t.body = v.clone();
                }
                if let Some(v) = ch.priority {
                    t.priority = v;
                }
                if let Some(v) = ch.rank {
                    t.rank = v;
                }
                if let Some(v) = ch.assignee_id {
                    t.assignee_id = v;
                }
                if let Some(v) = ch.team_id {
                    t.team_id = v;
                }
                if let Some(v) = &ch.labels {
                    t.labels = v.clone();
                }
                if let Some(v) = ch.due {
                    t.due = v;
                }
                if let Some(v) = ch.waiting_on_id {
                    t.waiting_on_id = v;
                }
                if let Some(v) = &ch.blocked_reason {
                    t.blocked_reason = v.clone();
                }
                if !blocked {
                    t.waiting_on_id = None;
                    t.blocked_reason = None;
                }
                if let Some(p) = ch.parent {
                    t.parent_id = match p {
                        None => None,
                        Some(pn) => {
                            let p = must(c, w.org_id, pn).await?;
                            if before.parent != Some(pn) {
                                check_parent(c, Some(before.id), &p).await?;
                            }
                            Some(p.id)
                        }
                    };
                }
                if t.state != before.state {
                    t.closed_at = if t.state.is_closed() { Some(now) } else { None };
                }
                sqlx::query(
                    "UPDATE tasks SET title = $2, body = $3, state = $4, priority = $5, rank = $6, assignee_id = $7, \
                     team_id = $8, parent_id = $9, labels = $10, due = $11, waiting_on_id = $12, blocked_reason = $13, \
                     closed_at = $14, due_reminded_at = CASE WHEN $15 THEN NULL ELSE due_reminded_at END WHERE id = $1",
                )
                .bind(before.id)
                .bind(&t.title)
                .bind(&t.body)
                .bind(t.state.as_str())
                .bind(t.priority.as_str())
                .bind(t.rank)
                .bind(t.assignee_id)
                .bind(t.team_id)
                .bind(t.parent_id)
                .bind(labels_to_db(&t.labels))
                .bind(t.due)
                .bind(t.waiting_on_id)
                .bind(&t.blocked_reason)
                .bind(t.closed_at)
                .bind(t.due != before.due)
                .execute(&mut *c)
                .await?;
                let after = must(c, w.org_id, w.num).await?;
                let (fields, evs) = field_events(&before, &after);
                let mut bytes = task_bytes(&after) - task_bytes(&before);
                let wb = w.written_by.as_deref();
                for e in evs {
                    bytes += event(c, before.id, w.org_id, w.actor_id, wb, now, e).await?;
                }
                let (mut dependents, mut unblocked) = (Vec::new(), Vec::new());
                if after.state.is_closed() && !before.state.is_closed() {
                    (dependents, unblocked) = after_close(c, w, &after, now, &mut bytes).await?;
                }
                if !fields.is_empty() {
                    sqlx::query("UPDATE tasks SET updated_at = $2 WHERE id = $1")
                        .bind(before.id)
                        .bind(now)
                        .execute(&mut *c)
                        .await?;
                }
                charge(c, w.org_id, bytes, w.quota).await?;
                let task = must(c, w.org_id, w.num).await?;
                Ok(TaskChanged {
                    before,
                    task,
                    fields: fields.into_iter().map(str::to_string).collect(),
                    unblocked,
                    dependents,
                })
            }

            fn unchanged(t: TaskRec) -> TaskChanged {
                TaskChanged {
                    before: t.clone(),
                    task: t,
                    fields: Vec::new(),
                    unblocked: Vec::new(),
                    dependents: Vec::new(),
                }
            }

            pub async fn take(c: &mut Conn, w: &TaskWrite) -> Result<TaskChanged> {
                lock(c, w.org_id).await?;
                let now = now_ms();
                let before = must(c, w.org_id, w.num).await?;
                let Some(actor) = w.actor_id else {
                    return Err(StoreError::Invalid("take needs a seat".into()));
                };
                if before.assignee_id == Some(actor) {
                    return Ok(unchanged(before));
                }
                if before.state.is_closed() {
                    return Err(refused("closed", format!("t{} is {}", before.num, before.state)));
                }
                let n = sqlx::query("UPDATE tasks SET assignee_id = $2, updated_at = $3 WHERE id = $1 AND assignee_id IS NULL")
                    .bind(before.id)
                    .bind(actor)
                    .bind(now)
                    .execute(&mut *c)
                    .await?
                    .rows_affected();
                if n == 0 {
                    return Err(refused(
                        "taken",
                        format!("t{} is assigned to {}", before.num, before.assignee.clone().unwrap_or_default()),
                    ));
                }
                let task = must(c, w.org_id, w.num).await?;
                let e = Ev::new(TaskEventKind::Assigned).field("assignee").to(task.assignee.clone()).note("taken");
                let bytes = event(c, before.id, w.org_id, w.actor_id, w.written_by.as_deref(), now, e).await?;
                charge(c, w.org_id, bytes, w.quota).await?;
                Ok(TaskChanged {
                    before,
                    task,
                    fields: vec!["assignee".into()],
                    unblocked: Vec::new(),
                    dependents: Vec::new(),
                })
            }

            pub async fn add_dep(c: &mut Conn, w: &TaskWrite, blocker: i64) -> Result<TaskChanged> {
                lock(c, w.org_id).await?;
                let now = now_ms();
                let before = must(c, w.org_id, w.num).await?;
                let b = must(c, w.org_id, blocker).await?;
                if b.id == before.id {
                    return Err(refused("dependency_cycle", "a task cannot depend on itself"));
                }
                let exists: Option<i64> = sqlx::query_scalar("SELECT task_id FROM task_deps WHERE task_id = $1 AND blocker_id = $2")
                    .bind(before.id)
                    .bind(b.id)
                    .fetch_optional(&mut *c)
                    .await?;
                if exists.is_some() {
                    return Ok(unchanged(before));
                }
                let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM task_deps WHERE task_id = $1")
                    .bind(before.id)
                    .fetch_one(&mut *c)
                    .await?;
                if n >= MAX_TASK_BLOCKERS {
                    return Err(refused("too_many_blockers", format!("a task has at most {MAX_TASK_BLOCKERS} blockers")));
                }
                let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM task_deps WHERE blocker_id = $1")
                    .bind(b.id)
                    .fetch_one(&mut *c)
                    .await?;
                if n >= MAX_TASK_BLOCKED {
                    return Err(refused("too_many_blocked", format!("a task blocks at most {MAX_TASK_BLOCKED} tasks")));
                }
                // Cycle: the new blocker depends, directly or not, on this task.
                let mut seen: HashSet<i64> = HashSet::from([b.id]);
                let mut level = vec![b.id];
                let mut depth = 0;
                while !level.is_empty() {
                    if depth >= MAX_TASK_DEP_DEPTH {
                        return Err(refused(
                            "dependency_too_deep",
                            format!("dependency chains are limited to {MAX_TASK_DEP_DEPTH} tasks"),
                        ));
                    }
                    let sql = format!("SELECT blocker_id FROM task_deps WHERE task_id IN ({})", placeholders(1, level.len()));
                    let mut q = sqlx::query_scalar::<Db, i64>(&sql);
                    for x in &level {
                        q = q.bind(*x);
                    }
                    let next: Vec<i64> = q.fetch_all(&mut *c).await?;
                    if next.contains(&before.id) {
                        return Err(refused(
                            "dependency_cycle",
                            format!("t{} already depends on t{}: dependencies cannot form a cycle", b.num, before.num),
                        ));
                    }
                    level = next.into_iter().filter(|x| seen.insert(*x)).collect();
                    depth += 1;
                }
                sqlx::query("INSERT INTO task_deps (task_id, blocker_id, org_id, created_at) VALUES ($1, $2, $3, $4)")
                    .bind(before.id)
                    .bind(b.id)
                    .bind(w.org_id)
                    .bind(now)
                    .execute(&mut *c)
                    .await?;
                let wb = w.written_by.as_deref();
                let mut bytes = STORED_BYTES_PER_TASK_EVENT;
                let e = Ev::new(TaskEventKind::DependencyAdded).field("blocked_by").to(Some(tnum(b.num)));
                bytes += event(c, before.id, w.org_id, w.actor_id, wb, now, e).await?;
                let e = Ev::new(TaskEventKind::DependencyAdded).field("blocks").to(Some(tnum(before.num)));
                bytes += event(c, b.id, w.org_id, w.actor_id, wb, now, e).await?;
                touch(c, &[before.id, b.id], now).await?;
                charge(c, w.org_id, bytes, w.quota).await?;
                let task = must(c, w.org_id, w.num).await?;
                Ok(TaskChanged {
                    before,
                    task,
                    fields: vec!["blocked_by".into()],
                    unblocked: Vec::new(),
                    dependents: Vec::new(),
                })
            }

            async fn touch(c: &mut Conn, ids: &[i64], now: i64) -> Result<()> {
                for id in ids {
                    sqlx::query("UPDATE tasks SET updated_at = $2 WHERE id = $1").bind(*id).bind(now).execute(&mut *c).await?;
                }
                Ok(())
            }

            pub async fn remove_dep(c: &mut Conn, w: &TaskWrite, blocker: i64) -> Result<TaskChanged> {
                lock(c, w.org_id).await?;
                let now = now_ms();
                let before = must(c, w.org_id, w.num).await?;
                let b = must(c, w.org_id, blocker).await?;
                let n = sqlx::query("DELETE FROM task_deps WHERE task_id = $1 AND blocker_id = $2")
                    .bind(before.id)
                    .bind(b.id)
                    .execute(&mut *c)
                    .await?
                    .rows_affected();
                if n == 0 {
                    return Err(StoreError::NotFound(format!("t{} does not depend on t{}", before.num, b.num)));
                }
                let wb = w.written_by.as_deref();
                let mut bytes = -STORED_BYTES_PER_TASK_EVENT;
                let e = Ev::new(TaskEventKind::DependencyRemoved).field("blocked_by").to(Some(tnum(b.num)));
                bytes += event(c, before.id, w.org_id, w.actor_id, wb, now, e).await?;
                let e = Ev::new(TaskEventKind::DependencyRemoved).field("blocks").to(Some(tnum(before.num)));
                bytes += event(c, b.id, w.org_id, w.actor_id, wb, now, e).await?;
                touch(c, &[before.id, b.id], now).await?;
                let task = must(c, w.org_id, w.num).await?;
                let mut unblocked = Vec::new();
                if !before.waits_on.is_empty() && task.waits_on.is_empty() && !task.state.is_closed() {
                    let e = Ev::new(TaskEventKind::Unblocked)
                        .to(Some(tnum(b.num)))
                        .note(format!("dependency on t{} removed", b.num));
                    bytes += event(c, before.id, w.org_id, w.actor_id, wb, now, e).await?;
                    unblocked.push(task.clone());
                }
                charge(c, w.org_id, bytes, w.quota).await?;
                let task = must(c, w.org_id, w.num).await?;
                Ok(TaskChanged {
                    before,
                    task,
                    fields: vec!["blocked_by".into()],
                    unblocked,
                    dependents: Vec::new(),
                })
            }

            /// Link a thread. `auto` (from a `[tNN]` subject): no error when
            /// the task does not exist, the thread is its discussion thread
            /// or the task has its most links; false then.
            pub async fn add_link(c: &mut Conn, w: &TaskWrite, thread_id: i64, auto: bool, note: Option<&str>) -> Result<bool> {
                lock(c, w.org_id).await?;
                let now = now_ms();
                let t = match by_num(c, w.org_id, w.num).await? {
                    Some(t) => t,
                    None if auto => return Ok(false),
                    None => return Err(StoreError::NotFound(format!("no task t{}", w.num))),
                };
                if auto && t.thread_id == Some(thread_id) {
                    return Ok(false);
                }
                let exists: Option<i64> = sqlx::query_scalar("SELECT task_id FROM task_links WHERE task_id = $1 AND thread_id = $2")
                    .bind(t.id)
                    .bind(thread_id)
                    .fetch_optional(&mut *c)
                    .await?;
                if exists.is_some() {
                    return Ok(false);
                }
                let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM task_links WHERE task_id = $1")
                    .bind(t.id)
                    .fetch_one(&mut *c)
                    .await?;
                if n >= MAX_TASK_LINKS {
                    if auto {
                        return Ok(false);
                    }
                    return Err(refused("too_many_links", format!("a task has at most {MAX_TASK_LINKS} linked threads")));
                }
                sqlx::query("INSERT INTO task_links (task_id, thread_id, org_id, seat_id, created_at) VALUES ($1, $2, $3, $4, $5)")
                    .bind(t.id)
                    .bind(thread_id)
                    .bind(w.org_id)
                    .bind(w.actor_id)
                    .bind(now)
                    .execute(&mut *c)
                    .await?;
                let mut e = Ev::new(TaskEventKind::Linked).to(Some(thread_id.to_string()));
                if let Some(n) = note {
                    e = e.note(n);
                }
                let bytes = STORED_BYTES_PER_TASK_EVENT + event(c, t.id, w.org_id, w.actor_id, w.written_by.as_deref(), now, e).await?;
                touch(c, &[t.id], now).await?;
                charge(c, w.org_id, bytes, w.quota).await?;
                Ok(true)
            }

            pub async fn remove_link(c: &mut Conn, w: &TaskWrite, thread_id: i64) -> Result<bool> {
                lock(c, w.org_id).await?;
                let now = now_ms();
                let t = must(c, w.org_id, w.num).await?;
                let n = sqlx::query("DELETE FROM task_links WHERE task_id = $1 AND thread_id = $2")
                    .bind(t.id)
                    .bind(thread_id)
                    .execute(&mut *c)
                    .await?
                    .rows_affected();
                if n == 0 {
                    return Ok(false);
                }
                let e = Ev::new(TaskEventKind::Unlinked).to(Some(thread_id.to_string()));
                let bytes = event(c, t.id, w.org_id, w.actor_id, w.written_by.as_deref(), now, e).await? - STORED_BYTES_PER_TASK_EVENT;
                touch(c, &[t.id], now).await?;
                charge(c, w.org_id, bytes, w.quota).await?;
                Ok(true)
            }

            /// Record a comment (message `message_id` in `thread_id`); the
            /// first one makes `thread_id` the task's discussion thread.
            pub async fn comment(c: &mut Conn, w: &TaskWrite, message_id: i64, thread_id: i64) -> Result<TaskRec> {
                lock(c, w.org_id).await?;
                let now = now_ms();
                let t = must(c, w.org_id, w.num).await?;
                sqlx::query("UPDATE tasks SET thread_id = $2 WHERE id = $1 AND thread_id IS NULL")
                    .bind(t.id)
                    .bind(thread_id)
                    .execute(&mut *c)
                    .await?;
                let e = Ev::new(TaskEventKind::Commented).message(message_id);
                let bytes = event(c, t.id, w.org_id, w.actor_id, w.written_by.as_deref(), now, e).await?;
                touch(c, &[t.id], now).await?;
                charge(c, w.org_id, bytes, None).await?;
                must(c, w.org_id, w.num).await
            }

            pub async fn reminder(c: &mut Conn, org_id: i64, task_id: i64, kind: &str, to: &str, message_id: i64) -> Result<()> {
                lock(c, org_id).await?;
                let exists: Option<i64> = sqlx::query_scalar("SELECT id FROM tasks WHERE id = $1 AND org_id = $2")
                    .bind(task_id)
                    .bind(org_id)
                    .fetch_optional(&mut *c)
                    .await?;
                if exists.is_none() {
                    return Ok(());
                }
                let mut e = Ev::new(TaskEventKind::ReminderSent).to(Some(to.to_string())).message(message_id);
                e.field = Some(match kind {
                    "assigned" => "assigned",
                    "unblocked" => "unblocked",
                    "waiting" => "waiting",
                    "closed" => "closed",
                    "urgent" => "urgent",
                    "stale" => "stale",
                    "due" => "due",
                    _ => "other",
                });
                let bytes = event(c, task_id, org_id, None, None, now_ms(), e).await?;
                charge(c, org_id, bytes, None).await
            }

            pub async fn events(c: &mut Conn, org_id: i64, num: i64, after: i64, limit: i64) -> Result<Option<(Vec<TaskEventRec>, i64)>> {
                let Some(t) = by_num(c, org_id, num).await? else { return Ok(None) };
                let sql = format!("{EVENT_SELECT} WHERE e.task_id = $1 AND e.seq > $2 ORDER BY e.seq LIMIT $3");
                let rows: Vec<EventRow> = sqlx::query_as(&sql).bind(t.id).bind(after).bind(limit).fetch_all(&mut *c).await?;
                let evs = rows.into_iter().map(EventRow::into_rec).collect::<Result<Vec<_>>>()?;
                Ok(Some((evs, t.events_compacted)))
            }

            pub async fn detail(c: &mut Conn, org_id: i64, num: i64, nevents: i64) -> Result<Option<TaskDetailRec>> {
                let Some(task) = by_num(c, org_id, num).await? else { return Ok(None) };
                let ids_b = ids(c, "SELECT blocker_id FROM task_deps WHERE task_id = $1", task.id).await?;
                let blockers = by_ids(c, &ids_b).await?;
                let ids_d = ids(c, "SELECT task_id FROM task_deps WHERE blocker_id = $1", task.id).await?;
                let blocked = by_ids(c, &ids_d).await?;
                let ids_c = ids(c, "SELECT id FROM tasks WHERE parent_id = $1 ORDER BY num LIMIT 500", task.id).await?;
                let children = by_ids(c, &ids_c).await?;
                let links: Vec<(i64, Option<String>, i64)> = sqlx::query_as(
                    "SELECT l.thread_id, s.name, l.created_at FROM task_links l LEFT JOIN seats s ON s.id = l.seat_id \
                     WHERE l.task_id = $1 ORDER BY l.created_at, l.thread_id",
                )
                .bind(task.id)
                .fetch_all(&mut *c)
                .await?;
                let sql = format!("{EVENT_SELECT} WHERE e.task_id = $1 ORDER BY e.seq DESC LIMIT $2");
                let rows: Vec<EventRow> = sqlx::query_as(&sql).bind(task.id).bind(nevents).fetch_all(&mut *c).await?;
                let mut events = rows.into_iter().map(EventRow::into_rec).collect::<Result<Vec<_>>>()?;
                events.reverse();
                Ok(Some(TaskDetailRec {
                    task,
                    blockers,
                    blocked,
                    children,
                    links: links
                        .into_iter()
                        .map(|(thread_id, seat, created_at)| TaskLinkRec { thread_id, seat, created_at })
                        .collect(),
                    events,
                }))
            }

            fn team_clause(b: &mut sqlx::QueryBuilder<'_, Db>, team: i64) {
                b.push(" AND (t.team_id = ");
                b.push_bind(team);
                b.push(" OR t.assignee_id IN (SELECT id FROM seats WHERE team_id = ");
                b.push_bind(team);
                b.push(") OR t.reporter_id IN (SELECT id FROM seats WHERE team_id = ");
                b.push_bind(team);
                b.push("))");
            }

            pub async fn list(c: &mut Conn, q: &TaskQuery) -> Result<TaskPage> {
                let limit = q.limit.clamp(1, MAX_TASK_PAGE);
                // A text search looks at a bounded window of tasks.
                let mut window_lo = None;
                if q.text.is_some() {
                    let lo: Option<i64> = sqlx::query_scalar(
                        "SELECT num FROM tasks WHERE org_id = $1 AND num < $2 ORDER BY num DESC LIMIT 1 OFFSET $3",
                    )
                    .bind(q.org_id)
                    .bind(q.before.unwrap_or(i64::MAX))
                    .bind(TASK_TEXT_SCAN - 1)
                    .fetch_optional(&mut *c)
                    .await?;
                    window_lo = lo;
                }
                let mut b = sqlx::QueryBuilder::<Db>::new(concat!(task_select!("''"), " WHERE t.org_id = "));
                b.push_bind(q.org_id);
                if let Some(before) = q.before {
                    b.push(" AND t.num < ").push_bind(before);
                }
                if let Some(lo) = window_lo {
                    b.push(" AND t.num >= ").push_bind(lo);
                }
                if !q.states.is_empty() {
                    b.push(" AND (FALSE");
                    for s in &q.states {
                        match s {
                            TaskState::Ready => {
                                b.push(format!(" OR (t.state = 'open' AND NOT {OPEN_BLOCKER})"));
                            }
                            TaskState::Open => {
                                b.push(format!(" OR (t.state = 'open' AND {OPEN_BLOCKER})"));
                            }
                            other => {
                                b.push(" OR t.state = ").push_bind(other.as_str());
                            }
                        }
                    }
                    b.push(")");
                }
                if q.waiting_only {
                    b.push(format!(" AND t.state NOT IN {CLOSED} AND {OPEN_BLOCKER}"));
                }
                match (q.assignee_id, q.pool_team_id) {
                    (Some(Some(a)), Some(team)) => {
                        b.push(" AND (t.assignee_id = ").push_bind(a);
                        b.push(" OR (t.assignee_id IS NULL AND t.team_id = ").push_bind(team);
                        b.push("))");
                    }
                    (Some(Some(a)), None) => {
                        b.push(" AND t.assignee_id = ").push_bind(a);
                    }
                    (Some(None), _) => {
                        b.push(" AND t.assignee_id IS NULL");
                    }
                    (None, _) => {}
                }
                if let Some(r) = q.reporter_id {
                    b.push(" AND t.reporter_id = ").push_bind(r);
                }
                if let Some(team) = q.team_id {
                    team_clause(&mut b, team);
                }
                if let Some(p) = q.priority {
                    b.push(" AND t.priority = ").push_bind(p.as_str());
                }
                if let Some(l) = &q.label {
                    b.push(" AND t.labels LIKE ").push_bind(crate::common::addr_pattern(l));
                    b.push(" ESCAPE '\\'");
                }
                if let Some(p) = q.parent {
                    b.push(" AND pt.num = ").push_bind(p);
                }
                if let Some(d) = q.due_before {
                    b.push(format!(" AND t.state NOT IN {CLOSED} AND t.due IS NOT NULL AND t.due < "));
                    b.push_bind(d);
                }
                if let Some(s) = q.stale_before {
                    b.push(" AND t.state IN ('doing', 'review') AND t.updated_at < ").push_bind(s);
                }
                if let Some(text) = &q.text {
                    let pat = like_pattern(text);
                    let op = DIALECT.ilike();
                    b.push(format!(" AND (t.title {op} ")).push_bind(pat.clone());
                    b.push(format!(" ESCAPE '\\' OR t.body {op} ")).push_bind(pat);
                    b.push(" ESCAPE '\\')");
                }
                match q.order {
                    TaskOrder::Newest => b.push(" ORDER BY t.num DESC"),
                    TaskOrder::Personal => b.push(format!(" ORDER BY {}, {PRIORITY_KEY}, t.rank, t.num", personal_key())),
                    TaskOrder::Priority => b.push(format!(" ORDER BY {PRIORITY_KEY}, t.rank, t.num")),
                    TaskOrder::Closed => b.push(" ORDER BY t.closed_at DESC, t.num DESC"),
                };
                b.push(" LIMIT ").push_bind(limit + 1);
                let rows: Vec<TaskRow> = b.build_query_as().fetch_all(&mut *c).await?;
                let mut tasks = rows.into_iter().map(TaskRow::into_rec).collect::<Result<Vec<_>>>()?;
                let truncated = tasks.len() as i64 > limit;
                tasks.truncate(limit as usize);
                fill(c, &mut tasks).await?;
                let next_before = match q.order {
                    TaskOrder::Newest if truncated => tasks.last().map(|t| t.num),
                    TaskOrder::Newest => window_lo,
                    _ => None,
                };
                Ok(TaskPage { tasks, next_before, truncated })
            }

            pub async fn waiting(c: &mut Conn, org_id: i64, seat_id: i64, limit: i64) -> Result<(Vec<TaskWaitRec>, bool)> {
                let limit = limit.clamp(1, MAX_TASK_PAGE);
                let own: Vec<i64> = sqlx::query_scalar(
                    "SELECT id FROM tasks WHERE org_id = $1 AND assignee_id = $2 AND state = 'blocked' \
                     AND waiting_on_id IS NOT NULL ORDER BY num LIMIT $3",
                )
                .bind(org_id)
                .bind(seat_id)
                .bind(limit + 1)
                .fetch_all(&mut *c)
                .await?;
                let pairs: Vec<(i64, i64)> = sqlx::query_as(concat!(
                    "SELECT d.task_id, d.blocker_id FROM task_deps d JOIN tasks t ON t.id = d.task_id \
                     JOIN tasks b ON b.id = d.blocker_id WHERE t.org_id = $1 AND t.assignee_id = $2 \
                     AND t.state NOT IN ('done', 'cancelled') AND b.state NOT IN ('done', 'cancelled') \
                     ORDER BY t.num, b.num LIMIT $3"
                ))
                .bind(org_id)
                .bind(seat_id)
                .bind(limit + 1)
                .fetch_all(&mut *c)
                .await?;
                let mut all: Vec<i64> = own.clone();
                all.extend(pairs.iter().flat_map(|p| [p.0, p.1]));
                let recs = by_ids(c, &all).await?;
                let get = |id: i64| recs.iter().find(|t| t.id == id).cloned();
                let mut out = Vec::new();
                for id in own {
                    if let Some(t) = get(id) {
                        out.push(TaskWaitRec { seat: t.waiting_on.clone(), task: t, other: None });
                    }
                }
                for (tid, bid) in pairs {
                    if let (Some(t), Some(b)) = (get(tid), get(bid)) {
                        out.push(TaskWaitRec { seat: Some(b.responsible().1.to_string()), task: t, other: Some(b) });
                    }
                }
                let truncated = out.len() as i64 > limit;
                out.truncate(limit as usize);
                Ok((out, truncated))
            }

            pub async fn owed(c: &mut Conn, org_id: i64, seat_id: i64, limit: i64) -> Result<(Vec<TaskWaitRec>, bool)> {
                let limit = limit.clamp(1, MAX_TASK_PAGE);
                let waiting: Vec<i64> = sqlx::query_scalar(
                    "SELECT id FROM tasks WHERE org_id = $1 AND waiting_on_id = $2 AND state = 'blocked' \
                     AND COALESCE(assignee_id, reporter_id) <> $2 ORDER BY num LIMIT $3",
                )
                .bind(org_id)
                .bind(seat_id)
                .bind(limit + 1)
                .fetch_all(&mut *c)
                .await?;
                let pairs: Vec<(i64, i64)> = sqlx::query_as(concat!(
                    "SELECT d.blocker_id, d.task_id FROM task_deps d JOIN tasks b ON b.id = d.blocker_id \
                     JOIN tasks x ON x.id = d.task_id WHERE b.org_id = $1 \
                     AND COALESCE(b.assignee_id, b.reporter_id) = $2 \
                     AND b.state NOT IN ('done', 'cancelled') AND x.state NOT IN ('done', 'cancelled') \
                     AND COALESCE(x.assignee_id, x.reporter_id) <> $2 \
                     AND NOT EXISTS (SELECT 1 FROM task_deps d2 JOIN tasks b2 ON b2.id = d2.blocker_id \
                       WHERE d2.task_id = x.id AND d2.blocker_id <> b.id AND b2.state NOT IN ('done', 'cancelled')) \
                     ORDER BY b.num, x.num LIMIT $3"
                ))
                .bind(org_id)
                .bind(seat_id)
                .bind(limit + 1)
                .fetch_all(&mut *c)
                .await?;
                let mut all: Vec<i64> = waiting.clone();
                all.extend(pairs.iter().flat_map(|p| [p.0, p.1]));
                let recs = by_ids(c, &all).await?;
                let get = |id: i64| recs.iter().find(|t| t.id == id).cloned();
                let mut out = Vec::new();
                for id in waiting {
                    if let Some(t) = get(id) {
                        out.push(TaskWaitRec { seat: Some(t.responsible().1.to_string()), task: t, other: None });
                    }
                }
                for (bid, xid) in pairs {
                    if let (Some(b), Some(x)) = (get(bid), get(xid)) {
                        out.push(TaskWaitRec { seat: Some(x.responsible().1.to_string()), task: b, other: Some(x) });
                    }
                }
                let truncated = out.len() as i64 > limit;
                out.truncate(limit as usize);
                Ok((out, truncated))
            }

            pub async fn board(c: &mut Conn, org_id: i64, team: Option<i64>, stale_hours: i64, now: i64) -> Result<TaskBoardRec> {
                let mut b = sqlx::QueryBuilder::<Db>::new(format!(
                    "SELECT {} AS s, COUNT(*) AS n FROM tasks t WHERE t.org_id = ",
                    shown_state_sql()
                ));
                b.push_bind(org_id);
                if let Some(team) = team {
                    team_clause(&mut b, team);
                }
                b.push(" GROUP BY 1");
                let counts: Vec<(String, i64)> = b.build_query_as().fetch_all(&mut *c).await?;
                let base = TaskQuery { org_id, team_id: team, limit: BOARD_COLUMN_TASKS, ..Default::default() };
                let mut out = TaskBoardRec::default();
                for s in [TaskState::Ready, TaskState::Open, TaskState::Doing, TaskState::Blocked, TaskState::Review, TaskState::Done, TaskState::Cancelled] {
                    let n = counts.iter().find(|(x, _)| x == s.as_str()).map_or(0, |x| x.1);
                    let tasks = if n == 0 {
                        Vec::new()
                    } else {
                        let order = if s.is_closed() { TaskOrder::Closed } else { TaskOrder::Priority };
                        list(c, &TaskQuery { states: vec![s], order, ..base.clone() }).await?.tasks
                    };
                    out.columns.push((s, n, tasks));
                }
                let mut b = sqlx::QueryBuilder::<Db>::new(format!(
                    "SELECT s.name, \
                     CAST(SUM(CASE WHEN t.state = 'doing' THEN 1 ELSE 0 END) AS BIGINT), \
                     CAST(SUM(CASE WHEN t.state = 'open' AND NOT {ob} THEN 1 ELSE 0 END) AS BIGINT), \
                     CAST(SUM(CASE WHEN t.state = 'blocked' THEN 1 ELSE 0 END) AS BIGINT), \
                     CAST(SUM(CASE WHEN t.state = 'open' AND {ob} THEN 1 ELSE 0 END) AS BIGINT), \
                     CAST(SUM(CASE WHEN t.state = 'review' THEN 1 ELSE 0 END) AS BIGINT) \
                     FROM tasks t JOIN seats s ON s.id = t.assignee_id \
                     WHERE t.state NOT IN {CLOSED} AND t.org_id = ",
                    ob = OPEN_BLOCKER
                ));
                b.push_bind(org_id);
                if let Some(team) = team {
                    team_clause(&mut b, team);
                }
                b.push(" GROUP BY s.name ORDER BY COUNT(*) DESC, s.name LIMIT 500");
                let load: Vec<(String, i64, i64, i64, i64, i64)> = b.build_query_as().fetch_all(&mut *c).await?;
                out.load = load
                    .into_iter()
                    .map(|(seat, doing, ready, blocked, waiting, review)| SeatLoadRec { seat, doing, ready, blocked, waiting, review })
                    .collect();
                if stale_hours > 0 {
                    let q = TaskQuery { stale_before: Some(now - stale_hours * 3_600_000), order: TaskOrder::Priority, ..base.clone() };
                    out.stale = list(c, &q).await?.tasks;
                }
                let q = TaskQuery { due_before: Some(now), order: TaskOrder::Priority, ..base.clone() };
                out.overdue = list(c, &q).await?.tasks;
                let q = TaskQuery { waiting_only: true, order: TaskOrder::Priority, limit: 200, ..base };
                out.chains = list(c, &q).await?.tasks;
                Ok(out)
            }

            /// Tasks whose discussion thread `thread_id` is, or that link it.
            pub async fn thread_tasks(c: &mut Conn, org_id: i64, thread_id: i64) -> Result<Vec<TaskRec>> {
                let ids: Vec<i64> = sqlx::query_scalar(
                    "SELECT id FROM tasks WHERE org_id = $1 AND thread_id = $2 \
                     UNION SELECT task_id FROM task_links WHERE org_id = $1 AND thread_id = $2 LIMIT 200",
                )
                .bind(org_id)
                .bind(thread_id)
                .fetch_all(&mut *c)
                .await?;
                by_ids(c, &ids).await
            }

            /// Mark up to `limit` unfinished tasks whose due time passed as
            /// reminded, and return them (each once, also with several
            /// instances).
            pub async fn claim_due(c: &mut Conn, now: i64, limit: i64) -> Result<Vec<TaskRec>> {
                let ids: Vec<i64> = sqlx::query_scalar(concat!(
                    "UPDATE tasks SET due_reminded_at = $1 WHERE id IN (SELECT id FROM tasks \
                     WHERE due IS NOT NULL AND due_reminded_at IS NULL AND due <= $1 \
                     AND state NOT IN ('done', 'cancelled') ORDER BY due LIMIT $2) \
                     AND due_reminded_at IS NULL RETURNING id"
                ))
                .bind(now)
                .bind(limit)
                .fetch_all(&mut *c)
                .await?;
                by_ids(c, &ids).await
            }

            /// The stale pass: for each org whose last pass is an hour old
            /// or more and whose stale period is not 0, mark up to `limit`
            /// `doing`/`review` tasks not changed (nor reminded) for the
            /// period as reminded, and return them.
            pub async fn claim_stale(c: &mut Conn, now: i64, limit: i64) -> Result<Vec<TaskRec>> {
                let orgs: Vec<(i64, i64)> = sqlx::query_as(
                    "UPDATE orgs SET task_stale_pass_at = $1 WHERE deleted_at IS NULL AND task_stale_hours > 0 \
                     AND (task_stale_pass_at IS NULL OR task_stale_pass_at <= $1 - 3600000) \
                     AND EXISTS (SELECT 1 FROM tasks t WHERE t.org_id = orgs.id AND t.state IN ('doing', 'review')) \
                     RETURNING id, task_stale_hours",
                )
                .bind(now)
                .fetch_all(&mut *c)
                .await?;
                let mut out = Vec::new();
                for (org_id, hours) in orgs {
                    let cut = now - hours * 3_600_000;
                    let ids: Vec<i64> = sqlx::query_scalar(
                        "UPDATE tasks SET stale_reminded_at = $1 WHERE id IN (SELECT id FROM tasks WHERE org_id = $2 \
                         AND state IN ('doing', 'review') AND updated_at <= $3 \
                         AND (stale_reminded_at IS NULL OR stale_reminded_at <= $3) ORDER BY updated_at LIMIT $4) \
                         AND (stale_reminded_at IS NULL OR stale_reminded_at <= $3) RETURNING id",
                    )
                    .bind(now)
                    .bind(org_id)
                    .bind(cut)
                    .bind(limit)
                    .fetch_all(&mut *c)
                    .await?;
                    out.extend(by_ids(c, &ids).await?);
                }
                Ok(out)
            }
        }
    };
}
