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
        /// Create a task even if the same one (reporter, title, body,
        /// assignee, parent) was created in the last 24 hours. Without it,
        /// running the same command again answers the task it created.
        #[arg(long = "new")]
        new: bool,
    },
    /// A task: its fields, blockers, blocked tasks, children, linked
    /// threads and last events. A task number, or the id of an imported
    /// task.
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
        /// ok, rejected, or none (no result).
        #[arg(long)]
        result: Option<String>,
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
    /// The lead's report: assigned but not started, in progress, waiting
    /// for review, rejected, approved but not taken further, parents partly
    /// done, blocked.
    Report {
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
        /// With --state done or cancelled: close a parent although some
        /// of its children are not closed.
        #[arg(long)]
        force: bool,
        /// Refuse the change if the task changed since this revision
        /// (`Rev:` of `task show`); pass it when changing the title or body.
        #[arg(long, value_name = "REV")]
        if_rev: Option<i64>,
    },
    /// Assign a task to a seat (`none` unassigns it).
    Assign {
        task: String,
        seat: String,
        #[arg(long, value_name = "REV")]
        if_rev: Option<i64>,
    },
    /// Assign an unassigned task to yourself (refused if someone has it).
    Take {
        task: String,
        #[arg(long, value_name = "REV")]
        if_rev: Option<i64>,
    },
    /// Ask the assignee of a task that is not started to hand it over to
    /// you (they answer with `yield` or `keep`).
    Steal { task: String },
    /// Answer a request to take over your task: hand it over.
    Yield { task: String },
    /// Answer a request to take over your task: keep it.
    Keep { task: String },
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
        #[arg(long, value_name = "REV")]
        if_rev: Option<i64>,
    },
    /// Back to `open` from `blocked`.
    Unblock {
        task: String,
        #[arg(long, value_name = "REV")]
        if_rev: Option<i64>,
    },
    /// Set `doing`.
    Start {
        task: String,
        #[arg(long, value_name = "REV")]
        if_rev: Option<i64>,
    },
    /// Set `review`.
    Review {
        task: String,
        #[arg(long, value_name = "REV")]
        if_rev: Option<i64>,
    },
    /// Set `done`, with an optional result (what was concluded, for which
    /// version).
    Done {
        task: String,
        /// ok or rejected.
        #[arg(long)]
        result: Option<String>,
        /// With --result: the version it is about (a commit, tag, URL).
        #[arg(long = "ref", value_name = "REF", requires = "result")]
        result_ref: Option<String>,
        /// With --result: a note.
        #[arg(short = 'm', long, requires = "result")]
        note: Option<String>,
        /// Close a parent although some of its children are not closed.
        #[arg(long)]
        force: bool,
        #[arg(long, value_name = "REV")]
        if_rev: Option<i64>,
    },
    /// Set `cancelled`.
    Cancel {
        task: String,
        /// Close a parent although some of its children are not closed.
        #[arg(long)]
        force: bool,
        #[arg(long, value_name = "REV")]
        if_rev: Option<i64>,
    },
    /// Reopen a closed task (`open`); its result is cleared.
    Reopen {
        task: String,
        #[arg(long, value_name = "REV")]
        if_rev: Option<i64>,
    },
    /// The result of a done task: `result t12 ok|rejected [--ref r] [-m
    /// note]`; `result withdraw t12 -m reason` clears it.
    Result {
        /// The task, or `withdraw`.
        first: String,
        /// ok or rejected; after `withdraw`, the task.
        second: String,
        /// The version the result is about (a commit, tag, URL).
        #[arg(long = "ref", value_name = "REF")]
        result_ref: Option<String>,
        /// A note; for `withdraw`, the reason (required).
        #[arg(short = 'm', long)]
        note: Option<String>,
        #[arg(long, value_name = "REV")]
        if_rev: Option<i64>,
    },
    /// The task depends on another one (`dep t12 --on t9`). Repeat it with
    /// --needs-ok or --any-close to change an existing dependency.
    Dep {
        task: String,
        #[arg(long, value_name = "TASK")]
        on: String,
        /// Only a done result `ok` of the other task satisfies it.
        #[arg(long, conflicts_with = "any_close")]
        needs_ok: bool,
        /// Any close of the other task satisfies it (the default).
        #[arg(long)]
        any_close: bool,
        #[arg(long, value_name = "REV")]
        if_rev: Option<i64>,
    },
    /// Remove a dependency.
    Undep {
        task: String,
        #[arg(long, value_name = "TASK")]
        on: String,
        #[arg(long, value_name = "REV")]
        if_rev: Option<i64>,
    },
    /// Link a thread (by thread id or the id of any message in it).
    Link {
        task: String,
        id: i64,
        #[arg(long, value_name = "REV")]
        if_rev: Option<i64>,
    },
    /// Unlink a thread.
    Unlink {
        task: String,
        thread: i64,
        #[arg(long, value_name = "REV")]
        if_rev: Option<i64>,
    },
    /// Import tasks from a JSON Lines file (user token): one object per
    /// task with `id` and `title` (see the reference). Importing the same
    /// file again updates the tasks it created.
    Import {
        file: std::path::PathBuf,
        /// Check the file and print what would happen; change nothing.
        #[arg(long)]
        dry_run: bool,
        /// Write the pairs `id<TAB>tN` to this file.
        #[arg(long, value_name = "FILE")]
        map: Option<std::path::PathBuf>,
    },
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

