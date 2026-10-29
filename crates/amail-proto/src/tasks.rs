//! Wire types of tasks (`/v1/tasks…`): one shared task record per org,
//! with dependencies, linked threads and a history of events.
//!
//! Tasks are numbered per org (`num`, shown as `t12`); numbers are never
//! reused. Everything here is visible to every seat of the owning org and
//! to nothing outside it.

use super::{Deserialize, FromStr, Serialize, double_option, fmt};

string_enum!(
    /// A task's state. The stored state is one of `open`, `doing`,
    /// `blocked`, `review`, `done`, `cancelled`. `ready` is never stored:
    /// answers show it in place of `open` when every blocker of the task
    /// is closed (`done` or `cancelled`), see [`Task::state`].
    TaskState {
        Open => "open",
        Ready => "ready",
        Doing => "doing",
        Blocked => "blocked",
        Review => "review",
        Done => "done",
        Cancelled => "cancelled",
    }
);

impl TaskState {
    /// `done` or `cancelled`.
    pub fn is_closed(self) -> bool {
        matches!(self, TaskState::Done | TaskState::Cancelled)
    }
}

string_enum!(
    /// A task's priority; lists sort by it (urgent first), then by `rank`,
    /// then by age.
    TaskPriority { Urgent => "urgent", High => "high", Normal => "normal", Low => "low" }
);

impl TaskPriority {
    /// 0 for `urgent` up to 3 for `low` (the sort order).
    pub fn order(self) -> i64 {
        match self {
            TaskPriority::Urgent => 0,
            TaskPriority::High => 1,
            TaskPriority::Normal => 2,
            TaskPriority::Low => 3,
        }
    }
}

string_enum!(
    /// The result of a closed task: what was concluded about the work
    /// (see [`Task::result`]).
    TaskResult { Ok => "ok", Rejected => "rejected" }
);

string_enum!(
    /// The kind of a task event (see [`TaskEvent`]).
    TaskEventKind {
        Created => "created",
        Changed => "changed",
        Assigned => "assigned",
        Linked => "linked",
        Unlinked => "unlinked",
        DependencyAdded => "dependency_added",
        DependencyRemoved => "dependency_removed",
        Commented => "commented",
        ReminderSent => "reminder_sent",
        Unblocked => "unblocked",
        StealRequested => "steal_requested",
        StealDeclined => "steal_declined",
        StealExpired => "steal_expired",
        ResultWithdrawn => "result_withdrawn",
        Imported => "imported",
    }
);

/// A task as answers show it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    pub num: i64,
    pub title: String,
    /// The description. Listings leave it out (empty); `GET /tasks/{num}`
    /// and the answers to changes carry it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub body: String,
    /// The shown state: `ready` for a stored `open` task whose blockers are
    /// all closed, otherwise the stored state.
    pub state: TaskState,
    /// The stored state (never `ready`).
    pub stored_state: TaskState,
    pub priority: TaskPriority,
    /// Order within a priority (smaller first); new tasks go last.
    pub rank: i64,
    /// The seat that created the task.
    pub reporter: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    /// The parent task's number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due: Option<String>,
    /// With state `blocked`: the seat whose action the task waits for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_on: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<String>,
    /// The discussion thread (created by the first comment).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<i64>,
    /// Numbers of the blockers that do not satisfy their dependency yet
    /// (`waits on t9, t11`), lowest first: not closed, or, for a
    /// dependency that needs an ok result, not `done` with `ok`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub waits_on: Vec<i64>,
    /// The numbers in `waits_on` whose dependency needs an ok result
    /// (`waits on t9 (ok needed)`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub waits_on_ok: Vec<i64>,
    /// The task's revision: the sequence number of its newest event. A
    /// write with `if_rev` is refused with `stale` when it differs.
    #[serde(default)]
    pub rev: i64,
    /// For a task with child tasks: how many are `done` of how many are not
    /// `cancelled`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<TaskProgress>,
    /// The result of a `done` task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<TaskResult>,
    /// The version the result is about: a commit, tag, URL or other name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_note: Option<String>,
    /// The seat that set the result (for an imported result: the name the
    /// file gives).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_at: Option<String>,
    /// The id the task had in the file it was imported from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_key: Option<String>,
    /// A pending request of this seat to take the task over (see
    /// `POST /tasks/{num}/steal`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steal_requested_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steal_requested_at: Option<String>,
    /// The number of the oldest open request for the person on this task
    /// (`r12`, see `POST /requests`): the task shows `blocked-on-human`,
    /// and `take`, `steal` and `GET /tasks/next` skip it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_on_human: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<String>,
}

