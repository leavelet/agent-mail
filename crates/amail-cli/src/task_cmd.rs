//! `amail task …`: the org's shared task board (`/v1/tasks…`).
//!
//! Every command prints the server's JSON unchanged with `--json`, and
//! otherwise one line per task (see [`task_line`]). Titles, labels, seat
//! names and reasons are user text: every one of them goes through
//! [`l`] or [`text::subject`].

use crate::Out;
use crate::client::{Client, enc, is_not_found};
use crate::output::{l, short_time};
use amail_proto::{self as p, TaskState, text};
use anyhow::{Context, Result, bail};
use reqwest::Method;
use serde_json::{Map, Value, json};

#[derive(clap::Subcommand)]
pub enum TaskCmd {
    /// Create a task. With --from, the message's subject is the default
    /// title and its thread is linked.
    New {
        /// One line (required unless --from is given).
        title: Option<String>,
        /// The description.
        #[arg(short = 'm', long)]
        body: Option<String>,
        /// urgent, high, normal (default) or low.
        #[arg(long)]
        priority: Option<String>,
        #[arg(long, value_name = "SEAT")]
        assignee: Option<String>,
        /// Default: the assignee's team, else yours.
        #[arg(long)]
        team: Option<String>,
        /// The parent task (t3 or 3).
        #[arg(long, value_name = "TASK")]
        parent: Option<String>,
        /// A label (repeat for more).
        #[arg(long)]
        label: Vec<String>,
        /// Due time: RFC 3339, `2026-10-30T12:00`, or `2026-10-30` (UTC).
        #[arg(long)]
        due: Option<String>,
        /// A message id: its subject is the default title, its thread is
        /// linked.
        #[arg(long = "from", value_name = "MESSAGE")]
        from: Option<i64>,
    },
    /// A task: its fields, blockers, blocked tasks, children, linked
    /// threads and last events.
    Show { task: String },
    /// The events of a task (the last 50; every event with --all).
    History {
        task: String,
        #[arg(long)]
        all: bool,
    },
    /// Tasks of the org, newest first (unfinished only unless --state).
    List {
        /// Comma-separated shown states (ready, open, doing, blocked,
        /// review, done, cancelled), or `all`.
        #[arg(long)]
        state: Option<String>,
        /// A seat, or `none` for unassigned tasks.
        #[arg(long)]
        assignee: Option<String>,
        #[arg(long)]
        reporter: Option<String>,
        #[arg(long)]
        team: Option<String>,
        #[arg(long)]
        priority: Option<String>,
        #[arg(long)]
        label: Option<String>,
        #[arg(long, value_name = "TASK")]
        parent: Option<String>,
        #[arg(long, value_name = "TIME")]
        due_before: Option<String>,
        /// `doing` or `review` tasks not changed for the org's stale period.
        #[arg(long)]
        stale: bool,
        /// Text in the title or body.
        #[arg(long)]
        text: Option<String>,
        #[arg(long)]
        limit: Option<u32>,
        /// Continue an earlier listing (its `next_before`).
        #[arg(long)]
        before: Option<i64>,
    },
    /// Your unfinished tasks, ready ones first.
    Mine,
    /// What you can start now: your ready tasks.
    Next {
        /// Also the unassigned ready tasks of your team (take one with
        /// `amail task take`).
        #[arg(long)]
        pool: bool,
    },
    /// Whom you are waiting for.
    Waiting,
    /// Who is waiting for you.
    Owed,
    /// The board: columns by state, load per seat, stale and overdue
    /// tasks, blocked chains.
    Board {
        #[arg(long)]
        team: Option<String>,
    },
    /// Change fields of a task.
    Set {
        task: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        body: Option<String>,
        /// A stored state: open, doing, blocked, review, done, cancelled.
        #[arg(long)]
        state: Option<String>,
        #[arg(long)]
        priority: Option<String>,
        /// Order within the priority (smaller first).
        #[arg(long)]
        rank: Option<i64>,
        /// A seat, or `none`.
        #[arg(long)]
        assignee: Option<String>,
        /// A team, or `none`.
        #[arg(long)]
        team: Option<String>,
        /// A task, or `none`.
        #[arg(long, value_name = "TASK")]
        parent: Option<String>,
        /// Replaces the labels (repeat for more).
        #[arg(long, conflicts_with = "no_labels")]
        label: Vec<String>,
        /// Remove every label.
        #[arg(long)]
        no_labels: bool,
        /// A time, or `none`.
        #[arg(long)]
        due: Option<String>,
    },
    /// Assign a task to a seat (`none` unassigns it).
    Assign { task: String, seat: String },
    /// Assign an unassigned task to yourself (refused if someone has it).
    Take { task: String },
    /// Set a task `blocked`: waiting for a seat's action or an event that is
    /// not another task (use `dep` for that).
    Block {
        task: String,
        /// The seat whose action the task waits for (it gets a reminder).
        #[arg(long, value_name = "SEAT")]
        on: Option<String>,
        /// The reason, one line.
        #[arg(short = 'm', long)]
        reason: Option<String>,
    },
    /// Back to `open` from `blocked`.
    Unblock { task: String },
    /// Set `doing`.
    Start { task: String },
    /// Set `review`.
    Review { task: String },
    /// Set `done`.
    Done { task: String },
    /// Set `cancelled`.
    Cancel { task: String },
    /// Reopen a closed task (`open`).
    Reopen { task: String },
    /// The task depends on another one (`dep t12 --on t9`).
    Dep {
        task: String,
        #[arg(long, value_name = "TASK")]
        on: String,
    },
    /// Remove a dependency.
    Undep {
        task: String,
        #[arg(long, value_name = "TASK")]
        on: String,
    },
    /// Link a thread (by thread id or the id of any message in it).
    Link { task: String, id: i64 },
    /// Unlink a thread.
    Unlink { task: String, thread: i64 },
    /// Comment: mail in the task's discussion thread to the assignee and
    /// reporter. The body comes from -m or stdin.
    Comment {
        task: String,
        #[arg(short = 'm', long)]
        body: Option<String>,
    },
}

