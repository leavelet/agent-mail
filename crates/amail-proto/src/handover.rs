//! Wire types of the seat handover (M9): the handover note
//! (`/v1/seats/{name}/handover…`) and the worklog
//! (`/v1/seats/{name}/worklog`, `POST /v1/worklog`).
//!
//! The note has seven labelled fields in a fixed order
//! ([`HANDOVER_FIELDS`]). It is written as text, with each field as a
//! Markdown heading (`## State`) or a `Field:` line, and parsed by
//! [`parse_handover`]; or as the seven fields in JSON.

use super::{Deserialize, FromStr, Serialize, fmt};

/// Characters of a stored note (the text [`HandoverFields::render`]
/// gives); a longer note is refused with 422 `note_too_large`.
pub const MAX_HANDOVER_NOTE_CHARS: usize = 16_384;
/// Characters of a worklog entry's text; longer text is cut.
pub const MAX_WORKLOG_TEXT_CHARS: usize = 1_000;
/// Characters of a worklog entry's kind (`a-z`, `0-9`, `_`, `-`, `.`).
pub const MAX_WORKLOG_KIND_CHARS: usize = 40;
/// Characters of a session id (worklog entries, notes, read
/// acknowledgements).
pub const MAX_SESSION_ID_CHARS: usize = 200;
/// Entries per page of `GET /v1/seats/{name}/worklog`.
pub const MAX_WORKLOG_PAGE: i64 = 1_000;
/// Entries listed by the worklog summary.
pub const WORKLOG_SUMMARY_LAST: i64 = 10;
/// Revisions listed with a note.
pub const HANDOVER_REVISIONS_SHOWN: i64 = 20;
/// Largest body of `POST /v1/worklog`.
pub const MAX_WORKLOG_POST_BYTES: usize = 8 * 1024 * 1024;
/// Bytes counted against the storage quota for each note revision and
/// each worklog entry, on top of their texts.
pub const STORED_BYTES_PER_HANDOVER_NOTE: i64 = 128;
pub const STORED_BYTES_PER_WORKLOG_ENTRY: i64 = 64;
/// A note older than this many days is injected with a warning line.
pub const HANDOVER_STALE_DAYS: i64 = 7;
/// Tasks and documents named in the `Pointers` the server fills in.
pub const HANDOVER_POINTER_ITEMS: usize = 10;

/// The labels of the note's fields, in the order the note shows them.
pub const HANDOVER_FIELDS: [&str; 7] = [
    "State",
    "Summary",
    "Next",
    "Waiting on",
    "Traps",
    "Verify",
    "Pointers",
];

/// The fields the node shows whole at session start; `Traps` and
/// `Pointers` are cut first.
pub const HANDOVER_KEPT_FIELDS: [usize; 5] = [0, 1, 2, 3, 5];

string_enum!(
    /// Who wrote a note revision: the seat's agent (or the user for it),
    /// the node from a compaction summary (a draft), or the server (the
    /// idle note after the seat's last task closed).
    HandoverSource { Agent => "agent", Compaction => "compaction", Server => "server" }
);

string_enum!(
    /// Who wrote a worklog entry.
    WorklogSource { Server => "server", Node => "node" }
);

/// The seven fields of a handover note.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HandoverFields {
    pub state: String,
    pub summary: String,
    pub next: String,
    pub waiting_on: String,
    pub traps: String,
    pub verify: String,
    pub pointers: String,
}

impl HandoverFields {
    /// The fields in the order of [`HANDOVER_FIELDS`].
    pub fn values(&self) -> [&str; 7] {
        [
            &self.state,
            &self.summary,
            &self.next,
            &self.waiting_on,
            &self.traps,
            &self.verify,
            &self.pointers,
        ]
    }

    fn slot(&mut self, i: usize) -> &mut String {
        match i {
            0 => &mut self.state,
            1 => &mut self.summary,
            2 => &mut self.next,
            3 => &mut self.waiting_on,
            4 => &mut self.traps,
            5 => &mut self.verify,
            _ => &mut self.pointers,
        }
    }

    /// Set field `i` (an index of [`HANDOVER_FIELDS`]).
    pub fn set(&mut self, i: usize, v: impl Into<String>) {
        *self.slot(i) = v.into();
    }

    /// Trim each field.
    pub fn trimmed(mut self) -> Self {
        for i in 0..7 {
            let t = self.slot(i).trim().to_string();
            *self.slot(i) = t;
        }
        self
    }

