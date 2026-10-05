//! Claude Code session transcripts (`<config>/projects/<dir>/<session>.jsonl`).
//!
//! Records used (everything else is ignored):
//!
//! - `user` with `message.content` a string (a prompt) or blocks: `text`,
//!   `image`, `tool_result` (`tool_use_id`, `content` string or blocks,
//!   `is_error`).
//! - `assistant` with blocks `text` and `tool_use` (`id`, `name`, `input`);
//!   `thinking` blocks are skipped.
//! - `attachment` with `attachment.type` `hook_additional_context`
//!   (`content`: text injected by a hook, i.e. mail) or `queued_command`
//!   (`prompt`: a message that arrived mid-turn).
//!
//! Records of hidden sub-agents (`isSidechain`) and meta records (`isMeta`)
//! are skipped.

use super::{ParseStats, Parser, ToolNames, event, parse_object, str_at, text_event};
use amail_proto::{TranscriptEvent, TranscriptKind as K, TranscriptRole as R};
use serde_json::Value;

#[derive(Debug, Default)]
pub struct ClaudeParser {
    stats: ParseStats,
    tools: ToolNames,
}

impl Parser for ClaudeParser {
    fn line(&mut self, line: &str) -> Vec<TranscriptEvent> {
        let Some(v) = parse_object(line, &mut self.stats) else {
            return Vec::new();
        };
        let ts = str_at(&v, "timestamp").map(str::to_string);
        let flag = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
        if flag("isSidechain") || flag("isMeta") {
            return Vec::new();
        }
        match str_at(&v, "type") {
            Some("user") => self.user(&v, ts),
            Some("assistant") => self.assistant(&v, ts),
            Some("attachment") => self.attachment(&v, ts),
            _ => {
                self.stats.ignored += 1;
                Vec::new()
            }
        }
    }

    fn stats(&self) -> ParseStats {
        self.stats
    }
}