/// `t12` or `12`.
pub fn num(s: &str) -> Result<i64> {
    let t = s.trim();
    t.strip_prefix('t')
        .unwrap_or(t)
        .parse::<i64>()
        .ok()
        .filter(|n| *n > 0)
        .with_context(|| format!("{s:?} is not a task number (t12 or 12)"))
}

/// A due time for the server (RFC 3339): a timestamp as given, or a date
/// or date-time without a zone read as UTC.
fn due(s: &str) -> Result<String> {
    let s = s.trim();
    if s.parse::<jiff::Timestamp>().is_ok() {
        return Ok(s.to_string());
    }
    if let Ok(dt) = s.parse::<jiff::civil::DateTime>() {
        return Ok(dt
            .to_zoned(jiff::tz::TimeZone::UTC)?
            .timestamp()
            .to_string());
    }
    if let Ok(d) = s.parse::<jiff::civil::Date>() {
        return Ok(d.to_zoned(jiff::tz::TimeZone::UTC)?.timestamp().to_string());
    }
    bail!("{s:?} is not a time (RFC 3339, 2026-10-30T12:00 or 2026-10-30)")
}

/// `none` (or an empty value) as JSON `null`.
fn or_null(s: &str) -> Value {
    match s.trim() {
        "" | "none" => Value::Null,
        v => json!(v),
    }
}

