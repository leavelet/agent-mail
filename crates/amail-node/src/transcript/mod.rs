//! Conversation mirroring: harness transcript parsers and a file follower.
//!
//! Each harness writes its session as a JSONL file (Claude Code: one record
//! per line under its projects directory; Codex: a "rollout" file). Neither
//! format is a stable contract, so the parsers here are deliberately
//! forgiving: lines that are not JSON are skipped and counted, records and
//! fields they do not know are ignored, and nothing panics.
//!
//! Every record that matters becomes a harness-independent
//! [`TranscriptEvent`]. Text that the mail system put into the session (it
//! starts with `[amail`) is marked as a `notice`, so the mirror can tell it
//! apart from what a person typed.

pub mod claude;
pub mod codex;
pub mod dsh;
pub mod tail;
pub mod usage;

use crate::adapter::Harness;
use amail_proto::{TranscriptEvent, TranscriptKind, TranscriptRole};
use serde_json::Value;

/// Longest `tool_call` / `tool_result` text kept, in characters.
pub const MAX_TOOL_CHARS: usize = 4000;
/// Longest `text` / `notice` text kept, in characters.
pub const MAX_TEXT_CHARS: usize = 16000;

/// `path` with symbolic links resolved: its nearest existing parent,
/// resolved, with the components that do not exist yet appended.
fn resolve(path: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut rest = Vec::new();
    let mut p = path;
    loop {
        match p.canonicalize() {
            Ok(mut c) => {
                c.extend(rest.iter().rev());
                return Some(c);
            }
            Err(_) => {
                rest.push(p.file_name()?);
                p = p.parent()?;
            }
        }
    }
}

/// Whether `path` names a `.jsonl` file below the directory `base`: an
/// absolute path without `.` or `..` components that lies under `base`
/// once symbolic links are resolved in both (a file or directory that does
/// not exist yet counts by its name). The node follows only transcripts in
/// the directories where the harnesses write them.
pub fn path_under(path: &std::path::Path, base: &std::path::Path) -> bool {
    use std::path::Component;
    let plain = |p: &std::path::Path| {
        p.is_absolute()
            && !p
                .components()
                .any(|c| matches!(c, Component::CurDir | Component::ParentDir))
    };
    if !plain(path) || !plain(base) || path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
        return false;
    }
    match (resolve(path), resolve(base)) {
        (Some(p), Some(b)) => p.starts_with(&b) && p != b,
        _ => false,
    }
}

/// Counters of one parser.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ParseStats {
    /// Non-empty lines seen.
    pub lines: u64,
    /// Lines that could not be parsed (not JSON, or not an object).
    pub skipped: u64,
    /// Records of a type the parser does not handle.
    pub ignored: u64,
}

/// A stateful transcript parser: feed it lines in file order.
pub trait Parser: Send {
    /// Parse one line (without its newline); returns the events it yields.
    fn line(&mut self, line: &str) -> Vec<TranscriptEvent>;
    fn stats(&self) -> ParseStats;
}

/// The parser for a harness's transcript format.
pub fn parser_for(h: Harness) -> Box<dyn Parser> {
    match h {
        Harness::Claude => Box::new(claude::ClaudeParser::default()),
        Harness::Codex => Box::new(codex::CodexParser::default()),
        Harness::Dsh => Box::new(dsh::DshParser::default()),
    }
}

/// The last `max` bytes of the file at `path` as whole lines (a line cut by
/// the start of that window is dropped), with its size and modification
/// time (nanoseconds) to tell whether it changed since.
pub fn read_tail(path: &std::path::Path, max: u64) -> std::io::Result<(String, (u64, i128))> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path)?;
    let meta = f.metadata()?;
    let size = meta.len();
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos() as i128);
    let start = size.saturating_sub(max);
    f.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::with_capacity((size - start) as usize);
    f.take(max).read_to_end(&mut buf)?;
    let mut text = String::from_utf8_lossy(&buf).into_owned();
    if start > 0 {
        text = match text.find('\n') {
            Some(i) => text[i + 1..].to_string(),
            None => String::new(),
        };
    }
    Ok((text, (size, mtime)))
}

/// Cut `s` to at most `max` characters; true if it was cut.
pub fn truncate(s: &str, max: usize) -> (String, bool) {
    match s.char_indices().nth(max) {
        None => (s.to_string(), false),
        Some((i, _)) => (s[..i].to_string(), true),
    }
}

/// Whether text was put into the session by the mail system.
pub fn is_notice(text: &str) -> bool {
    text.trim_start().starts_with("[amail")
}

/// Build an event, applying the length limit of its kind.
pub fn event(
    ts: Option<String>,
    role: TranscriptRole,
    kind: TranscriptKind,
    text: &str,
    tool: Option<String>,
) -> TranscriptEvent {
    let max = match kind {
        TranscriptKind::ToolCall | TranscriptKind::ToolResult => MAX_TOOL_CHARS,
        TranscriptKind::Text | TranscriptKind::Notice => MAX_TEXT_CHARS,
    };
    let (text, truncated) = truncate(text, max);
    TranscriptEvent {
        ts,
        role,
        kind,
        text,
        tool,
        truncated,
        error: false,
    }
}