/// Text of a `content` that is a string or a list of blocks.
fn content_text(c: &Value) -> String {
    match c {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| match str_at(b, "type") {
                Some("text") => str_at(b, "text").map(str::to_string),
                Some("image") => Some("[image]".to_string()),
                _ => b.as_str().map(str::to_string),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// What a tool call shows: a shell command as is, anything else as compact
/// JSON.
fn tool_input_text(input: &Value) -> String {
    match input.get("command") {
        Some(Value::String(c)) => c.clone(),
        _ => match input {
            Value::Null => String::new(),
            Value::String(s) => s.clone(),
            other => other.to_string(),
        },
    }
}

impl ClaudeParser {
    fn user(&mut self, v: &Value, ts: Option<String>) -> Vec<TranscriptEvent> {
        let Some(content) = v.pointer("/message/content") else {
            self.stats.ignored += 1;
            return Vec::new();
        };
        let blocks = match content {
            Value::String(s) => {
                if s.trim().is_empty() {
                    return Vec::new();
                }
                return vec![text_event(ts, R::User, s)];
            }
            Value::Array(b) => b,
            _ => {
                self.stats.ignored += 1;
                return Vec::new();
            }
        };
        let mut out = Vec::new();
        for b in blocks {
            match str_at(b, "type") {
                Some("text") => {
                    if let Some(t) = str_at(b, "text").filter(|t| !t.trim().is_empty()) {
                        out.push(text_event(ts.clone(), R::User, t));
                    }
                }
                Some("image") => out.push(event(ts.clone(), R::User, K::Text, "[image]", None)),
                Some("tool_result") => {
                    let tool = str_at(b, "tool_use_id").and_then(|id| self.tools.get(id));
                    let text = b.get("content").map(content_text).unwrap_or_default();
                    let mut e = event(ts.clone(), R::Tool, K::ToolResult, &text, tool);
                    e.error = b.get("is_error").and_then(Value::as_bool).unwrap_or(false);
                    out.push(e);
                }
                _ => {}
            }
        }
        out
    }

    fn assistant(&mut self, v: &Value, ts: Option<String>) -> Vec<TranscriptEvent> {
        let content = v.pointer("/message/content");
        let blocks = match content {
            Some(Value::Array(b)) => b,
            Some(Value::String(s)) if !s.trim().is_empty() => {
                return vec![text_event(ts, R::Assistant, s)];
            }
            Some(_) => return Vec::new(),
            None => {
                self.stats.ignored += 1;
                return Vec::new();
            }
        };
        let mut out = Vec::new();
        for b in blocks {
            match str_at(b, "type") {
                Some("text") => {
                    if let Some(t) = str_at(b, "text").filter(|t| !t.trim().is_empty()) {
                        out.push(text_event(ts.clone(), R::Assistant, t));
                    }
                }
                Some("tool_use") => {
                    let name = str_at(b, "name").unwrap_or("tool").to_string();
                    if let Some(id) = str_at(b, "id") {
                        self.tools.insert(id, &name);
                    }
                    let text = b.get("input").map(tool_input_text).unwrap_or_default();
                    out.push(event(
                        ts.clone(),
                        R::Assistant,
                        K::ToolCall,
                        &text,
                        Some(name),
                    ));
                }
                // thinking, redacted_thinking, server tool blocks...
                _ => {}
            }
        }
        out
    }

    fn attachment(&mut self, v: &Value, ts: Option<String>) -> Vec<TranscriptEvent> {
        let Some(a) = v.get("attachment") else {
            self.stats.ignored += 1;
            return Vec::new();
        };
        match str_at(a, "type") {
            Some("hook_additional_context") => {
                let text = a.get("content").map(content_text).unwrap_or_default();
                if text.trim().is_empty() {
                    return Vec::new();
                }
                vec![event(ts, R::System, K::Notice, &text, None)]
            }
            Some("queued_command") => {
                let text = a.get("prompt").map(content_text).unwrap_or_default();
                if text.trim().is_empty() {
                    return Vec::new();
                }
                vec![text_event(ts, R::User, &text)]
            }
            _ => {
                self.stats.ignored += 1;
                Vec::new()
            }
        }
    }
}

/// What the end of a Claude Code transcript says about the session's turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnMark {
    /// The last record that belongs to a turn is the person interrupting it
    /// (Escape): a user message `[Request interrupted by user]` or
    /// `[Request interrupted by user for tool use]`. Claude Code fires no
    /// hook for an interrupted turn. `ts` is the record's `timestamp`.
    Interrupted { ts: Option<String> },
    /// The last such record is anything else (a prompt, a tool call or its
    /// result, model output): the turn may still be running.
    Other,
}

/// Prefix of the text Claude Code records when a person interrupts a turn.
const INTERRUPT_TEXT: &str = "[request interrupted by user";

fn is_interrupt_text(t: &str) -> bool {
    let t = t.trim_start();
    t.len() < 200 && t.to_ascii_lowercase().starts_with(INTERRUPT_TEXT)
}

/// The [`TurnMark`] of the last turn record in `tail` (whole JSONL lines;
/// unknown records, sub-agent and meta records are skipped). `None` when
/// no line says anything about a turn.
pub fn last_turn_mark(tail: &str) -> Option<TurnMark> {
    let mut mark = None;
    let mut stats = ParseStats::default();
    for line in tail.lines() {
        let Some(v) = parse_object(line, &mut stats) else {
            continue;
        };
        let flag = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
        if flag("isSidechain") || flag("isMeta") {
            continue;
        }
        let ts = || str_at(&v, "timestamp").map(str::to_string);
        match str_at(&v, "type") {
            Some("user") => {
                let interrupted = match v.pointer("/message/content") {
                    Some(Value::String(s)) => is_interrupt_text(s),
                    Some(Value::Array(blocks)) => blocks.iter().any(|b| {
                        str_at(b, "type") == Some("text")
                            && str_at(b, "text").is_some_and(is_interrupt_text)
                    }),
                    _ => continue,
                };
                mark = Some(if interrupted {
                    TurnMark::Interrupted { ts: ts() }
                } else {
                    TurnMark::Other
                });
            }
            Some("assistant") => mark = Some(TurnMark::Other),
            Some("attachment") => match v.pointer("/attachment/type").and_then(Value::as_str) {
                Some("queued_command") | Some("hook_additional_context") => {
                    mark = Some(TurnMark::Other)
                }
                _ => {}
            },
            _ => {}
        }
    }
    mark
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupt_marks() {
        let prompt = r#"{"type":"user","message":{"role":"user","content":"Write a story."},"timestamp":"2026-10-05T02:40:58.000Z"}"#;
        let text = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Once"}]},"timestamp":"2026-10-05T02:41:00.000Z"}"#;
        let tool = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"sleep 100"}}]},"timestamp":"2026-10-05T02:41:01.000Z"}"#;
        let rejected = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"The user doesn't want to proceed with this tool use.","is_error":true}]},"timestamp":"2026-10-05T02:41:02.000Z"}"#;
        let esc = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"[Request interrupted by user]"}]},"timestamp":"2026-10-05T02:41:02.500Z","newField":1}"#;
        let esc_tool = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"[Request interrupted by user for tool use]"}]},"timestamp":"2026-10-05T02:41:03.000Z"}"#;
        let esc_str = r#"{"type":"user","message":{"role":"user","content":"[Request interrupted by user]"}}"#;
        let noise = [
            r#"{"type":"system","subtype":"turn_duration","timestamp":"2026-10-05T02:41:04.000Z"}"#,
            r#"{"type":"file-history-snapshot","snapshot":{}}"#,
            r#"{"type":"attachment","attachment":{"type":"environment"}}"#,
            r#"{"type":"user","isMeta":true,"message":{"content":"<local-command-caveat>"}}"#,
            r#"{"type":"assistant","isSidechain":true,"message":{"content":[{"type":"text","text":"x"}]}}"#,
            "not json",
        ]
        .join("\n");
        let join = |v: &[&str]| v.join("\n");
        assert_eq!(last_turn_mark(""), None);
        assert_eq!(last_turn_mark(&noise), None);
        // A long generation and a long tool call: no interrupt.
        assert_eq!(last_turn_mark(prompt), Some(TurnMark::Other));
        assert_eq!(
            last_turn_mark(&join(&[prompt, text, tool])),
            Some(TurnMark::Other)
        );
        // Interrupted while the model wrote, then records that do not belong
        // to a turn.
        assert_eq!(
            last_turn_mark(&join(&[prompt, text, esc, &noise])),
            Some(TurnMark::Interrupted {
                ts: Some("2026-10-05T02:41:02.500Z".into())
            })
        );
        // Interrupted during a tool call.
        assert!(matches!(
            last_turn_mark(&join(&[prompt, tool, rejected, esc_tool])),
            Some(TurnMark::Interrupted { .. })
        ));
        assert_eq!(
            last_turn_mark(esc_str),
            Some(TurnMark::Interrupted { ts: None })
        );
        // A new prompt after the interrupt: running again.
        assert_eq!(last_turn_mark(&join(&[esc, prompt])), Some(TurnMark::Other));
        let queued = r#"{"type":"attachment","attachment":{"type":"queued_command","prompt":"[amail] You have new mail (wake #4)."}}"#;
        assert_eq!(last_turn_mark(&join(&[esc, queued])), Some(TurnMark::Other));
        // The marker text quoted inside a longer message is not one.
        let quoted = r#"{"type":"user","message":{"content":"[Request interrupted by user] is what Claude Code writes when you press Escape; please explain why it appears in the log file I attached, which is long enough to make this message longer than two hundred characters in total."}}"#;
        assert_eq!(last_turn_mark(quoted), Some(TurnMark::Other));
    }

    // Hand-written records shaped like Claude Code 2.1 transcripts.
    const SAMPLE: &str = r#"{"type":"permission-mode","permissionMode":"bypassPermissions","sessionId":"s1"}
{"type":"user","message":{"role":"user","content":"Build the parser."},"uuid":"u1","timestamp":"2026-10-04T10:00:00.000Z","sessionId":"s1","isSidechain":false}
{"type":"attachment","attachment":{"type":"environment","snapshot":{}},"timestamp":"2026-10-04T10:00:00.010Z"}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"","signature":"x"},{"type":"text","text":"Looking at the tests first."},{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"cargo test","description":"run tests"}}]},"timestamp":"2026-10-04T10:00:01.000Z"}
not json at all
{"type":"user","message":{"role":"user","content":[{"tool_use_id":"toolu_1","type":"tool_result","content":"test result: FAILED","is_error":true}]},"timestamp":"2026-10-04T10:00:02.000Z"}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_2","name":"Edit","input":{"file_path":"/w/a.rs","old_string":"a","new_string":"b"}}]},"timestamp":"2026-10-04T10:00:03.000Z"}
{"type":"user","message":{"role":"user","content":[{"tool_use_id":"toolu_2","type":"tool_result","content":[{"type":"text","text":"edited"},{"type":"image","source":{}}]}]},"timestamp":"2026-10-04T10:00:04.000Z"}
{"type":"attachment","attachment":{"type":"hook_additional_context","content":["[amail] 1 new message for w1:\n#4 owner (USER): Use the v2 API"],"hookEvent":"PostToolUse"},"timestamp":"2026-10-04T10:00:05.000Z"}
{"type":"attachment","attachment":{"type":"queued_command","prompt":"[amail] You have new mail (wake #2).","origin":{"kind":"peer"}},"timestamp":"2026-10-04T10:00:06.000Z"}
{"type":"user","message":{"role":"user","content":"[amail] You have new mail (wake #3)."},"timestamp":"2026-10-04T10:00:07.000Z"}
{"type":"user","isMeta":true,"message":{"role":"user","content":"<local-command-caveat>"},"timestamp":"2026-10-04T10:00:08.000Z"}
{"type":"assistant","isSidechain":true,"message":{"role":"assistant","content":[{"type":"text","text":"hidden agent"}]}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Done: all tests pass."}]},"timestamp":"2026-10-04T10:00:09.000Z","someNewField":{"x":1}}
{"type":"system","subtype":"stop_hook_summary","timestamp":"2026-10-04T10:00:10.000Z"}
[1,2,3]
"#;

    #[test]
    fn claude_sample() {
        let mut p = ClaudeParser::default();
        let evs: Vec<TranscriptEvent> = SAMPLE.lines().flat_map(|l| p.line(l)).collect();
        let short: Vec<(R, K, &str, Option<&str>)> = evs
            .iter()
            .map(|e| (e.role, e.kind, e.text.as_str(), e.tool.as_deref()))
            .collect();
        assert_eq!(
            short,
            vec![
                (R::User, K::Text, "Build the parser.", None),
                (R::Assistant, K::Text, "Looking at the tests first.", None),
                (R::Assistant, K::ToolCall, "cargo test", Some("Bash")),
                (R::Tool, K::ToolResult, "test result: FAILED", Some("Bash")),
                (
                    R::Assistant,
                    K::ToolCall,
                    r#"{"file_path":"/w/a.rs","new_string":"b","old_string":"a"}"#,
                    Some("Edit")
                ),
                (R::Tool, K::ToolResult, "edited\n[image]", Some("Edit")),
                (
                    R::System,
                    K::Notice,
                    "[amail] 1 new message for w1:\n#4 owner (USER): Use the v2 API",
                    None
                ),
                (
                    R::User,
                    K::Notice,
                    "[amail] You have new mail (wake #2).",
                    None
                ),
                (
                    R::User,
                    K::Notice,
                    "[amail] You have new mail (wake #3).",
                    None
                ),
                (R::Assistant, K::Text, "Done: all tests pass.", None),
            ]
        );
        assert!(evs[3].error);
        assert!(!evs[5].error);
        assert_eq!(evs[0].ts.as_deref(), Some("2026-10-04T10:00:00.000Z"));
        let s = p.stats();
        assert_eq!(s.skipped, 2, "the garbage line and the JSON array");
        // permission-mode, environment attachment, system
        assert_eq!(s.ignored, 3);
        assert_eq!(s.lines, SAMPLE.lines().count() as u64);
    }

    #[test]
    fn claude_truncates_tool_output() {
        let mut p = ClaudeParser::default();
        let big = "x".repeat(10_000);
        let line = serde_json::json!({
            "type": "user",
            "message": {"content": [{"type": "tool_result", "tool_use_id": "nope", "content": big}]}
        })
        .to_string();
        let evs = p.line(&line);
        assert_eq!(evs.len(), 1);
        assert!(evs[0].truncated);
        assert_eq!(evs[0].text.len(), super::super::MAX_TOOL_CHARS);
        assert_eq!(evs[0].tool, None);
    }

    #[test]
    fn claude_odd_shapes_do_not_panic() {
        let mut p = ClaudeParser::default();
        for l in [
            r#"{"type":"user"}"#,
            r#"{"type":"user","message":{"content":42}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use"}]}}"#,
            r#"{"type":"assistant","message":{"content":[null,1,"x"]}}"#,
            r#"{"type":"attachment"}"#,
            r#"{"type":"attachment","attachment":{"type":"hook_additional_context","content":null}}"#,
            r#"{"type":5}"#,
            "",
            "   ",
            "{",
        ] {
            p.line(l);
        }
        assert_eq!(p.stats().skipped, 1);
    }
}