    /// The labels of the empty fields, in order. `Pointers` is never
    /// missing: the server fills it in from the worklog when it is empty.
    pub fn missing(&self) -> Vec<&'static str> {
        self.values()
            .iter()
            .enumerate()
            .take(6)
            .filter(|(_, v)| v.trim().is_empty())
            .map(|(i, _)| HANDOVER_FIELDS[i])
            .collect()
    }

    /// The note as text: each field as `Label: value` when the value is
    /// one line, else `Label:` followed by its lines; a blank line
    /// between fields. [`parse_handover`] reads it back.
    pub fn render(&self) -> String {
        let mut out = String::new();
        for (i, v) in self.values().iter().enumerate() {
            render_field(&mut out, i, v);
        }
        out
    }

    /// Characters of [`HandoverFields::render`].
    pub fn chars(&self) -> usize {
        self.render().chars().count()
    }

    /// The note as text within `max` characters, for injection at session
    /// start: `State`, `Summary`, `Next`, `Waiting on` and `Verify` whole,
    /// then as much of `Traps` and `Pointers` as fits, in the note's
    /// order. When something was left out, the text ends with
    /// `… the rest: amail handover show` (true in the answer).
    pub fn render_within(&self, max: usize) -> (String, bool) {
        let full = self.render();
        if full.chars().count() <= max {
            return (full, false);
        }
        const MORE: &str = "… the rest: amail handover show\n";
        let vals = self.values();
        let kept: usize = HANDOVER_KEPT_FIELDS
            .iter()
            .map(|i| {
                let mut s = String::new();
                render_field(&mut s, *i, vals[*i]);
                // With the blank line before it.
                s.chars().count() + 1
            })
            .sum();
        // Room left for Traps and Pointers.
        let mut room = max.saturating_sub(kept + MORE.chars().count());
        let mut out = String::new();
        for (i, v) in vals.iter().enumerate() {
            if HANDOVER_KEPT_FIELDS.contains(&i) {
                render_field(&mut out, i, v);
                continue;
            }
            let mut s = String::new();
            render_field(&mut s, i, v);
            let n = s.chars().count() + 1;
            if n <= room {
                render_field(&mut out, i, v);
                room -= n;
            } else if room > HANDOVER_FIELDS[i].len() + 8 {
                // A cut field: whole lines while they fit.
                let mut part = String::new();
                // The blank line before it and the one after a cut.
                let mut used = 2;
                for line in s.lines() {
                    let l = line.chars().count() + 1;
                    if used + l > room {
                        break;
                    }
                    part.push_str(line);
                    part.push('\n');
                    used += l;
                }
                if !part.is_empty() {
                    out.push('\n');
                    out.push_str(&part);
                }
                room = 0;
            }
        }
        out.push_str(MORE);
        (out, true)
    }
}

fn render_field(out: &mut String, i: usize, v: &str) {
    if !out.is_empty() {
        out.push('\n');
    }
    let v = v.trim();
    if v.contains('\n') {
        out.push_str(HANDOVER_FIELDS[i]);
        out.push_str(":\n");
        out.push_str(v);
        out.push('\n');
    } else {
        out.push_str(HANDOVER_FIELDS[i]);
        out.push(':');
        if !v.is_empty() {
            out.push(' ');
            out.push_str(v);
        }
        out.push('\n');
    }
}