/// How many child tasks of a task are done (see [`Task::progress`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskProgress {
    /// Children in `done`.
    pub done: i64,
    /// Children that are not `cancelled`.
    pub total: i64,
}

/// A task named from another task or a thread: its number, title, shown
/// state and assignee.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRef {
    pub num: i64,
    pub title: String,
    pub state: TaskState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<TaskResult>,
    /// In `blockers`: the dependency needs an ok result of this task.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub needs_ok: bool,
}

/// A thread linked to a task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskLink {
    pub thread_id: i64,
    /// Who linked it; absent for a link made from a `[tNN]` subject tag by
    /// a seat that no longer exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linked_by: Option<String>,
    pub linked_at: String,
}

/// One entry of a task's history. Events are appended and never changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskEvent {
    /// 1, 2, ... per task.
    pub seq: i64,
    pub at: String,
    /// The seat that made the change; absent for what the server did on
    /// its own (reminders of the stale and due passes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    /// For a change the user made in the name of a seat: the user seat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub written_by: Option<String>,
    pub kind: TaskEventKind,
    /// `changed`: the field (`title`, `body`, `state`, `priority`, `rank`,
    /// `team`, `parent`, `labels`, `due`, `waiting_on`,
    /// `blocked_reason`). `reminder_sent`: the reminder (`assigned`,
    /// `unblocked`, `waiting`, `closed`, `urgent`, `stale`, `due`, `steal`,
    /// `steal_declined`, `steal_expired`). `steal_*`: `from` is the
    /// assignee, `to` the requesting seat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// The value before (`changed`, `assigned`). A change of the body
    /// carries the whole old and new text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// The value after; `dependency_*`: the other task (`t9`);
    /// `linked`/`unlinked`: the thread id; `reminder_sent`: the recipient.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// `commented` and `reminder_sent`: the message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<i64>,
}

/// Answer of `GET /v1/tasks/{num}` and of every change of one task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskDetail {
    pub task: Task,
    /// Every task this one depends on (`blocked_by`), closed ones included.
    #[serde(default)]
    pub blockers: Vec<TaskRef>,
    /// Every task that depends on this one.
    #[serde(default)]
    pub blocked: Vec<TaskRef>,
    /// The child tasks (`parent` is this task).
    #[serde(default)]
    pub children: Vec<TaskRef>,
    #[serde(default)]
    pub links: Vec<TaskLink>,
    /// The newest events (at most [`crate::limits::TASK_SHOWN_EVENTS`]),
    /// oldest first.
    #[serde(default)]
    pub events: Vec<TaskEvent>,
    /// Events in all (every event is kept).
    #[serde(default)]
    pub event_count: i64,
    /// Answer of `POST /tasks` with a `client_key` used before: the task
    /// that key created; nothing was created.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub existing: bool,
}

/// Body of `POST /v1/tasks`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NewTask {
    /// Required unless `from_message` is given (then the message's subject
    /// is the default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<TaskPriority>,
    /// A seat name of the org.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    /// A team name of the org (default: the assignee's team, else the
    /// reporter's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    /// The parent task's number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    /// RFC 3339.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due: Option<String>,
    /// A message the caller can read: its subject is the default title and
    /// its thread is linked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_message: Option<i64>,
    /// 1 to 64 characters. A second create with the same key in the same
    /// org within 24 hours answers the task the first one created (200,
    /// `existing`) and creates nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_key: Option<String>,
}