/// `t9, t11`.
fn nums(v: &[i64]) -> String {
    v.iter()
        .map(|n| format!("t{n}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn seat_or_dash(s: &Option<String>) -> String {
    s.as_deref()
        .map(|s| l(s).into_owned())
        .unwrap_or("-".into())
}

/// `t12  ready   high  w1       Title` (no blockers).
fn row(n: i64, state: TaskState, priority: &str, assignee: &Option<String>, title: &str) -> String {
    format!(
        "{:<4} {:<7} {:<5} {:<8} {}",
        format!("t{n}"),
        state.as_str(),
        priority,
        seat_or_dash(assignee),
        text::subject(title)
    )
    .trim_end()
    .to_string()
}

fn base_line(t: &p::Task) -> String {
    row(t.num, t.state, t.priority.as_str(), &t.assignee, &t.title)
}

/// One task on one line: number, shown state, priority, assignee (or `-`),
/// title, and what it waits on while it is not done.
pub fn task_line(t: &p::Task) -> String {
    let mut s = base_line(t);
    if !t.waits_on.is_empty() && !t.state.is_closed() {
        s.push_str(&format!("  · waits on {}", nums(&t.waits_on)));
    }
    if t.stored_state == TaskState::Blocked {
        match (&t.waiting_on, &t.blocked_reason) {
            (Some(w), Some(r)) => s.push_str(&format!("  · waits for {}: {}", l(w), l(r))),
            (Some(w), None) => s.push_str(&format!("  · waits for {}", l(w))),
            (None, Some(r)) => s.push_str(&format!("  · {}", l(r))),
            (None, None) => {}
        }
    }
    s
}

/// `t9   done    w2       Title`: a task named from another one.
fn ref_line(r: &p::TaskRef) -> String {
    format!(
        "{:<4} {:<7} {:<8} {}",
        format!("t{}", r.num),
        r.state.as_str(),
        seat_or_dash(&r.assignee),
        text::subject(&r.title)
    )
}

fn list_text(tasks: &[p::Task], empty: &str) -> String {
    if tasks.is_empty() {
        empty.to_string()
    } else {
        tasks.iter().map(task_line).collect::<Vec<_>>().join("\n")
    }
}

/// `2026-10-26 10:00Z  w1  changed state: open -> doing`
pub fn event_line(e: &p::TaskEvent) -> String {
    let actor = match (&e.actor, &e.written_by) {
        (Some(a), Some(w)) => format!("{} (by {})", l(a), l(w)),
        (Some(a), None) => l(a).into_owned(),
        (None, _) => "server".into(),
    };
    let v = |x: &Option<String>| {
        x.as_deref()
            .map(|s| l(s).into_owned())
            .unwrap_or("-".into())
    };
    let field = e
        .field
        .as_deref()
        .map(|f| l(f).into_owned())
        .unwrap_or_default();
    use p::TaskEventKind as K;
    let mut what = match e.kind {
        K::Created => format!("created: {}", v(&e.to)),
        K::Changed => format!("changed {field}: {} -> {}", v(&e.from), v(&e.to)),
        K::Assigned => format!("assigned: {} -> {}", v(&e.from), v(&e.to)),
        K::Linked => format!("linked thread {}", v(&e.to)),
        K::Unlinked => format!("unlinked thread {}", v(&e.to)),
        K::DependencyAdded if field == "blocks" => format!("now blocks {}", v(&e.to)),
        K::DependencyAdded => format!("depends on {}", v(&e.to)),
        K::DependencyRemoved if field == "blocks" => format!("no longer blocks {}", v(&e.to)),
        K::DependencyRemoved => format!("no longer depends on {}", v(&e.to)),
        K::Commented => "commented".into(),
        K::ReminderSent => format!("reminder {field} sent to {}", v(&e.to)),
        K::Unblocked => format!("unblocked: {} closed", v(&e.to)),
    };
    if let Some(m) = e.message_id {
        what.push_str(&format!(" (#{m})"));
    }
    if let Some(n) = &e.note
        && e.kind != K::Unblocked
    {
        what.push_str(&format!(" ({})", l(n)));
    }
    format!("{}  {actor}  {what}", short_time(&e.at))
}

/// The task record, one field per line, its body behind `  > `, then the
/// related tasks, links and the last events.
pub fn detail_text(d: &p::TaskDetail) -> String {
    let t = &d.task;
    let mut out = vec![format!("t{} {}", t.num, l(&t.title))];
    let state = if t.state == t.stored_state {
        t.state.to_string()
    } else {
        format!("{} (stored: {})", t.state, t.stored_state)
    };
    out.push(format!("State: {state}"));
    out.push(format!("Priority: {} (rank {})", t.priority, t.rank));
    out.push(format!("Reporter: {}", l(&t.reporter)));
    out.push(format!("Assignee: {}", seat_or_dash(&t.assignee)));
    if let Some(x) = &t.team {
        out.push(format!("Team: {}", l(x)));
    }
    if let Some(x) = t.parent {
        out.push(format!("Parent: t{x}"));
    }
    if !t.labels.is_empty() {
        let ls: Vec<String> = t.labels.iter().map(|x| l(x).into_owned()).collect();
        out.push(format!("Labels: {}", ls.join(", ")));
    }
    if let Some(x) = &t.due {
        out.push(format!("Due: {}", short_time(x)));
    }
    if let Some(x) = &t.waiting_on {
        out.push(format!("Waiting for: {}", l(x)));
    }
    if let Some(x) = &t.blocked_reason {
        out.push(format!("Blocked: {}", l(x)));
    }
    if !t.waits_on.is_empty() {
        out.push(format!("Waits on: {}", nums(&t.waits_on)));
    }
    if let Some(x) = t.thread {
        out.push(format!("Discussion: thread {x}"));
    }
    let mut times = format!(
        "Created: {} · updated {}",
        short_time(&t.created_at),
        short_time(&t.updated_at)
    );
    if let Some(x) = &t.closed_at {
        times.push_str(&format!(" · closed {}", short_time(x)));
    }
    out.push(times);
    if !t.body.is_empty() {
        out.push(String::new());
        out.push(text::quote(&t.body).trim_end().to_string());
    }
    for (name, refs) in [
        ("Blockers", &d.blockers),
        ("Blocked", &d.blocked),
        ("Children", &d.children),
    ] {
        if !refs.is_empty() {
            out.push(format!("{name}:"));
            out.extend(refs.iter().map(|r| format!("  {}", ref_line(r))));
        }
    }
    if !d.links.is_empty() {
        out.push("Links:".into());
        for k in &d.links {
            let by = k
                .linked_by
                .as_deref()
                .map(|b| format!(" by {}", l(b)))
                .unwrap_or_default();
            out.push(format!(
                "  thread {} (linked{by} {})",
                k.thread_id,
                short_time(&k.linked_at)
            ));
        }
    }
    if !d.events.is_empty() {
        out.push(if d.event_count > d.events.len() as i64 {
            format!(
                "Events (last {} of {}; all: amail task history t{} --all):",
                d.events.len(),
                d.event_count,
                t.num
            )
        } else {
            "Events:".into()
        });
        out.extend(d.events.iter().map(event_line));
    }
    out.join("\n")
}

/// Under a thread: the tasks linked to it.
pub fn thread_tasks_text(refs: &[p::TaskRef]) -> Option<String> {
    (!refs.is_empty()).then(|| {
        let mut s = String::from("tasks of this thread:");
        for r in refs {
            s.push_str(&format!("\n  {}", ref_line(r)));
        }
        s
    })
}

fn waiting_text(w: &p::WaitingList) -> String {
    if w.items.is_empty() {
        return "you are not waiting for anyone".into();
    }
    let mut lines: Vec<String> = w
        .items
        .iter()
        .map(|i| {
            let seat = seat_or_dash(&i.seat);
            match &i.blocker {
                Some(b) => format!(
                    "{} — waits for {seat} (t{} {})",
                    base_line(&i.task),
                    b.num,
                    b.state
                ),
                None => match &i.task.blocked_reason {
                    Some(r) => format!("{} — waits for {seat}: {}", base_line(&i.task), l(r)),
                    None => format!("{} — waits for {seat}", base_line(&i.task)),
                },
            }
        })
        .collect();
    if w.truncated {
        lines.push("(more not shown)".into());
    }
    lines.join("\n")
}

fn owed_text(o: &p::OwedList) -> String {
    if o.items.is_empty() {
        return "nobody is waiting for you".into();
    }
    let mut lines: Vec<String> = o
        .items
        .iter()
        .map(|i| {
            let seat = seat_or_dash(&i.seat);
            match &i.blocks {
                Some(b) => format!(
                    "{} — {seat} waits for it (t{} {})",
                    base_line(&i.task),
                    b.num,
                    b.state
                ),
                None => match &i.task.blocked_reason {
                    Some(r) => format!("{} — {seat} waits for you: {}", base_line(&i.task), l(r)),
                    None => format!("{} — {seat} waits for you", base_line(&i.task)),
                },
            }
        })
        .collect();
    if o.truncated {
        lines.push("(more not shown)".into());
    }
    lines.join("\n")
}

fn board_text(b: &p::TaskBoard) -> String {
    let mut out = Vec::new();
    out.push(match &b.team {
        Some(t) => format!("board of team {}", l(t)),
        None => "board".into(),
    });
    out.push(
        b.columns
            .iter()
            .map(|c| format!("{} {}", c.state, c.count))
            .collect::<Vec<_>>()
            .join(" · "),
    );
    for c in &b.columns {
        // Closed columns are counted only.
        if c.count == 0 || c.state.is_closed() {
            continue;
        }
        out.push(String::new());
        out.push(format!("{} ({}):", c.state, c.count));
        out.extend(c.tasks.iter().map(task_line));
        if c.count > c.tasks.len() as i64 {
            out.push(format!(
                "... and {} more (amail task list --state {})",
                c.count - c.tasks.len() as i64,
                c.state
            ));
        }
    }
    if !b.load.is_empty() {
        out.push(String::new());
        out.push("load:".into());
        for s in &b.load {
            out.push(format!(
                "{:<8} doing {} · ready {} · review {} · blocked {} · waiting {}",
                l(&s.seat),
                s.doing,
                s.ready,
                s.review,
                s.blocked,
                s.waiting
            ));
        }
    }
    for (name, v) in [("stale", &b.stale), ("overdue", &b.overdue)] {
        if !v.is_empty() {
            out.push(String::new());
            out.push(format!("{name}:"));
            out.extend(v.iter().map(task_line));
        }
    }
    if !b.chains.is_empty() {
        out.push(String::new());
        out.push("chains:".into());
        for c in &b.chains {
            out.push(format!("t{} waits on {}", c.num, nums(&c.waits_on)));
        }
    }
    out.join("\n")
}

/// Show the answer of a change: the task's line.
fn changed(out: &Out, v: &Value) -> Result<()> {
    out.show(v, || {
        let d: p::TaskDetail = crate::parse(v)?;
        Ok(task_line(&d.task))
    })
}

async fn patch(c: &Client, out: &Out, task: &str, body: Value) -> Result<()> {
    let n = num(task)?;
    let v = c
        .raw(Method::PATCH, &format!("/tasks/{n}"), Some(&body))
        .await?;
    changed(out, &v)
}

async fn set_state(c: &Client, out: &Out, task: &str, state: &str) -> Result<()> {
    patch(c, out, task, json!({ "state": state })).await
}

/// `k=v` pairs, encoded.
fn query(pairs: &[(&str, Option<String>)]) -> String {
    let q: Vec<String> = pairs
        .iter()
        .filter_map(|(k, v)| v.as_ref().map(|v| format!("{k}={}", enc(v))))
        .collect();
    if q.is_empty() {
        String::new()
    } else {
        format!("?{}", q.join("&"))
    }
}

/// The last events shown by `history` without `--all`.
const HISTORY_TAIL: i64 = 50;

pub async fn run(c: &Client, out: &Out, cmd: TaskCmd) -> Result<()> {
    match cmd {
        TaskCmd::New {
            title,
            body,
            priority,
            assignee,
            team,
            parent,
            label,
            due: d,
            from,
        } => {
            if title.is_none() && from.is_none() {
                bail!("give a title (or --from <message id>)");
            }
            let mut b = Map::new();
            let mut put = |k: &str, v: Option<Value>| {
                if let Some(v) = v {
                    b.insert(k.into(), v);
                }
            };
            put("title", title.map(Value::from));
            put("body", body.map(Value::from));
            put("priority", priority.map(Value::from));
            put("assignee", assignee.map(Value::from));
            put("team", team.map(Value::from));
            put(
                "parent",
                parent.as_deref().map(num).transpose()?.map(Value::from),
            );
            put("labels", (!label.is_empty()).then(|| json!(label)));
            put("due", d.as_deref().map(due).transpose()?.map(Value::from));
            put("from_message", from.map(Value::from));
            let v = c
                .raw(Method::POST, "/tasks", Some(&Value::Object(b)))
                .await?;
            changed(out, &v)
        }
        TaskCmd::Show { task } => {
            let n = num(&task)?;
            let v = c.raw(Method::GET, &format!("/tasks/{n}"), None).await?;
            out.show(&v, || Ok(detail_text(&crate::parse(&v)?)))
        }
        TaskCmd::History { task, all } => {
            let n = num(&task)?;
            let (v, earlier) = if all {
                let mut events: Vec<Value> = Vec::new();
                let mut after = 0;
                let compacted = loop {
                    let page = c
                        .raw(
                            Method::GET,
                            &format!("/tasks/{n}/events?after_seq={after}&limit=1000"),
                            None,
                        )
                        .await?;
                    if let Some(e) = page["events"].as_array() {
                        events.extend(e.iter().cloned());
                    }
                    match page["next_after_seq"].as_i64() {
                        Some(a) if a > after => after = a,
                        _ => break page["events_compacted"].clone(),
                    }
                };
                (
                    json!({ "events": events, "events_compacted": compacted }),
                    0,
                )
            } else {
                let d: p::TaskDetail = c.get(&format!("/tasks/{n}")).await?;
                let after = (d.event_count - HISTORY_TAIL).max(0);
                let v = c
                    .raw(
                        Method::GET,
                        &format!("/tasks/{n}/events?after_seq={after}&limit={HISTORY_TAIL}"),
                        None,
                    )
                    .await?;
                (v, after - d.events_compacted)
            };
            out.show(&v, || {
                let page: p::TaskEventsPage = crate::parse(&v)?;
                let mut lines = Vec::new();
                if page.events_compacted > 0 {
                    lines.push(format!(
                        "({} older events were compacted)",
                        page.events_compacted
                    ));
                }
                if earlier > 0 {
                    lines.push(format!(
                        "({earlier} earlier events: amail task history t{n} --all)"
                    ));
                }
                lines.extend(page.events.iter().map(event_line));
                Ok(lines.join("\n"))
            })
        }
        TaskCmd::List {
            state,
            assignee,
            reporter,
            team,
            priority,
            label,
            parent,
            due_before,
            stale,
            text: search,
            limit,
            before,
        } => {
            let q = query(&[
                ("state", state),
                ("assignee", assignee),
                ("reporter", reporter),
                ("team", team),
                ("priority", priority),
                ("label", label),
                (
                    "parent",
                    parent
                        .as_deref()
                        .map(num)
                        .transpose()?
                        .map(|n| n.to_string()),
                ),
                ("due_before", due_before.as_deref().map(due).transpose()?),
                ("stale", stale.then(|| "1".into())),
                ("text", search),
                ("limit", limit.map(|n| n.to_string())),
                ("before", before.map(|n| n.to_string())),
            ]);
            let v = c.raw(Method::GET, &format!("/tasks{q}"), None).await?;
            out.show(&v, || {
                let r: p::TaskList = crate::parse(&v)?;
                let mut s = list_text(&r.tasks, "no tasks");
                if let Some(b) = r.next_before {
                    s.push_str(&format!("\n(older: amail task list --before {b} ...)"));
                }
                Ok(s)
            })
        }
        TaskCmd::Mine | TaskCmd::Next { .. } => {
            let (path, empty) = match cmd {
                TaskCmd::Next { pool: true } => ("/tasks/next?pool=1", "nothing ready"),
                TaskCmd::Next { pool: false } => ("/tasks/next", "nothing ready"),
                _ => ("/tasks/mine", "no open tasks assigned to you"),
            };
            let v = c.raw(Method::GET, path, None).await?;
            out.show(&v, || {
                let r: p::TaskList = crate::parse(&v)?;
                let mut s = list_text(&r.tasks, empty);
                if r.truncated {
                    s.push_str("\n(more not shown: amail task list --assignee <you>)");
                }
                Ok(s)
            })
        }
        TaskCmd::Waiting => {
            let v = c.raw(Method::GET, "/tasks/waiting", None).await?;
            out.show(&v, || Ok(waiting_text(&crate::parse(&v)?)))
        }
        TaskCmd::Owed => {
            let v = c.raw(Method::GET, "/tasks/owed", None).await?;
            out.show(&v, || Ok(owed_text(&crate::parse(&v)?)))
        }
        TaskCmd::Board { team } => {
            let v = c
                .raw(
                    Method::GET,
                    &format!("/tasks/board{}", query(&[("team", team)])),
                    None,
                )
                .await?;
            out.show(&v, || Ok(board_text(&crate::parse(&v)?)))
        }
        TaskCmd::Set {
            task,
            title,
            body,
            state,
            priority,
            rank,
            assignee,
            team,
            parent,
            label,
            no_labels,
            due: d,
        } => {
            let mut b = Map::new();
            if let Some(x) = title {
                b.insert("title".into(), x.into());
            }
            if let Some(x) = body {
                b.insert("body".into(), x.into());
            }
            if let Some(x) = state {
                b.insert("state".into(), x.into());
            }
            if let Some(x) = priority {
                b.insert("priority".into(), x.into());
            }
            if let Some(x) = rank {
                b.insert("rank".into(), x.into());
            }
            if let Some(x) = assignee {
                b.insert("assignee".into(), or_null(&x));
            }
            if let Some(x) = team {
                b.insert("team".into(), or_null(&x));
            }
            if let Some(x) = parent {
                let v = match or_null(&x) {
                    Value::Null => Value::Null,
                    _ => num(&x)?.into(),
                };
                b.insert("parent".into(), v);
            }
            if no_labels {
                b.insert("labels".into(), json!([]));
            } else if !label.is_empty() {
                b.insert("labels".into(), json!(label));
            }
            if let Some(x) = d {
                let v = match or_null(&x) {
                    Value::Null => Value::Null,
                    _ => due(&x)?.into(),
                };
                b.insert("due".into(), v);
            }
            if b.is_empty() {
                bail!("nothing to change: give at least one option (see amail task set --help)");
            }
            patch(c, out, &task, Value::Object(b)).await
        }
        TaskCmd::Assign { task, seat } => {
            patch(c, out, &task, json!({ "assignee": or_null(&seat) })).await
        }
        TaskCmd::Take { task } => {
            let n = num(&task)?;
            let v = c
                .raw(Method::POST, &format!("/tasks/{n}/take"), None)
                .await?;
            changed(out, &v)
        }
        TaskCmd::Block { task, on, reason } => {
            let mut b = json!({ "state": "blocked" });
            if let Some(s) = on {
                b["waiting_on"] = json!(s);
            }
            if let Some(r) = reason {
                b["blocked_reason"] = json!(r);
            }
            patch(c, out, &task, b).await
        }
        TaskCmd::Unblock { task } => set_state(c, out, &task, "open").await,
        TaskCmd::Start { task } => set_state(c, out, &task, "doing").await,
        TaskCmd::Review { task } => set_state(c, out, &task, "review").await,
        TaskCmd::Done { task } => set_state(c, out, &task, "done").await,
        TaskCmd::Cancel { task } => set_state(c, out, &task, "cancelled").await,
        TaskCmd::Reopen { task } => set_state(c, out, &task, "open").await,
        TaskCmd::Dep { task, on } => {
            let (n, m) = (num(&task)?, num(&on)?);
            let v = c
                .raw(
                    Method::POST,
                    &format!("/tasks/{n}/deps"),
                    Some(&json!({ "blocked_by": m })),
                )
                .await?;
            changed(out, &v)
        }
        TaskCmd::Undep { task, on } => {
            let (n, m) = (num(&task)?, num(&on)?);
            let v = c
                .raw(Method::DELETE, &format!("/tasks/{n}/deps/{m}"), None)
                .await?;
            changed(out, &v)
        }
        TaskCmd::Link { task, id } => {
            let n = num(&task)?;
            let path = format!("/tasks/{n}/links");
            // A thread id first; an id that names no thread the caller
            // sees is taken as a message id.
            let v = match c
                .raw(Method::POST, &path, Some(&json!({ "thread_id": id })))
                .await
            {
                Err(e) if is_not_found(&e) => {
                    c.raw(Method::POST, &path, Some(&json!({ "message_id": id })))
                        .await?
                }
                r => r?,
            };
            out.show(&v, || {
                let d: p::TaskDetail = crate::parse(&v)?;
                let threads: Vec<String> =
                    d.links.iter().map(|k| k.thread_id.to_string()).collect();
                Ok(format!(
                    "{}\nlinked threads: {}",
                    task_line(&d.task),
                    threads.join(", ")
                ))
            })
        }
        TaskCmd::Unlink { task, thread } => {
            let n = num(&task)?;
            let v = c
                .raw(Method::DELETE, &format!("/tasks/{n}/links/{thread}"), None)
                .await?;
            out.show(&v, || Ok(format!("unlinked thread {thread} from t{n}")))
        }
        TaskCmd::Comment { task, body } => {
            let n = num(&task)?;
            let body = crate::read_body(body)?;
            let v = c
                .raw(
                    Method::POST,
                    &format!("/tasks/{n}/comments"),
                    Some(&json!({ "body": body })),
                )
                .await?;
            out.show(&v, || {
                let r: p::TaskCommentResponse = crate::parse(&v)?;
                Ok(format!(
                    "commented on t{}: #{} thread {}",
                    r.task, r.id, r.thread_id
                ))
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task() -> p::Task {
        p::Task {
            num: 12,
            title: "Fix the\u{1b}[31m parser".into(),
            body: String::new(),
            state: TaskState::Open,
            stored_state: TaskState::Open,
            priority: p::TaskPriority::High,
            rank: 1,
            reporter: "lead".into(),
            assignee: Some("w1".into()),
            team: None,
            parent: None,
            labels: vec![],
            due: None,
            waiting_on: None,
            blocked_reason: None,
            thread: None,
            waits_on: vec![9, 11],
            created_at: "2026-10-26T10:00:00Z".into(),
            updated_at: "2026-10-26T10:00:00Z".into(),
            closed_at: None,
        }
    }

    #[test]
    fn numbers() {
        assert_eq!(num("t12").unwrap(), 12);
        assert_eq!(num("12").unwrap(), 12);
        assert!(num("t0").is_err() && num("x").is_err() && num("t-1").is_err());
    }

    #[test]
    fn due_times() {
        assert_eq!(due("2026-10-30").unwrap(), "2026-10-30T00:00:00Z");
        assert_eq!(due("2026-10-30T12:30").unwrap(), "2026-10-30T12:30:00Z");
        assert_eq!(
            due("2026-10-30T12:30:00+02:00").unwrap(),
            "2026-10-30T12:30:00+02:00"
        );
        assert!(due("soon").is_err());
    }

    #[test]
    fn lines() {
        let mut t = task();
        assert_eq!(
            task_line(&t),
            "t12  open    high  w1       Fix the\\u{1b}[31m parser  · waits on t9, t11"
        );
        t.state = TaskState::Ready;
        t.waits_on.clear();
        t.assignee = None;
        assert_eq!(
            task_line(&t),
            "t12  ready   high  -        Fix the\\u{1b}[31m parser"
        );
        t.stored_state = TaskState::Blocked;
        t.state = TaskState::Blocked;
        t.waiting_on = Some("w3".into());
        t.blocked_reason = Some("needs\nkeys".into());
        assert!(
            task_line(&t).ends_with("  · waits for w3: needs keys"),
            "{}",
            task_line(&t)
        );
        let e = p::TaskEvent {
            seq: 2,
            at: "2026-10-26T10:00:01.123Z".into(),
            actor: Some("w1".into()),
            written_by: None,
            kind: p::TaskEventKind::Changed,
            field: Some("state".into()),
            from: Some("open".into()),
            to: Some("doing".into()),
            note: None,
            message_id: None,
        };
        assert_eq!(
            event_line(&e),
            "2026-10-26 10:00Z  w1  changed state: open -> doing"
        );
    }
}