/// The index of the field a label names (case, `_`, `-` and surrounding
/// `*` ignored).
pub fn handover_field_index(label: &str) -> Option<usize> {
    let l = label.trim().trim_matches('*').trim().trim_end_matches(':');
    if l.len() > 20 {
        return None;
    }
    let l: String = l
        .to_ascii_lowercase()
        .replace(['_', '-'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    match l.as_str() {
        "state" => Some(0),
        "summary" => Some(1),
        "next" | "next steps" => Some(2),
        "waiting on" => Some(3),
        "traps" => Some(4),
        "verify" => Some(5),
        "pointers" => Some(6),
        _ => None,
    }
}

/// A line that starts a field: a Markdown heading naming it (`## State`,
/// also `## State: value`) or `State: value` (also `**State:** value`).
fn label_line(line: &str) -> Option<(usize, &str)> {
    let t = line.trim_start();
    if t.starts_with('#') {
        let h = t.trim_start_matches('#');
        let hashes = t.len() - h.len();
        if hashes > 6 || !h.starts_with([' ', '\t']) {
            return None;
        }
        let h = h.trim();
        if let Some(i) = handover_field_index(h) {
            return Some((i, ""));
        }
        let (label, rest) = h.split_once(':')?;
        return handover_field_index(label).map(|i| (i, rest.trim()));
    }
    let (label, rest) = t.split_once(':')?;
    let i = handover_field_index(label)?;
    Some((i, rest.trim_start_matches('*').trim()))
}

/// Read a note written as text: each field starts at a Markdown heading
/// naming it or at a `Field:` line, and runs to the next field. A field
/// is recognized once: a later line with the same label is part of the
/// field it is in. Text before the first field is left out. Missing
/// fields are empty (see [`HandoverFields::missing`]).
pub fn parse_handover(text: &str) -> HandoverFields {
    let mut f = HandoverFields::default();
    let mut seen = [false; 7];
    let mut cur: Option<usize> = None;
    let mut buf = String::new();
    for line in text.lines() {
        if let Some((i, rest)) = label_line(line).filter(|(i, _)| !seen[*i]) {
            if let Some(c) = cur {
                f.set(c, buf.trim());
            }
            seen[i] = true;
            cur = Some(i);
            buf = rest.to_string();
            buf.push('\n');
        } else if cur.is_some() {
            buf.push_str(line);
            buf.push('\n');
        }
    }
    if let Some(c) = cur {
        f.set(c, buf.trim());
    }
    f
}

/// The template `amail handover template` prints.
pub const HANDOVER_TEMPLATE: &str = "\
State: on track | blocked | environment broken | idle, with a few words

Summary:
The current work in two or three sentences.

Next:
1. The exact command to run or file to open first.
2. ...

Waiting on: who, for what, since when (or: nothing)

Traps:
Rejected approaches with the reason, constraints learned, files not to
touch, flaky tests.

Verify:
A command that confirms the state, and its expected output.

Pointers:
Task ids, document ids, message ids, commit hashes; no pasted content.
Left empty, the server fills it in from the worklog.
";

/// Body of `PUT /v1/seats/{name}/handover`: the note as `text` or as
/// `fields`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HandoverWrite {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fields: Option<HandoverFields>,
    /// Limit the note to the seat, its lead and the user. Not given: as
    /// the seat's previous note.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub private: Option<bool>,
    /// The writing session's id (its worklog entries fill in `Pointers`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// `agent` (default) or `compaction`: a draft from the harness's
    /// compaction summary, given as `text`; its fields are not checked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<HandoverSource>,
}

/// A note revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoverNote {
    pub seat: String,
    pub rev: i64,
    pub fields: HandoverFields,
    /// The fields as text ([`HandoverFields::render`]).
    pub text: String,
    pub chars: usize,
    /// `Pointers` was filled in by the server from the worklog.
    #[serde(default)]
    pub pointers_filled: bool,
    pub private: bool,
    pub source: HandoverSource,
    /// A draft from a compaction summary (`source` = `compaction`).
    #[serde(default)]
    pub draft: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// The user's name when the user wrote the note for the seat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub written_by: Option<String>,
    pub written_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_by_session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_at: Option<String>,
}

/// One revision in a note's history (without its fields).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoverRevision {
    pub rev: i64,
    pub written_at: String,
    pub source: HandoverSource,
    pub private: bool,
    pub chars: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub written_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_by_session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_at: Option<String>,
}

/// Answer of `GET` and `PUT /v1/seats/{name}/handover`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoverResponse {
    pub seat: String,
    /// The current note, or with `?rev=` that revision; none before the
    /// seat's first note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<HandoverNote>,
    /// The latest revision (0: none).
    pub current_rev: i64,
    /// The newest revisions, newest first (at most
    /// [`HANDOVER_REVISIONS_SHOWN`]).
    #[serde(default)]
    pub revisions: Vec<HandoverRevision>,
    /// The seat's notes are private (the current note's flag or the team
    /// file's `handover = "private"`).
    pub private: bool,
    /// A request for a note (by the lead or the user) not answered by a
    /// newer note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_by: Option<String>,
}

/// Answer of `POST /v1/seats/{name}/handover/request`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoverRequested {
    pub seat: String,
    pub requested_at: String,
    /// The system mail sent to the seat (none when it could not be sent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<i64>,
}

/// Body of `POST /v1/seats/{name}/handover/read`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HandoverRead {
    /// The session that was shown the note.
    pub session: String,
    /// The revision shown (default: the current one).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rev: Option<i64>,
}