/// Body of `PATCH /v1/tasks/{num}`: a missing field is unchanged; `null`
/// clears a field that can be empty.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// A stored state (not `ready`). Leaving `blocked` clears
    /// `waiting_on` and `blocked_reason`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<TaskState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<TaskPriority>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rank: Option<i64>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub assignee: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub team: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub parent: Option<Option<i64>>,
    /// Replaces the labels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labels: Option<Vec<String>>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub due: Option<Option<String>>,
    /// Only with the state `blocked` (given here or already stored).
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub waiting_on: Option<Option<String>>,
    /// Only with the state `blocked` (given here or already stored).
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub blocked_reason: Option<Option<String>>,
    /// The result, only for a task that is `done` (stored or set here);
    /// replaces `result_ref` and `result_note` with the ones given.
    /// `null` withdraws the result (the task stays `done`; `note` gives
    /// the reason).
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub result: Option<Option<TaskResult>>,
    /// With `result`: the version the result is about (at most 200
    /// characters).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_ref: Option<String>,
    /// With `result`: at most 2,000 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_note: Option<String>,
    /// With `result: null`: why the result is withdrawn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Close a parent although some of its children are not closed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub force: bool,
    /// Refuse the change with `stale` unless the task's `rev` is this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub if_rev: Option<i64>,
}

/// Body of `POST /v1/tasks/{num}/take` (optional).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskRevRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub if_rev: Option<i64>,
}

/// Body of `POST /v1/tasks/{num}/deps`: the task `num` depends on
/// `blocked_by`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskDepRequest {
    pub blocked_by: i64,
    /// Only a `done` blocker with result `ok` satisfies the dependency.
    /// Given for an existing dependency, it changes the dependency; absent,
    /// a new dependency is satisfied by any close.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needs_ok: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub if_rev: Option<i64>,
}

/// Body of `POST /v1/tasks/{num}/links`: a thread, by its id or by one of
/// its messages.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskLinkRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub if_rev: Option<i64>,
}

/// Body of a 409 `stale` answer: a write with `if_rev` found the task at
/// another revision. `error` is the usual error; `since` are the events
/// after `if_rev`, oldest first.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskStale {
    pub error: crate::ErrorDetail,
    pub rev: i64,
    pub task: TaskDetail,
    pub since: Vec<TaskEvent>,
}

/// Body of `POST /v1/tasks/{num}/comments`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskCommentRequest {
    pub body: String,
}

/// Answer of `POST /v1/tasks/{num}/comments`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskCommentResponse {
    /// The comment's message.
    pub id: i64,
    /// The task's discussion thread.
    pub thread_id: i64,
    pub task: i64,
}

/// Answer of `GET /v1/tasks/{num}/events`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskEventsPage {
    pub events: Vec<TaskEvent>,
    /// Pass as `after_seq` for the next page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_after_seq: Option<i64>,
    /// Events deleted by earlier builds of M8, which kept at most 1,000
    /// per task; 0 otherwise. Events are no longer deleted.
    #[serde(default)]
    pub events_compacted: i64,
}

/// Answer of `GET /v1/tasks` and of `mine` and `next`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskList {
    pub tasks: Vec<Task>,
    /// `GET /v1/tasks`: pass as `before` for the next (older) page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_before: Option<i64>,
    /// `mine`, `next`: more tasks match than the page holds.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

/// One entry of `GET /v1/tasks/waiting`: a task of the caller's that waits,
/// and whom it waits for.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaitingItem {
    /// The caller's task.
    pub task: Task,
    /// The seat responsible: `waiting_on`, or the blocker's assignee (its
    /// reporter when unassigned).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seat: Option<String>,
    /// The unfinished blocker; absent for a task `blocked` on a seat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocker: Option<TaskRef>,
}

/// One entry of `GET /v1/tasks/owed`: something another seat waits for
/// from the caller.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OwedItem {
    /// A task with `waiting_on` the caller, or the caller's task that is
    /// the last open blocker of `blocks`.
    pub task: Task,
    /// The seat that waits: the waiting task's assignee (its reporter when
    /// unassigned).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seat: Option<String>,
    /// The task that waits for `task`; absent for `waiting_on`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocks: Option<TaskRef>,
}

/// Answer of `GET /v1/tasks/waiting`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaitingList {
    pub items: Vec<WaitingItem>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

/// Answer of `GET /v1/tasks/owed`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OwedList {
    pub items: Vec<OwedItem>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

/// One column of the board: the tasks in one shown state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardColumn {
    pub state: TaskState,
    /// Tasks in this state (all of them).
    pub count: i64,
    /// The first [`crate::limits::BOARD_COLUMN_TASKS`]: by priority, rank
    /// and age; the closed columns newest closed first.
    pub tasks: Vec<Task>,
}

