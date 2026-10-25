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
    /// Numbers of the blockers that are not closed yet (`waits on t9,
    /// t11`), lowest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub waits_on: Vec<i64>,
    /// A pending request of this seat to take the task over (see
    /// `POST /tasks/{num}/steal`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steal_requested_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steal_requested_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<String>,
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
    /// The value before (`changed`, `assigned`). A body is not repeated:
    /// its events carry its size.
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
    /// Events in all, kept or compacted.
    #[serde(default)]
    pub event_count: i64,
    /// Old events that were deleted beyond
    /// [`crate::limits::MAX_TASK_EVENTS`] (counted only).
    #[serde(default)]
    pub events_compacted: i64,
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
}

/// Body of `POST /v1/tasks/{num}/deps`: the task `num` depends on
/// `blocked_by`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskDepRequest {
    pub blocked_by: i64,
}

/// Body of `POST /v1/tasks/{num}/links`: a thread, by its id or by one of
/// its messages.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskLinkRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<i64>,
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
    /// Old events deleted and counted only.
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