/// `t9 (ok needed), t11`: blockers, marking those whose dependency needs
/// an ok result.
fn waits(v: &[i64], ok: &[i64]) -> String {
    v.iter()
        .map(|n| {
            if ok.contains(n) {
                format!("t{n} (ok needed)")
            } else {
                format!("t{n}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The key `task new` sends by default: the first 32 hex digits of SHA-256
/// over reporter, title, body, assignee and parent (and the message of
/// `--from`, when given).
pub fn default_client_key(
    reporter: &str,
    title: Option<&str>,
    body: Option<&str>,
    assignee: Option<&str>,
    parent: Option<i64>,
    from: Option<i64>,
) -> String {
    use sha2::Digest;
    let mut v = json!([reporter, title, body, assignee, parent]);
    if let (Some(m), Some(a)) = (from, v.as_array_mut()) {
        a.push(json!(m));
    }
    let digest = sha2::Sha256::digest(v.to_string().as_bytes());
    hex::encode(digest)[..32].to_string()
}

/// The caller's seat name, for [`default_client_key`].
pub async fn reporter_name(c: &Client) -> Result<String> {
    let me: p::WhoAmI = c.get("/whoami").await?;
    Ok(me.seat.unwrap_or_else(|| format!("{}:user", me.org)))
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
        s.push_str(&format!(
            "  · waits on {}",
            waits(&t.waits_on, &t.waits_on_ok)
        ));
    }
    if t.stored_state == TaskState::Blocked {
        match (&t.waiting_on, &t.blocked_reason) {
            (Some(w), Some(r)) => s.push_str(&format!("  · waits for {}: {}", l(w), l(r))),
            (Some(w), None) => s.push_str(&format!("  · waits for {}", l(w))),
            (None, Some(r)) => s.push_str(&format!("  · {}", l(r))),
            (None, None) => {}
        }
    }
    if let Some(w) = &t.steal_requested_by {
        s.push_str(&format!("  · {} asks to take over", l(w)));
    }
    if let Some(p) = &t.progress {
        s.push_str(&format!("  · {}/{}", p.done, p.total));
    }
    if let Some(r) = t.result {
        s.push_str(&format!("  · {r}"));
    }
    s
}

/// `t9   done    w2       Title`: a task named from another one, with
/// its result and whether a dependency on it needs an ok result.
fn ref_line(r: &p::TaskRef) -> String {
    let mut s = format!(
        "{:<4} {:<7} {:<8} {}",
        format!("t{}", r.num),
        r.state.as_str(),
        seat_or_dash(&r.assignee),
        text::subject(&r.title)
    );
    if let Some(x) = r.result {
        s.push_str(&format!("  · {x}"));
    }
    if r.needs_ok {
        s.push_str("  (ok needed)");
    }
    s
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
        K::StealRequested => format!("asked to take over from {}", v(&e.from)),
        K::StealDeclined => format!("request of {} to take over declined", v(&e.to)),
        K::StealExpired => format!("request of {} to take over expired", v(&e.to)),
        K::ResultWithdrawn => format!("result withdrawn: {}", v(&e.from)),
        K::Imported => format!("imported: {}", v(&e.note)),
    };
    if let Some(m) = e.message_id {
        what.push_str(&format!(" (#{m})"));
    }
    if let Some(n) = &e.note
        && !matches!(e.kind, K::Unblocked | K::Imported)
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
    out.push(format!("Rev: {}", t.rev));
    if let Some(r) = t.result {
        let mut s = format!("Result: {r}");
        if let Some(x) = &t.result_ref {
            s.push_str(&format!(", ref {}", l(x)));
        }
        if let Some(x) = &t.result_by {
            s.push_str(&format!(", by {}", l(x)));
        }
        if let Some(x) = &t.result_at {
            s.push_str(&format!(" at {}", short_time(x)));
        }
        out.push(s);
        if let Some(n) = &t.result_note {
            out.push(format!("Result note: {}", l(n)));
        }
    }
    out.push(format!("Priority: {} (rank {})", t.priority, t.rank));
    out.push(format!("Reporter: {}", l(&t.reporter)));
    out.push(format!("Assignee: {}", seat_or_dash(&t.assignee)));
    if let Some(w) = &t.steal_requested_by {
        let at = t
            .steal_requested_at
            .as_deref()
            .map(|a| format!(" at {}", short_time(a)))
            .unwrap_or_default();
        out.push(format!(
            "Takeover: asked for by {}{at} (the assignee answers: amail task yield t{n} / keep t{n})",
            l(w),
            n = t.num
        ));
    }
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
        out.push(format!("Waits on: {}", waits(&t.waits_on, &t.waits_on_ok)));
    }
    if let Some(p) = &t.progress {
        out.push(format!("Children: {}/{} done", p.done, p.total));
    }
    if let Some(k) = &t.import_key {
        out.push(format!("Imported as: {}", l(k)));
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

/// The heading of a report section.
fn section_title(name: &str) -> &str {
    match name {
        "not_started" => "assigned, not started",
        "in_progress" => "in progress",
        "review" => "delivered, waiting for review",
        "rejected" => "rejected",
        "approved" => "approved, not taken further",
        "parents" => "parents partly done",
        "blocked" => "blocked",
        other => other,
    }
}

/// The lead's report: one section per kind, with counts; tasks grouped by
/// seat where the section is.
pub fn report_text(r: &p::TaskReport) -> String {
    let mut out = vec![match &r.team {
        Some(t) => format!("report of team {}", l(t)),
        None => "report".into(),
    }];
    for s in &r.sections {
        out.push(String::new());
        out.push(format!("{} ({}):", section_title(&s.name), s.count));
        if s.items.is_empty() {
            out.push("  none".into());
        }
        let mut seat: Option<&str> = None;
        let grouped = matches!(s.name.as_str(), "not_started" | "in_progress" | "review");
        for i in &s.items {
            if grouped && i.seat.as_deref() != seat {
                seat = i.seat.as_deref();
                out.push(format!(
                    "  {}:",
                    i.seat
                        .as_deref()
                        .map(|x| l(x).into_owned())
                        .unwrap_or("-".into())
                ));
            }
            let indent = if grouped { "    " } else { "  " };
            let mut line = format!("{indent}{}", task_line(&i.task));
            if !i.waiting_for.is_empty() {
                let w: Vec<String> = i.waiting_for.iter().map(|x| l(x).into_owned()).collect();
                line.push_str(&format!(" — waits for {}", w.join(", ")));
            }
            if !i.dependents.is_empty() {
                let d: Vec<String> = i.dependents.iter().map(|x| format!("t{}", x.num)).collect();
                line.push_str(&format!(" — still open: {}", d.join(", ")));
            }
            out.push(line);
        }
        if s.count > s.items.len() as i64 {
            out.push(format!("  ... and {} more", s.count - s.items.len() as i64));
        }
    }
    out.join("\n")
}

/// `ok` or `rejected`.
fn result_value(s: &str) -> Result<&'static str> {
    match s.trim() {
        "ok" => Ok("ok"),
        "rejected" => Ok("rejected"),
        other => bail!("{other:?} is not a result: ok or rejected"),
    }
}

/// The records of a JSON Lines import file (blank lines and lines
/// starting with `#` skipped).
fn read_records(file: &std::path::Path) -> Result<Vec<Value>> {
    let text =
        std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let v: Value = serde_json::from_str(line)
            .with_context(|| format!("{}:{}: not a JSON object", file.display(), i + 1))?;
        if !v.is_object() {
            bail!("{}:{}: not a JSON object", file.display(), i + 1);
        }
        out.push(v);
    }
    Ok(out)
}

async fn import(
    c: &Client,
    out: &Out,
    file: &std::path::Path,
    dry_run: bool,
    map: Option<&std::path::Path>,
) -> Result<()> {
    let records = read_records(file)?;
    if records.is_empty() {
        bail!("{}: no records", file.display());
    }
    let v = c
        .raw(
            Method::POST,
            "/tasks/import",
            Some(&json!({ "records": records, "dry_run": dry_run })),
        )
        .await?;
    let r: p::TaskImportResponse = crate::parse(&v)?;
    if let Some(m) = map
        && !dry_run
    {
        let mut s = String::new();
        for x in &r.results {
            if let Some(n) = x.num {
                s.push_str(&format!("{}\tt{n}\n", x.id));
            }
        }
        std::fs::write(m, s).with_context(|| format!("writing {}", m.display()))?;
    }
    out.show(&v, || Ok(import_text(&r)))
}

fn import_text(r: &p::TaskImportResponse) -> String {
    let mut out: Vec<String> = r
        .results
        .iter()
        .map(|x| {
            let to = x.num.map(|n| format!("t{n}")).unwrap_or("(new)".into());
            format!("{} -> {to} ({})", l(&x.id), x.action)
        })
        .collect();
    for w in &r.warnings {
        out.push(format!("warning: {}", l(w)));
    }
    if r.dry_run {
        out.push("(dry run: nothing was changed)".into());
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

/// `body` with `if_rev` when given.
fn with_rev(mut body: Value, if_rev: Option<i64>) -> Value {
    if let (Some(r), Some(o)) = (if_rev, body.as_object_mut()) {
        o.insert("if_rev".into(), json!(r));
    }
    body
}

/// `?if_rev=N` when given.
fn rev_query(if_rev: Option<i64>) -> String {
    if_rev.map(|r| format!("?if_rev={r}")).unwrap_or_default()
}

async fn patch(c: &Client, out: &Out, task: &str, body: Value, if_rev: Option<i64>) -> Result<()> {
    let n = num(task)?;
    let body = with_rev(body, if_rev);
    let v = c
        .raw(Method::PATCH, &format!("/tasks/{n}"), Some(&body))
        .await?;
    changed(out, &v)
}

/// `POST /tasks/{num}/<action>` without a body (or with `if_rev`).
async fn post(c: &Client, out: &Out, task: &str, action: &str, if_rev: Option<i64>) -> Result<()> {
    let n = num(task)?;
    let body = if_rev.map(|r| json!({ "if_rev": r }));
    let v = c
        .raw(Method::POST, &format!("/tasks/{n}/{action}"), body.as_ref())
        .await?;
    changed(out, &v)
}

async fn set_state(
    c: &Client,
    out: &Out,
    task: &str,
    state: &str,
    if_rev: Option<i64>,
) -> Result<()> {
    patch(c, out, task, json!({ "state": state }), if_rev).await
}

/// A task number, or the id of an imported task (found by its import key).
async fn resolve(c: &Client, task: &str) -> Result<i64> {
    if let Ok(n) = num(task) {
        return Ok(n);
    }
    let r: p::TaskList = c
        .get(&format!("/tasks?state=all&import_key={}", enc(task.trim())))
        .await?;
    match r.tasks.first() {
        Some(t) => Ok(t.num),
        None => bail!("{task:?} is not a task number (t12 or 12) nor the id of an imported task"),
    }
}

/// The text of a 409 `stale` answer: what changed since the revision the
/// command named, and the task as it is now.
pub fn stale_text(s: &p::TaskStale, if_rev_hint: bool) -> String {
    let mut out = vec![format!(
        "t{} changed since your revision; it is now at rev {}.",
        s.task.task.num, s.rev
    )];
    if !s.since.is_empty() {
        out.push("Changes since then:".into());
        out.extend(s.since.iter().map(|e| format!("  {}", event_line(e))));
    }
    out.push(String::new());
    out.push(detail_text(&s.task));
    if if_rev_hint {
        out.push(String::new());
        out.push(format!(
            "Nothing was changed. Check the changes above, then run the command again with --if-rev {}.",
            s.rev
        ));
    }
    out.join("\n")
}

/// Run a task command; a 409 `stale` prints the task as it is now (with
/// `--json`, the whole answer) before the error.
pub async fn run(c: &Client, out: &Out, cmd: TaskCmd) -> Result<()> {
    let r = run_cmd(c, out, cmd).await;
    if let Err(e) = &r
        && let Some(api) = e.downcast_ref::<crate::client::ApiError>()
        && api.body.error.code == "stale"
        && let Ok(s) = serde_json::from_value::<p::TaskStale>(api.raw.clone())
    {
        out.show(&api.raw, || Ok(stale_text(&s, true)))?;
    }
    r
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

async fn run_cmd(c: &Client, out: &Out, cmd: TaskCmd) -> Result<()> {
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
            new,
        } => {
            if title.is_none() && from.is_none() {
                bail!("give a title (or --from <message id>)");
            }
            let parent_num = parent.as_deref().map(num).transpose()?;
            let key = if new {
                None
            } else {
                let me = reporter_name(c).await?;
                Some(default_client_key(
                    &me,
                    title.as_deref(),
                    body.as_deref(),
                    assignee.as_deref(),
                    parent_num,
                    from,
                ))
            };
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
            put("parent", parent_num.map(Value::from));
            put("labels", (!label.is_empty()).then(|| json!(label)));
            put("due", d.as_deref().map(due).transpose()?.map(Value::from));
            put("from_message", from.map(Value::from));
            put("client_key", key.map(Value::from));
            let v = c
                .raw(Method::POST, "/tasks", Some(&Value::Object(b)))
                .await?;
            out.show(&v, || {
                let d: p::TaskDetail = crate::parse(&v)?;
                let line = task_line(&d.task);
                Ok(if d.existing {
                    format!(
                        "{line}\n(created before by the same command; nothing new was created: amail task new --new creates another)"
                    )
                } else {
                    line
                })
            })
        }
        TaskCmd::Show { task } => {
            let n = resolve(c, &task).await?;
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
                (v, after)
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
            result,
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
                ("result", result),
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
        TaskCmd::Report { team } => {
            let v = c
                .raw(
                    Method::GET,
                    &format!("/tasks/report{}", query(&[("team", team)])),
                    None,
                )
                .await?;
            out.show(&v, || Ok(report_text(&crate::parse(&v)?)))
        }
        TaskCmd::Import { file, dry_run, map } => {
            import(c, out, &file, dry_run, map.as_deref()).await
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
            force,
            if_rev,
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
            if force {
                b.insert("force".into(), json!(true));
            }
            patch(c, out, &task, Value::Object(b), if_rev).await
        }
        TaskCmd::Assign { task, seat, if_rev } => {
            patch(c, out, &task, json!({ "assignee": or_null(&seat) }), if_rev).await
        }
        TaskCmd::Take { task, if_rev } => post(c, out, &task, "take", if_rev).await,
        TaskCmd::Steal { task } => post(c, out, &task, "steal", None).await,
        TaskCmd::Yield { task } => post(c, out, &task, "yield", None).await,
        TaskCmd::Keep { task } => post(c, out, &task, "keep", None).await,
        TaskCmd::Block {
            task,
            on,
            reason,
            if_rev,
        } => {
            let mut b = json!({ "state": "blocked" });
            if let Some(s) = on {
                b["waiting_on"] = json!(s);
            }
            if let Some(r) = reason {
                b["blocked_reason"] = json!(r);
            }
            patch(c, out, &task, b, if_rev).await
        }
        TaskCmd::Unblock { task, if_rev } => set_state(c, out, &task, "open", if_rev).await,
        TaskCmd::Start { task, if_rev } => set_state(c, out, &task, "doing", if_rev).await,
        TaskCmd::Review { task, if_rev } => set_state(c, out, &task, "review", if_rev).await,
        TaskCmd::Done {
            task,
            result,
            result_ref,
            note,
            force,
            if_rev,
        } => {
            let mut b = json!({ "state": "done" });
            if let Some(r) = result {
                b["result"] = json!(result_value(&r)?);
            }
            if let Some(r) = result_ref {
                b["result_ref"] = json!(r);
            }
            if let Some(n) = note {
                b["result_note"] = json!(n);
            }
            if force {
                b["force"] = json!(true);
            }
            patch(c, out, &task, b, if_rev).await
        }
        TaskCmd::Cancel {
            task,
            force,
            if_rev,
        } => {
            let mut b = json!({ "state": "cancelled" });
            if force {
                b["force"] = json!(true);
            }
            patch(c, out, &task, b, if_rev).await
        }
        TaskCmd::Reopen { task, if_rev } => set_state(c, out, &task, "open", if_rev).await,
        TaskCmd::Result {
            first,
            second,
            result_ref,
            note,
            if_rev,
        } => {
            let b = if first == "withdraw" {
                let Some(reason) = note else {
                    bail!("give the reason: amail task result withdraw {second} -m \"...\"");
                };
                if result_ref.is_some() {
                    bail!("--ref goes with ok or rejected, not with withdraw");
                }
                patch(
                    c,
                    out,
                    &second,
                    json!({ "result": null, "note": reason }),
                    if_rev,
                )
                .await?;
                return Ok(());
            } else {
                let mut b = json!({ "result": result_value(&second)? });
                if let Some(r) = result_ref {
                    b["result_ref"] = json!(r);
                }
                if let Some(n) = note {
                    b["result_note"] = json!(n);
                }
                b
            };
            patch(c, out, &first, b, if_rev).await
        }
        TaskCmd::Dep {
            task,
            on,
            needs_ok,
            any_close,
            if_rev,
        } => {
            let (n, m) = (num(&task)?, num(&on)?);
            let mut b = json!({ "blocked_by": m });
            if needs_ok {
                b["needs_ok"] = json!(true);
            } else if any_close {
                b["needs_ok"] = json!(false);
            }
            let b = with_rev(b, if_rev);
            let v = c
                .raw(Method::POST, &format!("/tasks/{n}/deps"), Some(&b))
                .await?;
            changed(out, &v)
        }
        TaskCmd::Undep { task, on, if_rev } => {
            let (n, m) = (num(&task)?, num(&on)?);
            let v = c
                .raw(
                    Method::DELETE,
                    &format!("/tasks/{n}/deps/{m}{}", rev_query(if_rev)),
                    None,
                )
                .await?;
            changed(out, &v)
        }
        TaskCmd::Link { task, id, if_rev } => {
            let n = num(&task)?;
            let path = format!("/tasks/{n}/links");
            // A thread id first; an id that names no thread the caller
            // sees is taken as a message id.
            let v = match c
                .raw(
                    Method::POST,
                    &path,
                    Some(&with_rev(json!({ "thread_id": id }), if_rev)),
                )
                .await
            {
                Err(e) if is_not_found(&e) => {
                    c.raw(
                        Method::POST,
                        &path,
                        Some(&with_rev(json!({ "message_id": id }), if_rev)),
                    )
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
        TaskCmd::Unlink {
            task,
            thread,
            if_rev,
        } => {
            let n = num(&task)?;
            let v = c
                .raw(
                    Method::DELETE,
                    &format!("/tasks/{n}/links/{thread}{}", rev_query(if_rev)),
                    None,
                )
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
            steal_requested_by: None,
            steal_requested_at: None,
            waits_on_ok: vec![11],
            rev: 3,
            progress: None,
            result: None,
            result_ref: None,
            result_note: None,
            result_by: None,
            result_at: None,
            import_key: None,
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
            "t12  open    high  w1       Fix the\\u{1b}[31m parser  · waits on t9, t11 (ok needed)"
        );
        t.progress = Some(p::TaskProgress { done: 3, total: 5 });
        t.result = Some(p::TaskResult::Rejected);
        assert!(
            task_line(&t).ends_with("  · 3/5  · rejected"),
            "{}",
            task_line(&t)
        );
        t.progress = None;
        t.result = None;
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