/// One seat's load on the board.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeatLoad {
    pub seat: String,
    pub doing: i64,
    pub ready: i64,
    pub blocked: i64,
    /// `open` tasks with unfinished blockers.
    #[serde(default)]
    pub waiting: i64,
    #[serde(default)]
    pub review: i64,
}

/// An edge of a blocked chain: an unfinished task and its unfinished
/// blockers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardChain {
    pub num: i64,
    pub waits_on: Vec<i64>,
    /// The blockers in `waits_on` whose dependency needs an ok result.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub waits_on_ok: Vec<i64>,
}

/// Answer of `GET /v1/tasks/board`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskBoard {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    /// `ready`, `open`, `doing`, `blocked`, `review`, `done`, `cancelled`.
    pub columns: Vec<BoardColumn>,
    /// Per assignee, busiest first.
    pub load: Vec<SeatLoad>,
    /// `doing` or `review` tasks not changed for the org's stale period.
    pub stale: Vec<Task>,
    /// Unfinished tasks whose due time has passed.
    pub overdue: Vec<Task>,
    /// Unfinished tasks with unfinished blockers (at most 200).
    pub chains: Vec<BoardChain>,
}

/// Body of `POST /v1/tasks/import` (user token): the records of a JSON
/// Lines file, one object per task (see `docs/reference.md`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskImportRequest {
    pub records: Vec<serde_json::Value>,
    /// Check and report what would happen; change nothing.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dry_run: bool,
}

/// One record of an import file. Fields other than these are ignored with
/// a warning.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskImportRecord {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// A task state; `ready` is read as `open`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<TaskState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<TaskPriority>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reporter: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labels: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due: Option<String>,
    /// The `id` of the parent record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<TaskImportDep>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<TaskImportResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<TaskImportHistory>,
}

/// A dependency of an imported task.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskImportDep {
    /// The `id` of the blocker's record.
    pub id: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub needs_ok: bool,
}

/// The result of an imported task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskImportResult {
    pub value: TaskResult,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "ref")]
    pub ref_: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
}

/// One entry of an imported task's history.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskImportHistory {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    #[serde(default)]
    pub text: String,
}

/// What the import did with one record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskImportOutcome {
    /// The record's `id`.
    pub id: String,
    /// The task's number (with `dry_run`, absent for a task that would be
    /// created).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub num: Option<i64>,
    /// `created`, `updated` or `same`.
    pub action: String,
}

/// Answer of `POST /v1/tasks/import`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskImportResponse {
    /// One per record, in the file's order.
    pub results: Vec<TaskImportOutcome>,
    /// One line each: `ISSUE-42: no seat "w9"; the task is left
    /// unassigned`.
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dry_run: bool,
}

/// One task of a report section, with what the section says about it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportItem {
    pub task: Task,
    /// The seat the section groups by: the assignee; in `review`, the
    /// reporter, who reviews.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seat: Option<String>,
    /// `blocked`: the seats the task waits for.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub waiting_for: Vec<String>,
    /// `approved`: the unfinished tasks that depend on it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependents: Vec<TaskRef>,
}

/// One section of the lead's report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportSection {
    /// `not_started`, `in_progress`, `review`, `rejected`, `approved`,
    /// `parents`, `blocked`.
    pub name: String,
    /// Tasks in the section (all of them).
    pub count: i64,
    /// The first [`crate::limits::MAX_TASK_PAGE`].
    pub items: Vec<ReportItem>,
}

/// Answer of `GET /v1/tasks/report`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskReport {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    pub sections: Vec<ReportSection>,
}

/// Payload of a server-sent `task` event (user and seat tokens): a task of
/// the org changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskBusEvent {
    pub num: i64,
    /// `created`, `changed`, `assigned`, `dependency`, `link`, `commented`,
    /// `reminder`, `steal`.
    pub change: String,
    /// `changed`: the fields that changed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_state_round_trip() {
        for s in TaskState::ALL {
            assert_eq!(s.as_str().parse::<TaskState>().unwrap(), *s);
        }
        assert!(TaskState::Done.is_closed() && !TaskState::Ready.is_closed());
        assert!(TaskPriority::Urgent.order() < TaskPriority::Low.order());
    }

    #[test]
    fn patch_null_clears() {
        let p: TaskPatch = serde_json::from_str(r#"{"assignee":null,"title":"x"}"#).unwrap();
        assert_eq!(p.assignee, Some(None));
        assert_eq!(p.team, None);
    }
}
