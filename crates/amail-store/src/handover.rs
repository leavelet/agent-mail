//! Seat handover (M9): the handover note and the worklog, the SQL shared
//! by both backends as `tasks.rs` does it. [`impl_handover_queries!`]
//! generates `mod hq` for one sqlx database type; `impl_store_common!`
//! invokes it and implements the handover methods of [`crate::Store`] on
//! top of it.
//!
//! Note revisions and worklog entries are charged to the org's
//! `task_bytes` (the storage quota counts them as it counts task events).
//! A worklog append that would go over the quota first deletes the org's
//! oldest worklog entries to make room; a note over the quota is refused.
//!
//! Note writes of one seat are serialized by its `handover_seats` row
//! (`UPDATE ... note_rev = note_rev + 1`), which gives the revision number.

use amail_proto::{HandoverFields, HandoverSource, WorklogSource};

/// A note revision to write.
#[derive(Debug, Clone)]
pub struct NewNoteRec {
    pub org_id: i64,
    pub seat_id: i64,
    pub fields: HandoverFields,
    /// `None`: as the seat's previous note.
    pub private: Option<bool>,
    pub source: HandoverSource,
    /// The writing session (its worklog entries fill in `Pointers`).
    pub session: Option<String>,
    /// The user's name when the user writes for the seat.
    pub written_by: Option<String>,
    /// The org's storage quota (bytes); `None`: unlimited.
    pub quota: Option<i64>,
}

/// A stored note revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteRec {
    pub id: i64,
    pub org_id: i64,
    pub seat_id: i64,
    pub seat: String,
    pub rev: i64,
    pub fields: HandoverFields,
    pub pointers_filled: bool,
    pub private: bool,
    pub source: HandoverSource,
    pub session: Option<String>,
    pub written_by: Option<String>,
    pub written_at: i64,
    pub read_by_session: Option<String>,
    pub read_at: Option<i64>,
}

/// The handover state of a seat.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HandoverSeatRec {
    /// The latest revision (0: none).
    pub note_rev: i64,
    /// The team file's `handover = "private"`.
    pub private: bool,
    pub requested_at: Option<i64>,
    /// The name of the seat that asked.
    pub requested_by: Option<String>,
}

/// A worklog entry to append.
#[derive(Debug, Clone)]
pub struct NewWorklogEntry {
    pub seat_id: i64,
    /// `None` for server entries: the seat's last session the node
    /// reported.
    pub session: Option<String>,
    pub at: i64,
    pub kind: String,
    /// At most [`amail_proto::MAX_WORKLOG_TEXT_CHARS`] characters (cut).
    pub text: String,
    /// `None`: the seat's `doing` task changed last, if any.
    pub task: Option<i64>,
    pub doc: Option<i64>,
    pub source: WorklogSource,
}

/// A stored worklog entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorklogRec {
    pub id: i64,
    pub org_id: i64,
    pub seat_id: i64,
    pub seat: String,
    pub session: Option<String>,
    pub at: i64,
    pub kind: String,
    pub text: String,
    pub task: Option<i64>,
    pub doc: Option<i64>,
    pub source: WorklogSource,
}

/// A worklog query: one seat's entries, newest first.
#[derive(Debug, Clone, Default)]
pub struct WorklogQuery {
    pub org_id: i64,
    pub seat_id: i64,
    /// Entries at or after this time.
    pub since: Option<i64>,
    pub task: Option<i64>,
    pub kind: Option<String>,
    /// Entries with a smaller id (paging).
    pub before: Option<i64>,
    pub limit: i64,
}

/// Counts per kind, the tasks touched and the last entries of a query.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorklogSummaryRec {
    pub total: i64,
    /// (kind, count), most first.
    pub kinds: Vec<(String, i64)>,
    /// (task, count, last time), most recent first.
    pub tasks: Vec<(i64, i64, i64)>,
    pub last: Vec<WorklogRec>,
}

