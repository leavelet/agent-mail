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
use amail_proto::{TaskEventKind, TaskPriority, TaskResult, TaskState};
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
             t.events_compacted, t.steal_requested_by AS steal_by_id, sb.name AS steal_by, \
             t.steal_requested_at AS steal_at, t.result, t.result_ref, t.result_note, t.result_by, t.result_at, \
             t.import_key FROM tasks t JOIN seats rs ON rs.id = t.reporter_id \
             LEFT JOIN seats asg ON asg.id = t.assignee_id LEFT JOIN teams tm ON tm.id = t.team_id \
             LEFT JOIN tasks pt ON pt.id = t.parent_id LEFT JOIN seats ws ON ws.id = t.waiting_on_id \
             LEFT JOIN seats sb ON sb.id = t.steal_requested_by"
        )
    };
}

/// SQL that is true when the dependency `$d` (a `task_deps` row) on
/// blocker `$b` (a `tasks` row) is not satisfied: the blocker is not
/// closed, or the dependency needs an ok result and the blocker is not
/// `done` with `ok`.
macro_rules! unmet {
    ($d:literal, $b:literal) => {
        concat!(
            "(",
            $b,
            ".state NOT IN ('done', 'cancelled') OR (",
            $d,
            ".needs_ok AND (",
            $b,
            ".state <> 'done' OR ",
            $b,
            ".result IS NULL OR ",
            $b,
            ".result <> 'ok')))"
        )
    };
}

/// True for task `t` when one of its dependencies is not satisfied.
pub(crate) const OPEN_BLOCKER: &str = concat!(
    "EXISTS (SELECT 1 FROM task_deps od JOIN tasks ob ON ob.id = od.blocker_id WHERE od.task_id = t.id AND ",
    unmet!("od", "ob"),
    ")"
);

/// Refuse a write with `stale` when the task is not at the revision the
/// writer expects.
pub(crate) fn check_rev(w: &TaskWrite, t: &TaskRec) -> Result<()> {
    match w.if_rev {
        Some(r) if r != t.event_seq => Err(refused(
            "stale",
            format!(
                "t{} changed since rev {r}: it is at rev {}",
                t.num, t.event_seq
            ),
        )),
        _ => Ok(()),
    }
}

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
    steal_by_id: Option<i64>,
    steal_by: Option<String>,
    steal_at: Option<i64>,
    result: Option<String>,
    result_ref: Option<String>,
    result_note: Option<String>,
    result_by: Option<String>,
    result_at: Option<i64>,
    import_key: Option<String>,
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
            steal_requested_by_id: self.steal_by_id,
            steal_requested_by: self.steal_by,
            steal_requested_at: self.steal_at,
            progress: None,
            result: self
                .result
                .as_deref()
                .map(crate::common::parse::<TaskResult>)
                .transpose()?,
            result_ref: self.result_ref,
            result_note: self.result_note,
            result_by: self.result_by,
            result_at: self.result_at,
            import_key: self.import_key,
            waits_on_ok: Vec::new(),
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