/// A text event from a person or the model, or a notice if the mail system
/// wrote it.
pub fn text_event(ts: Option<String>, role: TranscriptRole, text: &str) -> TranscriptEvent {
    if is_notice(text) {
        event(ts, role, TranscriptKind::Notice, text, None)
    } else {
        event(ts, role, TranscriptKind::Text, text, None)
    }
}

/// Parse a line as a JSON object, counting failures.
pub(crate) fn parse_object(line: &str, stats: &mut ParseStats) -> Option<Value> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    stats.lines += 1;
    match serde_json::from_str::<Value>(line) {
        Ok(v) if v.is_object() => Some(v),
        _ => {
            stats.skipped += 1;
            None
        }
    }
}

pub(crate) fn str_at<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

/// A bounded map from tool-call id to tool name (oldest entries go first).
#[derive(Debug, Default)]
pub(crate) struct ToolNames {
    order: std::collections::VecDeque<String>,
    names: std::collections::HashMap<String, String>,
}

impl ToolNames {
    const CAP: usize = 512;

    pub(crate) fn insert(&mut self, id: &str, name: &str) {
        if self.names.contains_key(id) {
            return;
        }
        if self.order.len() >= Self::CAP
            && let Some(old) = self.order.pop_front()
        {
            self.names.remove(&old);
        }
        self.order.push_back(id.to_string());
        self.names.insert(id.to_string(), name.to_string());
    }

    pub(crate) fn get(&self, id: &str) -> Option<String> {
        self.names.get(id).cloned()
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.names.len()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn transcript_paths() {
        let d = tempfile::tempdir().unwrap();
        let base = d.path().join("projects");
        std::fs::create_dir_all(base.join("p")).unwrap();
        std::fs::write(base.join("p/s.jsonl"), "").unwrap();
        std::fs::write(d.path().join("outside.jsonl"), "").unwrap();
        std::os::unix::fs::symlink(d.path().join("outside.jsonl"), base.join("p/link.jsonl"))
            .unwrap();
        std::os::unix::fs::symlink(d.path(), base.join("up")).unwrap();
        let ok = |p: std::path::PathBuf| super::path_under(&p, &base);
        assert!(ok(base.join("p/s.jsonl")));
        // Not written yet, also in a directory not made yet.
        assert!(ok(base.join("p/new.jsonl")));
        assert!(ok(base.join("q/r/new.jsonl")));
        assert!(!ok(d.path().join("outside.jsonl")));
        assert!(!ok(base.join("p/link.jsonl")));
        assert!(!ok(base.join("up/outside.jsonl")));
        assert!(!ok(base.join("up/x/new.jsonl")));
        assert!(!ok(base.join("p/../../outside.jsonl")));
        assert!(!ok(base.join("p/s.txt")));
        assert!(!ok(std::path::PathBuf::from("projects/p/s.jsonl")));
        assert!(!ok(std::path::PathBuf::from("/etc/passwd")));
        // A transcript directory that does not exist yet.
        let missing = d.path().join("missing");
        assert!(super::path_under(&missing.join("a/s.jsonl"), &missing));
        assert!(!super::path_under(&base.join("p/s.jsonl"), &missing));
    }

    use super::*;

    #[test]
    fn truncate_is_char_safe() {
        assert_eq!(truncate("abc", 3), ("abc".into(), false));
        assert_eq!(truncate("abcd", 3), ("abc".into(), true));
        assert_eq!(truncate("数据库接口", 2), ("数据".into(), true));
        assert_eq!(truncate("", 0), ("".into(), false));
        let long = "é".repeat(MAX_TEXT_CHARS + 5);
        let e = event(
            None,
            TranscriptRole::Assistant,
            TranscriptKind::Text,
            &long,
            None,
        );
        assert!(e.truncated);
        assert_eq!(e.text.chars().count(), MAX_TEXT_CHARS);
        let e = event(
            None,
            TranscriptRole::Tool,
            TranscriptKind::ToolResult,
            &long,
            None,
        );
        assert_eq!(e.text.chars().count(), MAX_TOOL_CHARS);
    }

    #[test]
    fn notices() {
        assert!(is_notice("[amail] You have new mail (wake #3)."));
        assert!(is_notice("  [amail:seat=a launch=b] hello"));
        assert!(!is_notice("please check [amail] later"));
        let e = text_event(None, TranscriptRole::User, "[amail] 2 new messages");
        assert_eq!(e.kind, TranscriptKind::Notice);
        assert_eq!(e.role, TranscriptRole::User);
    }

    #[test]
    fn tool_names_bounded() {
        let mut t = ToolNames::default();
        for i in 0..2000 {
            t.insert(&format!("id{i}"), "Bash");
        }
        assert_eq!(t.len(), ToolNames::CAP);
        assert!(t.get("id0").is_none());
        assert_eq!(t.get("id1999").as_deref(), Some("Bash"));
    }
}