/// The `Pointers` field the server writes from the worklog.
pub(crate) fn pointers_text(tasks: &[i64], commit: Option<&str>, docs: &[i64]) -> String {
    let mut out = String::new();
    if !tasks.is_empty() {
        let t: Vec<String> = tasks.iter().map(|n| format!("t{n}")).collect();
        out.push_str(&format!("Tasks: {}\n", t.join(", ")));
    }
    if let Some(c) = commit {
        out.push_str(&format!("Last commit: {}\n", c.trim()));
    }
    if !docs.is_empty() {
        let d: Vec<String> = docs.iter().map(|n| format!("d{n}")).collect();
        out.push_str(&format!("Documents: {}\n", d.join(", ")));
    }
    if out.is_empty() {
        "nothing recorded in the worklog (filled in by the server)".to_string()
    } else {
        out.push_str("(filled in by the server from the worklog)");
        out
    }
}

/// Generates `mod hq`: the handover queries for one sqlx database type.
macro_rules! impl_handover_queries {
    ($db:ty) => {
        #[allow(dead_code)]
        pub(crate) mod hq {
            use super::tq;
            use $crate::handover::*;
            use $crate::tasks::refused;
            use $crate::{now_ms, Result, StoreError};
            use amail_proto::*;

            pub(crate) type Conn = <$db as sqlx::Database>::Connection;
            type Db = $db;

            const NOTE_COLS: &str = "n.id, n.org_id, n.seat_id, s.name AS seat, n.rev, n.state, n.summary, n.next, \
                 n.waiting_on, n.traps, n.verify, n.pointers, n.pointers_filled, n.private, n.source, n.session_id, \
                 n.written_by, n.written_at, n.read_by_session, n.read_at \
                 FROM handover_notes n JOIN seats s ON s.id = n.seat_id";
            const ENTRY_COLS: &str = "w.id, w.org_id, w.seat_id, s.name AS seat, w.session_id, w.at, w.kind, w.text, \
                 w.task_num, w.doc_num, w.source FROM worklog w JOIN seats s ON s.id = w.seat_id";

            #[derive(sqlx::FromRow)]
            struct NoteRow {
                id: i64,
                org_id: i64,
                seat_id: i64,
                seat: String,
                rev: i64,
                state: String,
                summary: String,
                next: String,
                waiting_on: String,
                traps: String,
                verify: String,
                pointers: String,
                pointers_filled: bool,
                private: bool,
                source: String,
                session_id: Option<String>,
                written_by: Option<String>,
                written_at: i64,
                read_by_session: Option<String>,
                read_at: Option<i64>,
            }

            impl NoteRow {
                fn into_rec(self) -> Result<NoteRec> {
                    Ok(NoteRec {
                        id: self.id,
                        org_id: self.org_id,
                        seat_id: self.seat_id,
                        seat: self.seat,
                        rev: self.rev,
                        fields: HandoverFields {
                            state: self.state,
                            summary: self.summary,
                            next: self.next,
                            waiting_on: self.waiting_on,
                            traps: self.traps,
                            verify: self.verify,
                            pointers: self.pointers,
                        },
                        pointers_filled: self.pointers_filled,
                        private: self.private,
                        source: $crate::common::parse(&self.source)?,
                        session: self.session_id,
                        written_by: self.written_by,
                        written_at: self.written_at,
                        read_by_session: self.read_by_session,
                        read_at: self.read_at,
                    })
                }
            }

            #[derive(sqlx::FromRow)]
            struct EntryRow {
                id: i64,
                org_id: i64,
                seat_id: i64,
                seat: String,
                session_id: Option<String>,
                at: i64,
                kind: String,
                text: String,
                task_num: Option<i64>,
                doc_num: Option<i64>,
                source: String,
            }

            impl EntryRow {
                fn into_rec(self) -> Result<WorklogRec> {
                    Ok(WorklogRec {
                        id: self.id,
                        org_id: self.org_id,
                        seat_id: self.seat_id,
                        seat: self.seat,
                        session: self.session_id,
                        at: self.at,
                        kind: self.kind,
                        text: self.text,
                        task: self.task_num,
                        doc: self.doc_num,
                        source: $crate::common::parse(&self.source)?,
                    })
                }
            }

            /// The seat is a seat of the org (deleted seats too: their
            /// history stays readable).
            async fn check_seat(c: &mut Conn, org_id: i64, seat_id: i64) -> Result<()> {
                seat_kind(c, org_id, seat_id).await.map(|_| ())
            }

            /// The kind of a seat of the org.
            async fn seat_kind(c: &mut Conn, org_id: i64, seat_id: i64) -> Result<String> {
                let k: Option<String> = sqlx::query_scalar("SELECT kind FROM seats WHERE id = $1 AND org_id = $2")
                    .bind(seat_id)
                    .bind(org_id)
                    .fetch_optional(&mut *c)
                    .await?;
                k.ok_or_else(|| StoreError::NotFound("seat".into()))
            }

            async fn ensure_seat_row(c: &mut Conn, org_id: i64, seat_id: i64) -> Result<()> {
                sqlx::query("INSERT INTO handover_seats (seat_id, org_id) VALUES ($1, $2) ON CONFLICT (seat_id) DO NOTHING")
                    .bind(seat_id)
                    .bind(org_id)
                    .execute(&mut *c)
                    .await?;
                Ok(())
            }

            pub async fn seat_state(c: &mut Conn, org_id: i64, seat_id: i64) -> Result<HandoverSeatRec> {
                let r: Option<(i64, bool, Option<i64>, Option<String>)> = sqlx::query_as(
                    "SELECT h.note_rev, h.private, h.requested_at, s.name FROM handover_seats h \
                     LEFT JOIN seats s ON s.id = h.requested_by WHERE h.seat_id = $1 AND h.org_id = $2",
                )
                .bind(seat_id)
                .bind(org_id)
                .fetch_optional(&mut *c)
                .await?;
                Ok(r.map(|(note_rev, private, requested_at, requested_by)| HandoverSeatRec {
                    note_rev,
                    private,
                    requested_at,
                    requested_by,
                })
                .unwrap_or_default())
            }

            pub async fn note(c: &mut Conn, org_id: i64, seat_id: i64, rev: Option<i64>) -> Result<Option<NoteRec>> {
                let r: Option<NoteRow> = match rev {
                    Some(rev) => sqlx::query_as(&format!("SELECT {NOTE_COLS} WHERE n.org_id = $1 AND n.seat_id = $2 AND n.rev = $3"))
                        .bind(org_id)
                        .bind(seat_id)
                        .bind(rev)
                        .fetch_optional(&mut *c)
                        .await?,
                    None => sqlx::query_as(&format!(
                        "SELECT {NOTE_COLS} WHERE n.org_id = $1 AND n.seat_id = $2 ORDER BY n.rev DESC LIMIT 1"
                    ))
                    .bind(org_id)
                    .bind(seat_id)
                    .fetch_optional(&mut *c)
                    .await?,
                };
                r.map(NoteRow::into_rec).transpose()
            }

            pub async fn notes(c: &mut Conn, org_id: i64, seat_id: i64, limit: i64) -> Result<Vec<NoteRec>> {
                let rows: Vec<NoteRow> = sqlx::query_as(&format!(
                    "SELECT {NOTE_COLS} WHERE n.org_id = $1 AND n.seat_id = $2 ORDER BY n.rev DESC LIMIT $3"
                ))
                .bind(org_id)
                .bind(seat_id)
                .bind(limit)
                .fetch_all(&mut *c)
                .await?;
                rows.into_iter().map(NoteRow::into_rec).collect()
            }

            /// The seat's last session that a node reported.
            async fn last_session(c: &mut Conn, seat_id: i64) -> Result<Option<String>> {
                Ok(sqlx::query_scalar(
                    "SELECT session_id FROM worklog WHERE seat_id = $1 AND source = 'node' \
                     AND session_id IS NOT NULL ORDER BY id DESC LIMIT 1",
                )
                .bind(seat_id)
                .fetch_optional(&mut *c)
                .await?)
            }

            /// `Pointers` from the worklog: tasks and documents the entries
            /// of `session` name (without a session: the entries at or after
            /// `since`), and the seat's last commit.
            pub async fn pointers(c: &mut Conn, seat_id: i64, session: Option<&str>, since: i64, also: Option<i64>) -> Result<String> {
                let scope = if session.is_some() { "session_id = $2" } else { "at >= $2" };
                let mut named: Vec<Vec<i64>> = Vec::new();
                for col in ["task_num", "doc_num"] {
                    let sql = format!(
                        "SELECT {col} FROM worklog WHERE seat_id = $1 AND {scope} AND {col} IS NOT NULL \
                         GROUP BY {col} ORDER BY MAX(id) DESC LIMIT {HANDOVER_POINTER_ITEMS}"
                    );
                    let q = sqlx::query_scalar::<Db, i64>(&sql).bind(seat_id);
                    named.push(match session {
                        Some(s) => q.bind(s.to_string()).fetch_all(&mut *c).await?,
                        None => q.bind(since).fetch_all(&mut *c).await?,
                    });
                }
                let mut tasks: Vec<i64> = also.into_iter().collect();
                for t in &named[0] {
                    if !tasks.contains(t) && tasks.len() < HANDOVER_POINTER_ITEMS {
                        tasks.push(*t);
                    }
                }
                let commit: Option<String> = sqlx::query_scalar(
                    "SELECT text FROM worklog WHERE seat_id = $1 AND kind = 'commit' ORDER BY id DESC LIMIT 1",
                )
                .bind(seat_id)
                .fetch_optional(&mut *c)
                .await?;
                Ok(pointers_text(&tasks, commit.as_deref(), &named[1]))
            }

            /// Write the next revision of the seat's note.
            pub async fn write_note(c: &mut Conn, n: &NewNoteRec, also_task: Option<i64>) -> Result<NoteRec> {
                check_seat(c, n.org_id, n.seat_id).await?;
                let mut f = n.fields.clone().trimmed();
                if n.source == HandoverSource::Agent {
                    let missing = f.missing();
                    if !missing.is_empty() {
                        return Err(refused(
                            "note_fields_missing",
                            format!("the note lacks {}", missing.join(", ")),
                        ));
                    }
                }
                let chars = f.chars();
                if chars > MAX_HANDOVER_NOTE_CHARS {
                    return Err(refused(
                        "note_too_large",
                        format!("the note has {chars} characters; the limit is {MAX_HANDOVER_NOTE_CHARS}"),
                    ));
                }
                ensure_seat_row(c, n.org_id, n.seat_id).await?;
                let rev: i64 = sqlx::query_scalar("UPDATE handover_seats SET note_rev = note_rev + 1 WHERE seat_id = $1 RETURNING note_rev")
                    .bind(n.seat_id)
                    .fetch_one(&mut *c)
                    .await?;
                let prev = note(c, n.org_id, n.seat_id, None).await?;
                let mut filled = false;
                if f.pointers.is_empty() {
                    let session = match &n.session {
                        Some(s) => Some(s.clone()),
                        None if n.source == HandoverSource::Server => last_session(c, n.seat_id).await?,
                        None => None,
                    };
                    let since = prev.as_ref().map(|p| p.written_at).unwrap_or(0);
                    let p = pointers(c, n.seat_id, session.as_deref(), since, also_task).await?;
                    // What fits under the cap.
                    let room = MAX_HANDOVER_NOTE_CHARS.saturating_sub(chars + 1);
                    f.pointers = cut_chars(&p, room);
                    filled = true;
                }
                let private = n.private.unwrap_or_else(|| prev.as_ref().is_some_and(|p| p.private));
                let now = now_ms();
                let bytes = f.render().len() as i64
                    + n.session.as_deref().map_or(0, str::len) as i64
                    + n.written_by.as_deref().map_or(0, str::len) as i64
                    + STORED_BYTES_PER_HANDOVER_NOTE;
                tq::charge(c, n.org_id, bytes, n.quota).await?;
                sqlx::query(
                    "INSERT INTO handover_notes (org_id, seat_id, rev, state, summary, next, waiting_on, traps, verify, \
                     pointers, pointers_filled, private, source, session_id, written_by, written_at, bytes) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)",
                )
                .bind(n.org_id)
                .bind(n.seat_id)
                .bind(rev)
                .bind(&f.state)
                .bind(&f.summary)
                .bind(&f.next)
                .bind(&f.waiting_on)
                .bind(&f.traps)
                .bind(&f.verify)
                .bind(&f.pointers)
                .bind(filled)
                .bind(private)
                .bind(n.source.as_str())
                .bind(&n.session)
                .bind(&n.written_by)
                .bind(now)
                .bind(bytes)
                .execute(&mut *c)
                .await?;
                note(c, n.org_id, n.seat_id, Some(rev))
                    .await?
                    .ok_or_else(|| StoreError::NotFound("note".into()))
            }

            /// The note the server writes when the seat's task `num` closed
            /// and the seat holds no other unfinished task; `None` when it
            /// holds one.
            pub async fn write_idle_note(c: &mut Conn, org_id: i64, seat_id: i64, num: i64, quota: Option<i64>) -> Result<Option<NoteRec>> {
                if seat_kind(c, org_id, seat_id).await? != "agent" {
                    return Ok(None);
                }
                let open: i64 = sqlx::query_scalar(
                    "SELECT COUNT(*) FROM tasks WHERE org_id = $1 AND assignee_id = $2 AND state NOT IN ('done', 'cancelled')",
                )
                .bind(org_id)
                .bind(seat_id)
                .fetch_one(&mut *c)
                .await?;
                if open > 0 {
                    return Ok(None);
                }
                let t: Option<(String, String, Option<String>, Option<String>, Option<String>, Option<i64>)> = sqlx::query_as(
                    "SELECT title, state, result, result_ref, result_note, closed_at FROM tasks WHERE org_id = $1 AND num = $2",
                )
                .bind(org_id)
                .bind(num)
                .fetch_optional(&mut *c)
                .await?;
                let Some((title, state, result, result_ref, result_note, closed_at)) = t else {
                    return Ok(None);
                };
                // The idle note replaces only a note older than the task's
                // closing, and never a note the agent (or the user for it)
                // wrote in the turn that closed the task or later. The turn
                // is the seat's last `turn_start` the node reported; without
                // one, an agent's note is kept.
                let prev = note(c, org_id, seat_id, None).await?;
                if let Some(p) = &prev {
                    let closed = closed_at.unwrap_or_else(now_ms);
                    if p.written_at >= closed {
                        return Ok(None);
                    }
                    if p.source == HandoverSource::Agent {
                        let turn: Option<i64> = sqlx::query_scalar(
                            "SELECT MAX(at) FROM worklog WHERE seat_id = $1 AND kind = 'turn_start' AND source = 'node'",
                        )
                        .bind(seat_id)
                        .fetch_one(&mut *c)
                        .await?;
                        if turn.is_none_or(|t| p.written_at >= t) {
                            return Ok(None);
                        }
                    }
                }
                // What the agent recorded as traps stays in the note.
                let traps = match prev.as_ref().filter(|p| p.source == HandoverSource::Agent) {
                    Some(p) if !p.fields.traps.trim().is_empty() => format!(
                        "From the seat's note rev {} (written by the agent before t{num} closed):\n{}",
                        p.rev,
                        p.fields.traps.trim()
                    ),
                    _ => "none recorded (this note was written by the server, not by the agent)".into(),
                };
                let mut summary = format!("t{num} ({title}) is {state}");
                if let Some(r) = &result {
                    summary.push_str(&format!(" with result {r}"));
                    if let Some(rf) = &result_ref {
                        summary.push_str(&format!(" for {rf}"));
                    }
                }
                summary.push('.');
                if let Some(nt) = result_note.as_deref().filter(|s| !s.trim().is_empty()) {
                    summary.push(' ');
                    summary.push_str(nt.trim());
                }
                summary.push_str(" The seat holds no other unfinished task.");
                // The agent's own summary, verification path and pointers
                // stay readable in the idle note: they are what the
                // successor needs when the seat is given work again.
                let agent = prev.as_ref().filter(|p| p.source == HandoverSource::Agent);
                let mut verify = format!("amail task show t{num}");
                let mut pointers = String::new();
                if let Some(p) = agent {
                    if !p.fields.summary.trim().is_empty() {
                        summary.push_str(&format!(
                            "\nEarlier summary (rev {}, before t{num} closed): {}",
                            p.rev,
                            p.fields.summary.trim()
                        ));
                    }
                    if !p.fields.verify.trim().is_empty() {
                        verify.push_str(&format!("\nFrom rev {}: {}", p.rev, p.fields.verify.trim()));
                    }
                    pointers = p.fields.pointers.trim().to_string();
                }
                let fields = HandoverFields {
                    state: format!("idle: t{num} closed, no other work"),
                    summary,
                    next: "Nothing assigned: read new mail, then `amail task next` for work that is ready.".into(),
                    waiting_on: "nothing".into(),
                    traps,
                    verify,
                    pointers,
                };
                let n = NewNoteRec {
                    org_id,
                    seat_id,
                    fields,
                    private: None,
                    source: HandoverSource::Server,
                    session: None,
                    written_by: None,
                    quota,
                };
                write_note(c, &n, Some(num)).await.map(Some)
            }

            pub async fn record_read(c: &mut Conn, org_id: i64, seat_id: i64, rev: Option<i64>, session: &str) -> Result<Option<NoteRec>> {
                let Some(n) = note(c, org_id, seat_id, rev).await? else {
                    return Ok(None);
                };
                sqlx::query("UPDATE handover_notes SET read_by_session = $1, read_at = $2 WHERE id = $3")
                    .bind(session)
                    .bind(now_ms())
                    .bind(n.id)
                    .execute(&mut *c)
                    .await?;
                note(c, org_id, seat_id, Some(n.rev)).await
            }

            pub async fn set_private(c: &mut Conn, org_id: i64, seat_id: i64, private: bool) -> Result<()> {
                check_seat(c, org_id, seat_id).await?;
                ensure_seat_row(c, org_id, seat_id).await?;
                sqlx::query("UPDATE handover_seats SET private = $1 WHERE seat_id = $2")
                    .bind(private)
                    .bind(seat_id)
                    .execute(&mut *c)
                    .await?;
                Ok(())
            }

            pub async fn request(c: &mut Conn, org_id: i64, seat_id: i64, by: i64, at: i64) -> Result<HandoverSeatRec> {
                check_seat(c, org_id, seat_id).await?;
                ensure_seat_row(c, org_id, seat_id).await?;
                sqlx::query("UPDATE handover_seats SET requested_at = $1, requested_by = $2 WHERE seat_id = $3")
                    .bind(at)
                    .bind(by)
                    .bind(seat_id)
                    .execute(&mut *c)
                    .await?;
                seat_state(c, org_id, seat_id).await
            }

            /// Delete the org's oldest worklog entries holding at least
            /// `need` bytes (refunded); false when all of them hold less.
            async fn evict(c: &mut Conn, org_id: i64, need: i64) -> Result<bool> {
                let mut freed = 0;
                while freed < need {
                    let rows: Vec<(i64, i64)> = sqlx::query_as(
                        "SELECT id, bytes FROM worklog WHERE org_id = $1 ORDER BY id LIMIT 1000",
                    )
                    .bind(org_id)
                    .fetch_all(&mut *c)
                    .await?;
                    if rows.is_empty() {
                        return Ok(false);
                    }
                    let mut last = 0;
                    let mut sum = 0;
                    for (id, b) in &rows {
                        last = *id;
                        sum += b;
                        if freed + sum >= need {
                            break;
                        }
                    }
                    sqlx::query("DELETE FROM worklog WHERE org_id = $1 AND id <= $2")
                        .bind(org_id)
                        .bind(last)
                        .execute(&mut *c)
                        .await?;
                    tq::charge(c, org_id, -sum, None).await?;
                    freed += sum;
                }
                Ok(true)
            }

            /// Append entries (of agent seats of the org; others are left out);
            /// the number written.
            pub async fn append(c: &mut Conn, org_id: i64, entries: &[NewWorklogEntry], quota: Option<i64>) -> Result<usize> {
                if entries.is_empty() {
                    return Ok(0);
                }
                let mut bytes = Vec::with_capacity(entries.len());
                for e in entries {
                    let text = cut_chars(&e.text, MAX_WORKLOG_TEXT_CHARS);
                    bytes.push(
                        (text.len() + e.kind.len() + e.session.as_deref().map_or(0, str::len)) as i64
                            + STORED_BYTES_PER_WORKLOG_ENTRY,
                    );
                }
                let total: i64 = bytes.iter().sum();
                match tq::charge(c, org_id, total, quota).await {
                    Err(StoreError::StorageQuota(stored)) => {
                        if !evict(c, org_id, total).await? {
                            return Err(StoreError::StorageQuota(stored));
                        }
                        tq::charge(c, org_id, total, quota).await?;
                    }
                    r => r?,
                }
                let seats: std::collections::HashSet<i64> = {
                    let mut ids: Vec<i64> = entries.iter().map(|e| e.seat_id).collect();
                    ids.sort_unstable();
                    ids.dedup();
                    let mut ok = std::collections::HashSet::new();
                    for id in ids {
                        // Agent seats only: the user and the system seat
                        // have no sessions to hand over.
                        if seat_kind(c, org_id, id).await.ok().as_deref() == Some("agent") {
                            ok.insert(id);
                        }
                    }
                    ok
                };
                let mut n = 0;
                let mut refund = 0;
                for (e, b) in entries.iter().zip(bytes) {
                    if !seats.contains(&e.seat_id) {
                        refund += b;
                        continue;
                    }
                    let session_sql = match e.source {
                        WorklogSource::Server => "COALESCE($3, (SELECT w.session_id FROM worklog w WHERE w.seat_id = $2 \
                             AND w.source = 'node' AND w.session_id IS NOT NULL ORDER BY w.id DESC LIMIT 1))",
                        WorklogSource::Node => "$3",
                    };
                    let sql = format!(
                        "INSERT INTO worklog (org_id, seat_id, session_id, at, kind, text, task_num, doc_num, source, bytes) \
                         VALUES ($1, $2, {session_sql}, $4, $5, $6, COALESCE($7, (SELECT t.num FROM tasks t \
                         WHERE t.assignee_id = $2 AND t.state = 'doing' ORDER BY t.updated_at DESC LIMIT 1)), $8, $9, $10)"
                    );
                    sqlx::query(&sql)
                        .bind(org_id)
                        .bind(e.seat_id)
                        .bind(&e.session)
                        .bind(e.at)
                        .bind(&e.kind)
                        .bind(cut_chars(&e.text, MAX_WORKLOG_TEXT_CHARS))
                        .bind(e.task)
                        .bind(e.doc)
                        .bind(e.source.as_str())
                        .bind(b)
                        .execute(&mut *c)
                        .await?;
                    n += 1;
                }
                if refund > 0 {
                    tq::charge(c, org_id, -refund, None).await?;
                }
                Ok(n)
            }

            /// The `WHERE` of a query, from `$1`, and its values.
            fn filter(q: &WorklogQuery) -> (String, Vec<i64>, Option<String>) {
                let mut sql = String::from("w.org_id = $1 AND w.seat_id = $2");
                let mut ints = vec![q.org_id, q.seat_id];
                for (col, v) in [("w.at >=", q.since), ("w.task_num =", q.task), ("w.id <", q.before)] {
                    if let Some(v) = v {
                        ints.push(v);
                        sql.push_str(&format!(" AND {col} ${}", ints.len()));
                    }
                }
                if q.kind.is_some() {
                    sql.push_str(&format!(" AND w.kind = ${}", ints.len() + 1));
                }
                (sql, ints, q.kind.clone())
            }

            macro_rules! bind_filter {
                ($q:expr, $ints:expr, $kind:expr) => {{
                    let mut q = $q;
                    for v in &$ints {
                        q = q.bind(*v);
                    }
                    if let Some(k) = &$kind {
                        q = q.bind(k.clone());
                    }
                    q
                }};
            }

            pub async fn query(c: &mut Conn, q: &WorklogQuery) -> Result<Vec<WorklogRec>> {
                let (cond, ints, kind) = filter(q);
                let sql = format!("SELECT {ENTRY_COLS} WHERE {cond} ORDER BY w.id DESC LIMIT {}", q.limit.max(0));
                let rows: Vec<EntryRow> = bind_filter!(sqlx::query_as::<Db, EntryRow>(&sql), ints, kind)
                    .fetch_all(&mut *c)
                    .await?;
                rows.into_iter().map(EntryRow::into_rec).collect()
            }

            pub async fn summary(c: &mut Conn, q: &WorklogQuery) -> Result<WorklogSummaryRec> {
                let (cond, ints, kind) = filter(q);
                let sql = format!(
                    "SELECT w.kind, CAST(COUNT(*) AS BIGINT) FROM worklog w WHERE {cond} GROUP BY w.kind ORDER BY 2 DESC, 1"
                );
                let kinds: Vec<(String, i64)> = bind_filter!(sqlx::query_as::<Db, (String, i64)>(&sql), ints, kind)
                    .fetch_all(&mut *c)
                    .await?;
                let sql = format!(
                    "SELECT w.task_num, CAST(COUNT(*) AS BIGINT), MAX(w.at) FROM worklog w WHERE {cond} \
                     AND w.task_num IS NOT NULL GROUP BY w.task_num ORDER BY 3 DESC LIMIT 100"
                );
                let tasks: Vec<(i64, i64, i64)> = bind_filter!(sqlx::query_as::<Db, (i64, i64, i64)>(&sql), ints, kind)
                    .fetch_all(&mut *c)
                    .await?;
                let last = query(c, &WorklogQuery { limit: WORKLOG_SUMMARY_LAST, ..q.clone() }).await?;
                Ok(WorklogSummaryRec {
                    total: kinds.iter().map(|k| k.1).sum(),
                    kinds,
                    tasks,
                    last,
                })
            }

            /// Delete entries older than `cutoff` (at most `limit`), refunding
            /// their bytes; the number deleted.
            pub async fn expire(c: &mut Conn, cutoff: i64, limit: i64) -> Result<u64> {
                let rows: Vec<(i64, i64, i64)> = sqlx::query_as(
                    "SELECT id, org_id, bytes FROM worklog WHERE at < $1 ORDER BY at LIMIT $2",
                )
                .bind(cutoff)
                .bind(limit)
                .fetch_all(&mut *c)
                .await?;
                let mut per_org: std::collections::BTreeMap<i64, i64> = Default::default();
                for chunk in rows.chunks(500) {
                    let sql = format!(
                        "DELETE FROM worklog WHERE id IN ({})",
                        $crate::common::placeholders(1, chunk.len())
                    );
                    let mut q = sqlx::query::<Db>(&sql);
                    for (id, org, b) in chunk {
                        q = q.bind(*id);
                        *per_org.entry(*org).or_default() += b;
                    }
                    q.execute(&mut *c).await?;
                }
                for (org, b) in per_org {
                    sqlx::query("UPDATE orgs SET task_bytes = task_bytes - $1 WHERE id = $2")
                        .bind(b)
                        .bind(org)
                        .execute(&mut *c)
                        .await?;
                }
                Ok(rows.len() as u64)
            }

            /// Delete the org's handover rows (the org is being deleted).
            pub async fn delete_org(c: &mut Conn, org_id: i64) -> Result<()> {
                for sql in [
                    "DELETE FROM worklog WHERE org_id = $1",
                    "DELETE FROM handover_notes WHERE org_id = $1",
                    "DELETE FROM handover_seats WHERE org_id = $1",
                ] {
                    sqlx::query(sql).bind(org_id).execute(&mut *c).await?;
                }
                Ok(())
            }
        }
    };
}