pub(crate) const EVENT_SELECT: &str = "SELECT e.task_id, e.seq, e.at, COALESCE(s.name, e.actor_name) AS actor, e.written_by, e.kind, \
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
            + t.blocked_reason.as_deref().map_or(0, str::len)
            + t.result_ref.as_deref().map_or(0, str::len)
            + t.result_note.as_deref().map_or(0, str::len)
            + t.result_by.as_deref().map_or(0, str::len)
            + t.import_key.as_deref().map_or(0, str::len)) as i64
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
    /// Imported events: the actor's name as the file gives it.
    pub actor_name: Option<String>,
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
            + [&self.from, &self.to, &self.note, &self.actor_name]
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
            evs.push(Ev::new(kind).field(name).from(from).to(to));
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

            /// Fill in `waits_on`, `waits_on_ok` and `progress`.
            pub async fn fill(c: &mut Conn, recs: &mut [TaskRec]) -> Result<()> {
                for chunk in recs.chunks_mut(500) {
                    let sql = format!(
                        "SELECT d.task_id, b.num, d.needs_ok FROM task_deps d JOIN tasks b ON b.id = d.blocker_id \
                         WHERE d.task_id IN ({}) AND {} ORDER BY b.num",
                        placeholders(1, chunk.len()),
                        unmet!("d", "b")
                    );
                    let mut q = sqlx::query_as::<Db, (i64, i64, bool)>(&sql);
                    for t in chunk.iter() {
                        q = q.bind(t.id);
                    }
                    let rows = q.fetch_all(&mut *c).await?;
                    let ok: Vec<(i64, i64)> = rows.iter().filter(|r| r.2).map(|r| (r.0, r.1)).collect();
                    let mut m = group_waits(rows.into_iter().map(|r| (r.0, r.1)).collect());
                    let mut m_ok = group_waits(ok);
                    let sql = format!(
                        "SELECT parent_id, CAST(SUM(CASE WHEN state = 'done' THEN 1 ELSE 0 END) AS BIGINT), \
                         CAST(SUM(CASE WHEN state <> 'cancelled' THEN 1 ELSE 0 END) AS BIGINT) \
                         FROM tasks WHERE parent_id IN ({}) GROUP BY parent_id",
                        placeholders(1, chunk.len())
                    );
                    let mut q = sqlx::query_as::<Db, (i64, i64, i64)>(&sql);
                    for t in chunk.iter() {
                        q = q.bind(t.id);
                    }
                    let progress: std::collections::HashMap<i64, (i64, i64)> =
                        q.fetch_all(&mut *c).await?.into_iter().map(|(p, d, n)| (p, (d, n))).collect();
                    for t in chunk.iter_mut() {
                        t.waits_on = m.remove(&t.id).unwrap_or_default();
                        t.waits_on_ok = m_ok.remove(&t.id).unwrap_or_default();
                        t.progress = progress.get(&t.id).copied();
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

            /// Write one event on `task_id`; returns the bytes it adds.
            /// Events are never deleted: the org's storage quota bounds them.
            pub async fn event(c: &mut Conn, task_id: i64, org_id: i64, actor: Option<i64>, written_by: Option<&str>, at: i64, e: Ev) -> Result<i64> {
                let seq: i64 = sqlx::query_scalar("UPDATE tasks SET event_seq = event_seq + 1 WHERE id = $1 RETURNING event_seq")
                    .bind(task_id)
                    .fetch_one(&mut *c)
                    .await?;
                let bytes = e.bytes(written_by);
                sqlx::query(
                    "INSERT INTO task_events (task_id, seq, org_id, at, actor_id, written_by, kind, field, \
                     from_value, to_value, note, message_id, actor_name) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
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
                .bind(&e.actor_name)
                .execute(&mut *c)
                .await?;
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
                     AND ($1 <= 0 OR CAST($3 AS BIGINT) IS NULL OR stored_bytes + task_bytes + doc_bytes + $1 <= $3) RETURNING id",
                )
                .bind(delta)
                .bind(org_id)
                .bind(quota)
                .fetch_optional(&mut *c)
                .await?;
                if ok.is_none() {
                    let stored: i64 = sqlx::query_scalar("SELECT stored_bytes + task_bytes + doc_bytes FROM orgs WHERE id = $1")
                        .bind(org_id)
                        .fetch_one(&mut *c)
                        .await?;
                    return Err(StoreError::StorageQuota(stored));
                }
                Ok(())
            }

            async fn open_count(c: &mut Conn, org_id: i64) -> Result<i64> {
                Ok(sqlx::query_scalar("SELECT COUNT(*) FROM tasks WHERE org_id = $1 AND state NOT IN ('done', 'cancelled')")
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

            pub async fn create(c: &mut Conn, n: &NewTaskRec) -> Result<(TaskRec, bool)> {
                let now = now_ms();
                if let Some(k) = &n.client_key {
                    lock(c, n.org_id).await?;
                    let num: Option<i64> = sqlx::query_scalar(
                        "SELECT num FROM tasks WHERE org_id = $1 AND client_key = $2 AND created_at >= $3 ORDER BY num DESC LIMIT 1",
                    )
                    .bind(n.org_id)
                    .bind(k)
                    .bind(now - TASK_CLIENT_KEY_HOURS * 3_600_000)
                    .fetch_optional(&mut *c)
                    .await?;
                    if let Some(num) = num {
                        return Ok((must(c, n.org_id, num).await?, true));
                    }
                }
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
                     team_id, parent_id, labels, due, created_at, updated_at, client_key) \
                     VALUES ($1, $2, $3, $4, 'open', $5, $6, $7, $8, $9, $10, $11, $12, $13, $13, $14) RETURNING id",
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
                .bind(&n.client_key)
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
                Ok((must(c, n.org_id, num).await?, false))
            }

            /// The unfinished tasks that depend on task `id` (with their
            /// `waits_on`).
            async fn dependents_of(c: &mut Conn, id: i64, needs_ok_only: bool) -> Result<Vec<TaskRec>> {
                let sql = if needs_ok_only {
                    "SELECT d.task_id FROM task_deps d JOIN tasks x ON x.id = d.task_id \
                     WHERE d.blocker_id = $1 AND d.needs_ok AND x.state NOT IN ('done', 'cancelled')"
                } else {
                    "SELECT d.task_id FROM task_deps d JOIN tasks x ON x.id = d.task_id \
                     WHERE d.blocker_id = $1 AND x.state NOT IN ('done', 'cancelled')"
                };
                let ids: Vec<i64> = sqlx::query_scalar(sql).bind(id).fetch_all(&mut *c).await?;
                by_ids(c, &ids).await
            }

            /// After a write to blocker `b`: the dependents (as they were
            /// before it, in `before`) that waited and wait no longer get an
            /// `unblocked` event each; returns them as they are now.
            async fn newly_unblocked(c: &mut Conn, w: &TaskWrite, b: &TaskRec, before: &[TaskRec], now: i64, bytes: &mut i64) -> Result<Vec<TaskRec>> {
                let ids: Vec<i64> = before.iter().filter(|d| !d.waits_on.is_empty()).map(|d| d.id).collect();
                let mut out = Vec::new();
                for d in by_ids(c, &ids).await? {
                    if d.waits_on.is_empty() && !d.state.is_closed() {
                        let note = match b.result {
                            Some(r) if b.state == TaskState::Done => format!("t{} done with result {r}", b.num),
                            _ => format!("t{} {}", b.num, b.state),
                        };
                        let e = Ev::new(TaskEventKind::Unblocked).to(Some(tnum(b.num))).note(note);
                        *bytes += event(c, d.id, w.org_id, w.actor_id, w.written_by.as_deref(), now, e).await?;
                        out.push(d);
                    }
                }
                Ok(out)
            }

            /// The name of seat `id`.
            async fn seat_name(c: &mut Conn, id: Option<i64>) -> Result<Option<String>> {
                let Some(id) = id else { return Ok(None) };
                Ok(sqlx::query_scalar("SELECT name FROM seats WHERE id = $1").bind(id).fetch_optional(&mut *c).await?)
            }

            pub async fn change(c: &mut Conn, w: &TaskWrite, ch: &TaskChange) -> Result<TaskChanged> {
                lock(c, w.org_id).await?;
                let now = now_ms();
                let before = must(c, w.org_id, w.num).await?;
                check_rev(w, &before)?;
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
                // A parent closes when its children are closed, or by force.
                let mut close_note = None;
                if t.state.is_closed() && !before.state.is_closed() {
                    let open: Vec<i64> = sqlx::query_scalar(
                        "SELECT num FROM tasks WHERE parent_id = $1 AND state NOT IN ('done', 'cancelled') ORDER BY num LIMIT 21",
                    )
                    .bind(before.id)
                    .fetch_all(&mut *c)
                    .await?;
                    if !open.is_empty() {
                        let mut names: Vec<String> = open.iter().take(20).map(|n| tnum(*n)).collect();
                        if open.len() > 20 {
                            names.push("...".into());
                        }
                        if !ch.force {
                            return Err(refused(
                                "children_open",
                                format!(
                                    "t{} has child tasks that are not closed: {}; close them first, or force the close",
                                    before.num,
                                    names.join(", ")
                                ),
                            ));
                        }
                        close_note = Some(format!("closed with open child tasks: {}", names.join(", ")));
                    }
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
                // The result: set on a `done` task, withdrawn, or cleared
                // when the task is reopened.
                let withdrawn = matches!(ch.result, Some(None)) && before.result.is_some();
                match ch.result {
                    Some(Some(r)) => {
                        if t.state != TaskState::Done {
                            return Err(refused(
                                "not_done",
                                format!("t{} is {}: a result goes with the state done", before.num, t.state),
                            ));
                        }
                        let same = before.result == Some(r) && before.result_ref == ch.result_ref && before.result_note == ch.result_note;
                        if !same {
                            t.result = Some(r);
                            t.result_ref = ch.result_ref.clone();
                            t.result_note = ch.result_note.clone();
                            t.result_by = seat_name(c, w.actor_id).await?;
                            t.result_at = Some(now);
                        }
                    }
                    Some(None) => {}
                    None if ch.result_ref.is_some() || ch.result_note.is_some() => {
                        return Err(StoreError::Invalid("result_ref and result_note go with result".into()));
                    }
                    None => {}
                }
                let reopened = before.state == TaskState::Done && t.state != TaskState::Done;
                if withdrawn || reopened {
                    t.result = None;
                    t.result_ref = None;
                    t.result_note = None;
                    t.result_by = None;
                    t.result_at = None;
                }
                // A pending steal request ends when the assignee or the state changes.
                let clear_steal = before.steal_requested_by_id.is_some()
                    && (t.assignee_id != before.assignee_id || t.state != before.state);
                // Dependents whose readiness this write may change.
                let watch = if t.state != before.state || t.result != before.result {
                    dependents_of(c, before.id, false).await?
                } else {
                    Vec::new()
                };
                sqlx::query(
                    "UPDATE tasks SET title = $2, body = $3, state = $4, priority = $5, rank = $6, assignee_id = $7, \
                     team_id = $8, parent_id = $9, labels = $10, due = $11, waiting_on_id = $12, blocked_reason = $13, \
                     closed_at = $14, due_reminded_at = CASE WHEN $15 THEN NULL ELSE due_reminded_at END, \
                     steal_requested_by = CASE WHEN $16 THEN NULL ELSE steal_requested_by END, \
                     steal_requested_at = CASE WHEN $16 THEN NULL ELSE steal_requested_at END, \
                     result = $17, result_ref = $18, result_note = $19, result_by = $20, result_at = $21 WHERE id = $1",
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
                .bind(clear_steal)
                .bind(t.result.map(|r| r.as_str()))
                .bind(&t.result_ref)
                .bind(&t.result_note)
                .bind(&t.result_by)
                .bind(t.result_at)
                .execute(&mut *c)
                .await?;
                let after = must(c, w.org_id, w.num).await?;
                let (mut fields, mut evs) = field_events(&before, &after);
                if let Some(n) = close_note
                    && let Some(e) = evs.iter_mut().find(|e| e.field == Some("state"))
                {
                    e.note = Some(n);
                }
                let mut bytes = task_bytes(&after) - task_bytes(&before);
                let wb = w.written_by.as_deref();
                for e in evs {
                    bytes += event(c, before.id, w.org_id, w.actor_id, wb, now, e).await?;
                }
                // Result events.
                let shown = |r: Option<amail_proto::TaskResult>| r.map(|r| r.to_string());
                if withdrawn {
                    let mut old = before.result.map(|r| r.to_string()).unwrap_or_default();
                    if let Some(x) = &before.result_ref {
                        old = format!("{old} {x}");
                    }
                    let mut e = Ev::new(TaskEventKind::ResultWithdrawn).field("result").from(Some(old));
                    e.note = ch.note.clone();
                    bytes += event(c, before.id, w.org_id, w.actor_id, wb, now, e).await?;
                    fields.push("result");
                } else if after.result != before.result || after.result_note != before.result_note {
                    let mut e = Ev::new(TaskEventKind::Changed).field("result").from(shown(before.result)).to(shown(after.result));
                    e.note = after.result_note.clone();
                    bytes += event(c, before.id, w.org_id, w.actor_id, wb, now, e).await?;
                    fields.push("result");
                }
                if !withdrawn && after.result_ref != before.result_ref {
                    let e = Ev::new(TaskEventKind::Changed)
                        .field("result_ref")
                        .from(before.result_ref.clone())
                        .to(after.result_ref.clone());
                    bytes += event(c, before.id, w.org_id, w.actor_id, wb, now, e).await?;
                    fields.push("result_ref");
                }
                if clear_steal {
                    let why = if after.assignee_id != before.assignee_id {
                        "cleared: the task was reassigned"
                    } else {
                        "cleared: the task's state changed"
                    };
                    let e = Ev::new(TaskEventKind::StealDeclined)
                        .from(before.assignee.clone())
                        .to(before.steal_requested_by.clone())
                        .note(why);
                    bytes += event(c, before.id, w.org_id, w.actor_id, wb, now, e).await?;
                    fields.push("steal");
                }
                let unblocked = newly_unblocked(c, w, &after, &watch, now, &mut bytes).await?;
                let dependents = if after.state.is_closed() && !before.state.is_closed() {
                    let ids: Vec<i64> = watch.iter().map(|d| d.id).collect();
                    by_ids(c, &ids).await?
                } else {
                    Vec::new()
                };
                // Seats whose dependency needs an ok result of this task.
                let rejected = after.state == TaskState::Done
                    && after.result == Some(amail_proto::TaskResult::Rejected)
                    && before.result != after.result;
                let ok_withdrawn = withdrawn && before.result == Some(amail_proto::TaskResult::Ok);
                let needs_ok_waiting = if rejected || ok_withdrawn {
                    dependents_of(c, before.id, true).await?
                } else {
                    Vec::new()
                };
                // The last open child of a parent closed.
                let mut children_done = None;
                if after.state.is_closed() && !before.state.is_closed()
                    && let Some(pid) = after.parent_id
                {
                    let (open, all): (i64, i64) = sqlx::query_as(
                        "SELECT CAST(COALESCE(SUM(CASE WHEN state NOT IN ('done', 'cancelled') THEN 1 ELSE 0 END), 0) AS BIGINT), \
                         COUNT(*) FROM tasks WHERE parent_id = $1",
                    )
                    .bind(pid)
                    .fetch_one(&mut *c)
                    .await?;
                    if open == 0
                        && let Some(p) = by_ids(c, &[pid]).await?.pop()
                        && !p.state.is_closed()
                    {
                        children_done = Some((p, all));
                    }
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
                    children_done,
                    needs_ok_waiting,
                })
            }

            fn unchanged(t: TaskRec) -> TaskChanged {
                TaskChanged {
                    before: t.clone(),
                    task: t,
                    fields: Vec::new(),
                    unblocked: Vec::new(),
                    dependents: Vec::new(),
                    children_done: None,
                    needs_ok_waiting: Vec::new(),
                }
            }

            /// A request of the actor to take over the task (see
            /// [`crate::Store::steal_task`]).
            pub async fn steal(c: &mut Conn, w: &TaskWrite) -> Result<TaskChanged> {
                lock(c, w.org_id).await?;
                let now = now_ms();
                let before = must(c, w.org_id, w.num).await?;
                let Some(actor) = w.actor_id else {
                    return Err(StoreError::Invalid("steal needs a seat".into()));
                };
                let Some(owner) = before.assignee_id else {
                    return Err(refused(
                        "not_assigned",
                        format!("t{} has no assignee: take it instead (amail task take t{})", before.num, before.num),
                    ));
                };
                if owner == actor {
                    return Err(refused("own_task", format!("t{} is assigned to you", before.num)));
                }
                if before.state != TaskState::Open {
                    return Err(refused(
                        "started",
                        format!("t{} is {}: only a task that is not started can be taken over", before.num, before.state),
                    ));
                }
                match before.steal_requested_by_id {
                    Some(s) if s == actor => return Ok(unchanged(before)),
                    Some(_) => {
                        return Err(refused(
                            "steal_pending",
                            format!(
                                "{} already asked to take over t{}; wait until {} answers or the request expires",
                                before.steal_requested_by.clone().unwrap_or_default(),
                                before.num,
                                before.assignee.clone().unwrap_or_default()
                            ),
                        ));
                    }
                    None => {}
                }
                sqlx::query("UPDATE tasks SET steal_requested_by = $2, steal_requested_at = $3 WHERE id = $1")
                    .bind(before.id)
                    .bind(actor)
                    .bind(now)
                    .execute(&mut *c)
                    .await?;
                let task = must(c, w.org_id, w.num).await?;
                let e = Ev::new(TaskEventKind::StealRequested)
                    .from(before.assignee.clone())
                    .to(task.steal_requested_by.clone());
                let bytes = event(c, before.id, w.org_id, w.actor_id, w.written_by.as_deref(), now, e).await?;
                charge(c, w.org_id, bytes, w.quota).await?;
                let task = must(c, w.org_id, w.num).await?;
                Ok(TaskChanged {
                    before,
                    task,
                    fields: vec!["steal".into()],
                    unblocked: Vec::new(),
                    dependents: Vec::new(),
                    children_done: None,
                    needs_ok_waiting: Vec::new(),
                })
            }

            /// Answer a pending steal request: `give` hands the task to the
            /// requester, otherwise the assignee keeps it. Only the assignee
            /// answers, or the org's user (`as_user`).
            pub async fn answer_steal(c: &mut Conn, w: &TaskWrite, give: bool, as_user: bool) -> Result<TaskChanged> {
                lock(c, w.org_id).await?;
                let now = now_ms();
                let before = must(c, w.org_id, w.num).await?;
                let (Some(by), Some(by_name)) = (before.steal_requested_by_id, before.steal_requested_by.clone()) else {
                    return Err(StoreError::NotFound(format!("t{} has no pending request to take it over", before.num)));
                };
                if !as_user && (w.actor_id.is_none() || before.assignee_id != w.actor_id) {
                    return Err(refused(
                        "not_assignee",
                        format!("only the assignee of t{} or the org's user answers the request", before.num),
                    ));
                }
                let wb = w.written_by.as_deref();
                let mut bytes = 0;
                if give {
                    sqlx::query(
                        "UPDATE tasks SET assignee_id = $2, steal_requested_by = NULL, steal_requested_at = NULL, \
                         updated_at = $3 WHERE id = $1",
                    )
                    .bind(before.id)
                    .bind(by)
                    .bind(now)
                    .execute(&mut *c)
                    .await?;
                    let e = Ev::new(TaskEventKind::Assigned)
                        .field("assignee")
                        .from(before.assignee.clone())
                        .to(Some(by_name.clone()))
                        .note(format!("yielded to {by_name} on request"));
                    bytes += event(c, before.id, w.org_id, w.actor_id, wb, now, e).await?;
                } else {
                    sqlx::query("UPDATE tasks SET steal_requested_by = NULL, steal_requested_at = NULL WHERE id = $1")
                        .bind(before.id)
                        .execute(&mut *c)
                        .await?;
                    let e = Ev::new(TaskEventKind::StealDeclined)
                        .from(before.assignee.clone())
                        .to(Some(by_name.clone()));
                    bytes += event(c, before.id, w.org_id, w.actor_id, wb, now, e).await?;
                }
                charge(c, w.org_id, bytes, None).await?;
                let task = must(c, w.org_id, w.num).await?;
                Ok(TaskChanged {
                    before,
                    task,
                    fields: vec![if give { "assignee" } else { "steal" }.into()],
                    unblocked: Vec::new(),
                    dependents: Vec::new(),
                    children_done: None,
                    needs_ok_waiting: Vec::new(),
                })
            }

            /// End the steal requests made before `cutoff` (each once,
            /// also with several instances), with a `steal_expired` event.
            pub async fn expire_steals(c: &mut Conn, cutoff: i64, limit: i64) -> Result<Vec<StealExpiredRec>> {
                let rows: Vec<(i64, i64, i64, i64)> = sqlx::query_as(
                    "SELECT t.id, t.org_id, t.num, t.steal_requested_by FROM tasks t \
                     WHERE t.steal_requested_at IS NOT NULL AND t.steal_requested_by IS NOT NULL \
                     AND t.steal_requested_at <= $1 \
                     ORDER BY t.steal_requested_at LIMIT $2",
                )
                .bind(cutoff)
                .bind(limit)
                .fetch_all(&mut *c)
                .await?;
                let now = now_ms();
                let mut out = Vec::new();
                for (id, org_id, num, by) in rows {
                    let before = must(c, org_id, num).await?;
                    let n = sqlx::query(
                        "UPDATE tasks SET steal_requested_by = NULL, steal_requested_at = NULL \
                         WHERE id = $1 AND steal_requested_by = $2 AND steal_requested_at <= $3",
                    )
                    .bind(id)
                    .bind(by)
                    .bind(cutoff)
                    .execute(&mut *c)
                    .await?
                    .rows_affected();
                    if n == 0 {
                        continue;
                    }
                    let requester = before.steal_requested_by.clone().unwrap_or_default();
                    let e = Ev::new(TaskEventKind::StealExpired)
                        .from(before.assignee.clone())
                        .to(Some(requester.clone()));
                    let bytes = event(c, id, org_id, None, None, now, e).await?;
                    charge(c, org_id, bytes, None).await?;
                    out.push(StealExpiredRec { task: must(c, org_id, num).await?, requester_id: by, requester });
                }
                Ok(out)
            }

            pub async fn take(c: &mut Conn, w: &TaskWrite) -> Result<TaskChanged> {
                lock(c, w.org_id).await?;
                let now = now_ms();
                let before = must(c, w.org_id, w.num).await?;
                check_rev(w, &before)?;
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
                    children_done: None,
                    needs_ok_waiting: Vec::new(),
                })
            }

            pub async fn add_dep(c: &mut Conn, w: &TaskWrite, blocker: i64, needs_ok: Option<bool>) -> Result<TaskChanged> {
                lock(c, w.org_id).await?;
                let now = now_ms();
                let before = must(c, w.org_id, w.num).await?;
                check_rev(w, &before)?;
                let b = must(c, w.org_id, blocker).await?;
                if b.id == before.id {
                    return Err(refused("dependency_cycle", "a task cannot depend on itself"));
                }
                let wb = w.written_by.as_deref();
                let exists: Option<bool> = sqlx::query_scalar("SELECT needs_ok FROM task_deps WHERE task_id = $1 AND blocker_id = $2")
                    .bind(before.id)
                    .bind(b.id)
                    .fetch_optional(&mut *c)
                    .await?;
                if let Some(cur) = exists {
                    // Repeating the dependency changes whether it needs an ok result.
                    let Some(want) = needs_ok.filter(|v| *v != cur) else {
                        return Ok(unchanged(before));
                    };
                    sqlx::query("UPDATE task_deps SET needs_ok = $3 WHERE task_id = $1 AND blocker_id = $2")
                        .bind(before.id)
                        .bind(b.id)
                        .bind(want)
                        .execute(&mut *c)
                        .await?;
                    let e = Ev::new(TaskEventKind::Changed)
                        .field("needs_ok")
                        .from(Some(cur.to_string()))
                        .to(Some(want.to_string()))
                        .note(format!("dependency on t{}", b.num));
                    let mut bytes = event(c, before.id, w.org_id, w.actor_id, wb, now, e).await?;
                    touch(c, &[before.id], now).await?;
                    let task = must(c, w.org_id, w.num).await?;
                    let mut unblocked = Vec::new();
                    if !before.waits_on.is_empty() && task.waits_on.is_empty() && !task.state.is_closed() {
                        let e = Ev::new(TaskEventKind::Unblocked)
                            .to(Some(tnum(b.num)))
                            .note(format!("t{} {}: any close satisfies the dependency now", b.num, b.state));
                        bytes += event(c, before.id, w.org_id, w.actor_id, wb, now, e).await?;
                        unblocked.push(task);
                    }
                    charge(c, w.org_id, bytes, w.quota).await?;
                    let task = must(c, w.org_id, w.num).await?;
                    return Ok(TaskChanged {
                        before,
                        task,
                        fields: vec!["blocked_by".into()],
                        unblocked,
                        dependents: Vec::new(),
                        children_done: None,
                        needs_ok_waiting: Vec::new(),
                    });
                }
                let needs_ok = needs_ok.unwrap_or(false);
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
                sqlx::query("INSERT INTO task_deps (task_id, blocker_id, org_id, created_at, needs_ok) VALUES ($1, $2, $3, $4, $5)")
                    .bind(before.id)
                    .bind(b.id)
                    .bind(w.org_id)
                    .bind(now)
                    .bind(needs_ok)
                    .execute(&mut *c)
                    .await?;
                let mut bytes = STORED_BYTES_PER_TASK_EVENT;
                let mut e = Ev::new(TaskEventKind::DependencyAdded).field("blocked_by").to(Some(tnum(b.num)));
                if needs_ok {
                    e = e.note("ok result needed");
                }
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
                    children_done: None,
                    needs_ok_waiting: Vec::new(),
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
                check_rev(w, &before)?;
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
                    children_done: None,
                    needs_ok_waiting: Vec::new(),
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
                check_rev(w, &t)?;
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
                check_rev(w, &t)?;
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
                    "steal" => "steal",
                    "steal_declined" => "steal_declined",
                    "steal_expired" => "steal_expired",
                    "children_done" => "children_done",
                    "blocker_rejected" => "blocker_rejected",
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

            /// The seats that commented on the task, in the order of their
            /// first comment.
            pub async fn commenters(c: &mut Conn, org_id: i64, num: i64) -> Result<Vec<String>> {
                let rows: Vec<(String, i64)> = sqlx::query_as(
                    "SELECT s.name, MIN(e.seq) AS first FROM task_events e JOIN tasks t ON t.id = e.task_id \
                     JOIN seats s ON s.id = e.actor_id WHERE t.org_id = $1 AND t.num = $2 AND e.kind = 'commented' \
                     GROUP BY s.name ORDER BY first LIMIT 500",
                )
                .bind(org_id)
                .bind(num)
                .fetch_all(&mut *c)
                .await?;
                Ok(rows.into_iter().map(|r| r.0).collect())
            }

            pub async fn detail(c: &mut Conn, org_id: i64, num: i64, nevents: i64) -> Result<Option<TaskDetailRec>> {
                let Some(task) = by_num(c, org_id, num).await? else { return Ok(None) };
                let deps: Vec<(i64, bool)> = sqlx::query_as("SELECT blocker_id, needs_ok FROM task_deps WHERE task_id = $1")
                    .bind(task.id)
                    .fetch_all(&mut *c)
                    .await?;
                let ids_b: Vec<i64> = deps.iter().map(|d| d.0).collect();
                let blockers = by_ids(c, &ids_b).await?;
                let blockers_needs_ok = blockers
                    .iter()
                    .filter(|b| deps.iter().any(|d| d.0 == b.id && d.1))
                    .map(|b| b.num)
                    .collect();
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
                    blockers_needs_ok,
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
                match q.result {
                    Some(Some(r)) => {
                        b.push(" AND t.result = ").push_bind(r.as_str());
                    }
                    Some(None) => {
                        b.push(" AND t.result IS NULL");
                    }
                    None => {}
                }
                if let Some(k) = &q.import_key {
                    b.push(" AND t.import_key = ").push_bind(k.clone());
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
                let pairs: Vec<(i64, i64)> = sqlx::query_as(concat!("SELECT d.task_id, d.blocker_id FROM task_deps d JOIN tasks t ON t.id = d.task_id \
                     JOIN tasks b ON b.id = d.blocker_id WHERE t.org_id = $1 AND t.assignee_id = $2 \
                     AND t.state NOT IN ('done', 'cancelled') AND ", unmet!("d", "b"), " \
                     ORDER BY t.num, b.num LIMIT $3"))
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
                let pairs: Vec<(i64, i64)> = sqlx::query_as(concat!("SELECT d.blocker_id, d.task_id FROM task_deps d JOIN tasks b ON b.id = d.blocker_id \
                     JOIN tasks x ON x.id = d.task_id WHERE b.org_id = $1 \
                     AND COALESCE(b.assignee_id, b.reporter_id) = $2 \
                     AND ", unmet!("d", "b"), " AND x.state NOT IN ('done', 'cancelled') \
                     AND COALESCE(x.assignee_id, x.reporter_id) <> $2 \
                     AND NOT EXISTS (SELECT 1 FROM task_deps d2 JOIN tasks b2 ON b2.id = d2.blocker_id \
                       WHERE d2.task_id = x.id AND d2.blocker_id <> b.id AND ", unmet!("d2", "b2"), ") \
                     ORDER BY b.num, x.num LIMIT $3"))
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

            /// The task imported from record `key`.
            async fn by_import_key(c: &mut Conn, org_id: i64, key: &str) -> Result<Option<TaskRec>> {
                let num: Option<i64> = sqlx::query_scalar("SELECT num FROM tasks WHERE org_id = $1 AND import_key = $2")
                    .bind(org_id)
                    .bind(key)
                    .fetch_optional(&mut *c)
                    .await?;
                match num {
                    Some(n) => by_num(c, org_id, n).await,
                    None => Ok(None),
                }
            }

            /// Bring task `num` to the state, fields and result of record
            /// `r` (fields `r` does not give are kept); true when something
            /// changed.
            async fn import_fields(c: &mut Conn, w: &TaskWrite, r: &ImportTaskRec, warnings: &mut Vec<String>) -> Result<bool> {
                let before = must(c, w.org_id, w.num).await?;
                let mut ch = TaskChange { force: true, ..Default::default() };
                if before.title != r.title {
                    ch.title = Some(r.title.clone());
                }
                if let Some(b) = r.body.as_ref().filter(|b| **b != before.body) {
                    ch.body = Some(b.clone());
                }
                if let Some(p) = r.priority.filter(|p| *p != before.priority) {
                    ch.priority = Some(p);
                }
                if let Some(a) = r.assignee_id.filter(|a| *a != before.assignee_id) {
                    ch.assignee_id = Some(a);
                }
                if let Some(l) = r.labels.as_ref().filter(|l| **l != before.labels) {
                    ch.labels = Some(l.clone());
                }
                if let Some(d) = r.due.filter(|d| *d != before.due) {
                    ch.due = Some(d);
                }
                let state = r.state.filter(|s| *s != before.state);
                let done = r.state.unwrap_or(before.state) == TaskState::Done;
                let mut result_set = false;
                match &r.result {
                    Some(x) if done => {
                        if before.result != Some(x.result) || before.result_ref != x.result_ref || before.result_note != x.note {
                            ch.result = Some(Some(x.result));
                            ch.result_ref = x.result_ref.clone();
                            ch.result_note = x.note.clone();
                            result_set = true;
                        }
                    }
                    Some(_) => warnings.push(format!("{}: the result is ignored: the task is not done", r.key)),
                    None => {}
                }
                // A closed task changes to another closed state through `open`.
                if let Some(s) = state {
                    if before.state.is_closed() && s.is_closed() {
                        let reopen = TaskChange { state: Some(TaskState::Open), ..Default::default() };
                        change(c, w, &reopen).await?;
                    }
                    ch.state = Some(s);
                }
                let changed = change(c, w, &ch).await?;
                if result_set && let Some(x) = &r.result {
                    let by = match &x.by {
                        Some(b) => Some(b.clone()),
                        None => seat_name(c, w.actor_id).await?,
                    };
                    sqlx::query("UPDATE tasks SET result_by = $2, result_at = $3 WHERE id = $1")
                        .bind(before.id)
                        .bind(by)
                        .bind(x.at.unwrap_or_else(now_ms))
                        .execute(&mut *c)
                        .await?;
                }
                Ok(!changed.fields.is_empty() || state.is_some())
            }

            /// Add the history entries of record `r` that task `id` does not
            /// have yet (as `imported` events); true when it added one.
            async fn import_history(c: &mut Conn, w: &TaskWrite, id: i64, r: &ImportTaskRec) -> Result<bool> {
                let have: Vec<(i64, Option<String>, Option<String>)> = sqlx::query_as(
                    "SELECT at, actor_name, note FROM task_events WHERE task_id = $1 AND kind = 'imported'",
                )
                .bind(id)
                .fetch_all(&mut *c)
                .await?;
                let mut bytes = 0;
                let mut added = false;
                for h in &r.history {
                    if have.iter().any(|x| x.0 == h.at && x.1 == h.by && x.2.as_deref() == Some(h.text.as_str())) {
                        continue;
                    }
                    let mut e = Ev::new(TaskEventKind::Imported).note(h.text.clone());
                    e.actor_name = h.by.clone();
                    bytes += event(c, id, w.org_id, None, None, h.at, e).await?;
                    added = true;
                }
                charge(c, w.org_id, bytes, w.quota).await?;
                Ok(added)
            }

            pub async fn import(c: &mut Conn, imp: &TaskImportRec) -> Result<TaskImportRes> {
                lock(c, imp.org_id).await?;
                let mut res = TaskImportRes::default();
                let mut nums: Vec<i64> = Vec::new();
                let w = |num: i64| TaskWrite {
                    org_id: imp.org_id,
                    num,
                    actor_id: Some(imp.actor_id),
                    written_by: None,
                    quota: imp.quota,
                    if_rev: None,
                };
                // Pass 1: the tasks.
                for r in &imp.records {
                    let existing = by_import_key(c, imp.org_id, &r.key).await?;
                    let (num, action) = match existing {
                        Some(t) => {
                            let changed = import_fields(c, &w(t.num), r, &mut res.warnings).await?;
                            let added = import_history(c, &w(t.num), t.id, r).await?;
                            (t.num, if changed || added { "updated" } else { "same" })
                        }
                        None => {
                            let (t, _) = create(
                                c,
                                &NewTaskRec {
                                    org_id: imp.org_id,
                                    reporter_id: r.reporter_id.unwrap_or(imp.actor_id),
                                    written_by: None,
                                    title: r.title.clone(),
                                    body: r.body.clone().unwrap_or_default(),
                                    priority: r.priority,
                                    assignee_id: r.assignee_id.flatten(),
                                    team_id: None,
                                    parent: None,
                                    labels: r.labels.clone().unwrap_or_default(),
                                    due: r.due.flatten(),
                                    link_thread: None,
                                    quota: imp.quota,
                                    client_key: None,
                                },
                            )
                            .await?;
                            let key_bytes = r.key.len() as i64;
                            sqlx::query("UPDATE tasks SET import_key = $2, created_at = COALESCE($3, created_at) WHERE id = $1")
                                .bind(t.id)
                                .bind(&r.key)
                                .bind(r.created_at)
                                .execute(&mut *c)
                                .await?;
                            charge(c, imp.org_id, key_bytes, imp.quota).await?;
                            import_fields(c, &w(t.num), r, &mut res.warnings).await?;
                            import_history(c, &w(t.num), t.id, r).await?;
                            (t.num, "created")
                        }
                    };
                    // The record's time, and no stale reminder before one stale
                    // period after the import (nothing is mailed for an import).
                    if let Some(u) = r.updated_at.filter(|_| action != "same") {
                        sqlx::query("UPDATE tasks SET updated_at = $3, stale_reminded_at = $4 WHERE org_id = $1 AND num = $2")
                            .bind(imp.org_id)
                            .bind(num)
                            .bind(u)
                            .bind(now_ms())
                            .execute(&mut *c)
                            .await?;
                    }
                    nums.push(num);
                    res.results.push((r.key.clone(), Some(num), action));
                }
                // Pass 2: parents and dependencies, between tasks of the file
                // or tasks imported before.
                for (i, r) in imp.records.iter().enumerate() {
                    let num = nums[i];
                    let mut changed = false;
                    if let Some(pk) = &r.parent {
                        match by_import_key(c, imp.org_id, pk).await? {
                            None => res.warnings.push(format!("{}: no task imported as {pk:?}; the parent is not set", r.key)),
                            Some(p) => {
                                let ch = TaskChange { parent: Some(Some(p.num)), ..Default::default() };
                                let cur = must(c, imp.org_id, num).await?;
                                if cur.parent != Some(p.num) {
                                    match change(c, &w(num), &ch).await {
                                        Ok(_) => changed = true,
                                        Err(StoreError::Refused(_, m)) => res.warnings.push(format!("{}: parent {pk}: {m}", r.key)),
                                        Err(e) => return Err(e),
                                    }
                                }
                            }
                        }
                    }
                    for (bk, needs_ok) in &r.deps {
                        match by_import_key(c, imp.org_id, bk).await? {
                            None => res.warnings.push(format!("{}: no task imported as {bk:?}; the dependency is not set", r.key)),
                            Some(b) => match add_dep(c, &w(num), b.num, Some(*needs_ok)).await {
                                Ok(ch) => changed |= !ch.fields.is_empty(),
                                Err(StoreError::Refused(_, m)) => res.warnings.push(format!("{}: dependency on {bk}: {m}", r.key)),
                                Err(e) => return Err(e),
                            },
                        }
                    }
                    if changed && res.results[i].2 == "same" {
                        res.results[i].2 = "updated";
                    }
                }
                if imp.dry_run {
                    for x in res.results.iter_mut().filter(|x| x.2 == "created") {
                        x.1 = None;
                    }
                    return Err(StoreError::ImportDryRun(Box::new(res)));
                }
                Ok(res)
            }

            /// One section of the report: tasks matching `cond`, by
            /// `order`, with their count.
            async fn report_section(c: &mut Conn, org_id: i64, team: Option<i64>, cond: &str, order: &str, limit: i64) -> Result<(i64, Vec<TaskRec>)> {
                let mut b = sqlx::QueryBuilder::<Db>::new("SELECT COUNT(*) FROM tasks t WHERE t.org_id = ");
                b.push_bind(org_id);
                if let Some(team) = team {
                    team_clause(&mut b, team);
                }
                b.push(format!(" AND {cond}"));
                let count: i64 = b.build_query_scalar().fetch_one(&mut *c).await?;
                if count == 0 {
                    return Ok((0, Vec::new()));
                }
                let mut b = sqlx::QueryBuilder::<Db>::new(concat!(task_select!("''"), " WHERE t.org_id = "));
                b.push_bind(org_id);
                if let Some(team) = team {
                    team_clause(&mut b, team);
                }
                b.push(format!(" AND {cond} ORDER BY {order} LIMIT "));
                b.push_bind(limit);
                let rows: Vec<TaskRow> = b.build_query_as().fetch_all(&mut *c).await?;
                let mut tasks = rows.into_iter().map(TaskRow::into_rec).collect::<Result<Vec<_>>>()?;
                fill(c, &mut tasks).await?;
                Ok((count, tasks))
            }

            pub async fn report(c: &mut Conn, org_id: i64, team: Option<i64>, limit: i64) -> Result<TaskReportRec> {
                let limit = limit.clamp(1, MAX_TASK_PAGE);
                let by_prio = format!("{PRIORITY_KEY}, t.rank, t.num");
                let mut out = TaskReportRec::default();
                let item = |t: TaskRec, seat: Option<String>| ReportItemRec { task: t, seat, waiting_for: Vec::new(), dependents: Vec::new() };

                let (n, v) = report_section(c, org_id, team, "t.state = 'open' AND t.assignee_id IS NOT NULL", &format!("asg.name, {by_prio}"), limit).await?;
                out.sections.push(("not_started", n, v.into_iter().map(|t| { let s = t.assignee.clone(); item(t, s) }).collect()));

                let (n, v) = report_section(c, org_id, team, "t.state = 'doing'", &format!("asg.name, {by_prio}"), limit).await?;
                out.sections.push(("in_progress", n, v.into_iter().map(|t| { let s = t.assignee.clone(); item(t, s) }).collect()));

                let (n, v) = report_section(c, org_id, team, "t.state = 'review'", &format!("rs.name, {by_prio}"), limit).await?;
                out.sections.push(("review", n, v.into_iter().map(|t| { let s = Some(t.reporter.clone()); item(t, s) }).collect()));

                let (n, v) = report_section(c, org_id, team, "t.state = 'done' AND t.result = 'rejected'", "t.closed_at DESC, t.num DESC", limit).await?;
                out.sections.push(("rejected", n, v.into_iter().map(|t| { let s = t.assignee.clone(); item(t, s) }).collect()));

                let cond = "t.state = 'done' AND t.result = 'ok' AND EXISTS (SELECT 1 FROM task_deps rd JOIN tasks rx ON rx.id = rd.task_id \
                            WHERE rd.blocker_id = t.id AND rx.state NOT IN ('done', 'cancelled'))";
                let (n, v) = report_section(c, org_id, team, cond, "t.closed_at DESC, t.num DESC", limit).await?;
                let mut items = Vec::new();
                for t in v {
                    let deps = dependents_of(c, t.id, false).await?;
                    let s = t.assignee.clone();
                    items.push(ReportItemRec { dependents: deps, ..item(t, s) });
                }
                out.sections.push(("approved", n, items));

                let cond = "t.state NOT IN ('done', 'cancelled') AND EXISTS (SELECT 1 FROM tasks rc WHERE rc.parent_id = t.id \
                            AND rc.state NOT IN ('done', 'cancelled'))";
                let (n, v) = report_section(c, org_id, team, cond, &by_prio, limit).await?;
                out.sections.push(("parents", n, v.into_iter().map(|t| { let s = t.assignee.clone(); item(t, s) }).collect()));

                let cond = format!("(t.state = 'blocked' OR (t.state = 'open' AND {OPEN_BLOCKER}))");
                let (n, v) = report_section(c, org_id, team, &cond, &by_prio, limit).await?;
                let mut items = Vec::new();
                for t in v {
                    let mut waiting_for: Vec<String> = t.waiting_on.iter().cloned().collect();
                    let ids: Vec<i64> = sqlx::query_scalar(concat!(
                        "SELECT b.id FROM task_deps d JOIN tasks b ON b.id = d.blocker_id WHERE d.task_id = $1 AND ",
                        unmet!("d", "b")
                    ))
                    .bind(t.id)
                    .fetch_all(&mut *c)
                    .await?;
                    for b in by_ids(c, &ids).await? {
                        let name = b.responsible().1.to_string();
                        if !waiting_for.contains(&name) {
                            waiting_for.push(name);
                        }
                    }
                    let s = t.assignee.clone();
                    items.push(ReportItemRec { waiting_for, ..item(t, s) });
                }
                out.sections.push(("blocked", n, items));
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
                let ids: Vec<i64> = sqlx::query_scalar("UPDATE tasks SET due_reminded_at = $1 WHERE id IN (SELECT id FROM tasks \
                     WHERE due IS NOT NULL AND due_reminded_at IS NULL AND due <= $1 \
                     AND state NOT IN ('done', 'cancelled') ORDER BY due LIMIT $2) \
                     AND due_reminded_at IS NULL RETURNING id")
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