/// Answer of `POST /v1/seats/{name}/handover/read`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoverReadResponse {
    pub seat: String,
    pub rev: i64,
    pub read_by_session: String,
    pub read_at: String,
}

/// A worklog entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorklogEntry {
    pub id: i64,
    pub seat: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    pub at: String,
    pub kind: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<i64>,
    pub source: WorklogSource,
}

/// Answer of `GET /v1/seats/{name}/worklog`: newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorklogPage {
    pub seat: String,
    pub entries: Vec<WorklogEntry>,
    /// Pass as `before` for the next page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_before: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorklogKindCount {
    pub kind: String,
    pub count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorklogTaskCount {
    pub task: i64,
    pub count: i64,
    /// When the seat last did something about the task.
    pub last_at: String,
}

/// Answer of `GET /v1/seats/{name}/worklog?summary=1`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorklogSummary {
    pub seat: String,
    pub total: i64,
    /// Entries per kind, most first.
    pub kinds: Vec<WorklogKindCount>,
    /// Tasks the entries name, most recent first.
    pub tasks: Vec<WorklogTaskCount>,
    /// The last [`WORKLOG_SUMMARY_LAST`] entries, newest first.
    pub last: Vec<WorklogEntry>,
}

/// One entry of `POST /v1/worklog`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WorklogPostEntry {
    pub seat: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// RFC 3339; default: when the server receives it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    pub kind: String,
    pub text: String,
    /// A task number (`12`) of the org.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<i64>,
    /// A document number of the org.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc: Option<i64>,
}

/// Body of `POST /v1/worklog` (node tokens).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WorklogPost {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    pub entries: Vec<WorklogPostEntry>,
}

/// Answer of `POST /v1/worklog`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorklogPosted {
    pub accepted: usize,
    /// Entries left out: unknown seats, seats that are not agent seats,
    /// unreadable kinds or times.
    pub skipped: usize,
}

/// A worklog kind: 1 to [`MAX_WORKLOG_KIND_CHARS`] characters of `a-z`,
/// `0-9`, `_`, `-` and `.`.
pub fn valid_worklog_kind(k: &str) -> bool {
    !k.is_empty()
        && k.len() <= MAX_WORKLOG_KIND_CHARS
        && k.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-' | b'.')
        })
}

/// `s` cut to `max` characters.
pub fn cut_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => s[..i].to_string(),
        None => s.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_headings_and_lines() {
        let f = parse_handover(
            "# Handover\n\n## State\non track\n\n## Summary\nDoing x.\nAnd y.\n\n### Next\n1. cargo test\n\
             Waiting on: nothing\n**Traps:** do not touch a.rs\nNext: not a new field\n## verify\n`make` prints ok\n",
        );
        assert_eq!(f.state, "on track");
        assert_eq!(f.summary, "Doing x.\nAnd y.");
        assert_eq!(f.next, "1. cargo test");
        assert_eq!(f.waiting_on, "nothing");
        assert_eq!(f.traps, "do not touch a.rs\nNext: not a new field");
        assert_eq!(f.verify, "`make` prints ok");
        assert_eq!(f.pointers, "");
        assert!(f.missing().is_empty());
        assert_eq!(parse_handover(&f.render()), f);
    }

    #[test]
    fn missing_fields_in_order() {
        let f = parse_handover("Summary: x\nVerify: y\n");
        assert_eq!(f.missing(), vec!["State", "Next", "Waiting on", "Traps"]);
        assert!(parse_handover("").missing().len() == 6);
    }

    #[test]
    fn within_budget() {
        let f = HandoverFields {
            state: "on track".into(),
            summary: "s".into(),
            next: "n".into(),
            waiting_on: "nothing".into(),
            traps: (0..200).map(|i| format!("trap {i}\n")).collect(),
            verify: "v".into(),
            pointers: "t1".into(),
        };
        let (t, cut) = f.render_within(400);
        assert!(cut);
        assert!(t.chars().count() <= 400, "{}", t.chars().count());
        assert!(t.contains("State: on track") && t.contains("Verify: v"));
        assert!(t.ends_with("… the rest: amail handover show\n"));
        let (t, cut) = f.render_within(100_000);
        assert!(!cut);
        assert_eq!(t, f.render());
    }

    #[test]
    fn kinds() {
        assert!(valid_worklog_kind("tool"));
        assert!(valid_worklog_kind("session.start"));
        assert!(!valid_worklog_kind("Tool"));
        assert!(!valid_worklog_kind(""));
        assert_eq!(cut_chars("äbc", 2), "äb");
    }
}
