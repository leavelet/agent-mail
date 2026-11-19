//! Antigravity CLI transcripts
//! (`<app data>/brain/<conversation>/.system_generated/logs/transcript_full.jsonl`,
//! the path every hook gets as `transcriptPath`).
//!
//! One step per line: `{"step_index", "source", "type", "status",
//! "created_at" (RFC 3339), "content", "tool_calls", "model",
//! "input_tokens", "output_tokens", "cache_read_tokens"}` (agy 1.5.11):
//!
//! - `USER_INPUT`: `content` wraps the text in `<USER_REQUEST>` ...
//!   `</USER_REQUEST>`, followed by blocks agy adds (`<ADDITIONAL_METADATA>`
//!   with the local time, `<USER_SETTINGS_CHANGE>` on the first step).
//!   Source `USER_EXPLICIT` is a prompt (typed, or sent through the
//!   language server: wake-ups and the person's prompts); `SYSTEM_SDK` is a
//!   `userMessage` a hook injected;
//! - `EPHEMERAL_MESSAGE` (source `SYSTEM_SDK`): an `ephemeralMessage` a hook
//!   injected (the mail amail shows);
//! - `PLANNER_RESPONSE` (source `MODEL`): the model's text in `content` and
//!   its tool calls in `tool_calls[]` (`name`, `args`; the shell tool
//!   `run_command` has `args.CommandLine`);
//! - `GENERIC` (source `MODEL`): a tool's result, in the order of the calls;
//! - `SYSTEM_MESSAGE`: text agy adds (background task completions,
//!   notices); skipped.
//!
//! Steps of other types are ignored and counted.

use super::{ParseStats, Parser, event, is_notice, parse_object, str_at, text_event};
use amail_proto::{TranscriptEvent, TranscriptKind as K, TranscriptRole as R};
use serde_json::Value;
use std::collections::VecDeque;

/// Tool calls whose result has not been seen, at most.
const PENDING_CAP: usize = 64;

#[derive(Debug, Default)]
pub struct AgyParser {
    stats: ParseStats,
    /// Names of the tool calls waiting for their `GENERIC` result, oldest
    /// first.
    pending: VecDeque<String>,
}

/// The text of a user step: what is inside `<USER_REQUEST>`, or, without
/// that wrapper, the text before the blocks agy appends
/// (`<ADDITIONAL_METADATA>`, `<USER_SETTINGS...>`).
pub fn user_request(text: &str) -> String {
    const OPEN: &str = "<USER_REQUEST>";
    if let Some(start) = text.find(OPEN) {
        let rest = &text[start + OPEN.len()..];
        let end = rest.find("</USER_REQUEST>").unwrap_or(rest.len());
        return rest[..end].trim().to_string();
    }
    let cut = ["<ADDITIONAL_METADATA>", "<USER_SETTINGS"]
        .iter()
        .filter_map(|m| text.find(m))
        .min()
        .unwrap_or(text.len());
    text[..cut].trim().to_string()
}

/// A tool call's text: the shell command when there is one, else the
/// arguments as compact JSON.
fn call_text(args: Option<&Value>) -> String {
    match args {
        Some(a) => match str_at(a, "CommandLine") {
            Some(c) => c.to_string(),
            None => match a {
                Value::Null => String::new(),
                Value::String(s) => s.clone(),
                v => v.to_string(),
            },
        },
        None => String::new(),
    }
}

/// Text that a hook injected: a notice when amail wrote it, else text the
/// model got as the user's.
fn injected(ts: Option<String>, text: &str) -> Vec<TranscriptEvent> {
    if text.trim().is_empty() {
        Vec::new()
    } else if is_notice(text) {
        vec![event(ts, R::System, K::Notice, text, None)]
    } else {
        vec![text_event(ts, R::User, text)]
    }
}

impl Parser for AgyParser {
    fn line(&mut self, line: &str) -> Vec<TranscriptEvent> {
        let Some(v) = parse_object(line, &mut self.stats) else {
            return Vec::new();
        };
        let ts = str_at(&v, "created_at").map(str::to_string);
        let content = str_at(&v, "content").unwrap_or_default();
        let source = str_at(&v, "source");
        let out = match str_at(&v, "type") {
            Some("USER_INPUT") => {
                let text = user_request(content);
                Some(match source {
                    Some("SYSTEM_SDK") => injected(ts, &text),
                    _ if text.is_empty() => Vec::new(),
                    _ => vec![text_event(ts, R::User, &text)],
                })
            }
            Some("EPHEMERAL_MESSAGE") => Some(injected(ts, content)),
            Some("PLANNER_RESPONSE") => {
                let mut out = Vec::new();
                if !content.trim().is_empty() {
                    out.push(text_event(ts.clone(), R::Assistant, content));
                }
                for call in v
                    .get("tool_calls")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let name = str_at(call, "name").unwrap_or("tool").to_string();
                    out.push(event(
                        ts.clone(),
                        R::Assistant,
                        K::ToolCall,
                        &call_text(call.get("args")),
                        Some(name.clone()),
                    ));
                    if self.pending.len() >= PENDING_CAP {
                        self.pending.pop_front();
                    }
                    self.pending.push_back(name);
                }
                Some(out)
            }
            Some("GENERIC") if source.is_none_or(|s| s == "MODEL") => {
                let tool = self.pending.pop_front();
                Some(vec![event(ts, R::Tool, K::ToolResult, content, tool)])
            }
            Some("GENERIC" | "SYSTEM_MESSAGE") => Some(Vec::new()),
            _ => None,
        };
        out.unwrap_or_else(|| {
            self.stats.ignored += 1;
            Vec::new()
        })
    }

    fn stats(&self) -> ParseStats {
        self.stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lines of a transcript agy 1.5.11 wrote (long content shortened), an
    /// injected `userMessage` and `ephemeralMessage`, and a step of a type
    /// the parser does not know.
    const LOG: &str = r#"{"step_index": 0, "source": "USER_EXPLICIT", "type": "USER_INPUT", "status": "DONE", "created_at": "2026-11-18T10:25:16Z", "content": "<USER_REQUEST>\nRun this shell command and then reply with one word, done: env | grep ANTIGRAVITY > log/env.txt\n</USER_REQUEST>\n<ADDITIONAL_METADATA>\nThe current local time is: 2026-11-18T18:25:16+08:00.\n</ADDITIONAL_METADATA>\n<USER_SETTINGS_CHANGE>\nThe user changed settings.\n</USER_SETTINGS_CHANGE>"}
{"step_index": 1, "source": "MODEL", "type": "PLANNER_RESPONSE", "status": "DONE", "created_at": "2026-11-18T10:25:16Z", "model": "gemini-3.8-flash-low", "input_tokens": 9213, "cache_read_tokens": 0, "output_tokens": 90, "tool_calls": [{"name": "run_command", "args": {"CommandLine": "mkdir -p log && env | grep ANTIGRAVITY > log/env.txt", "WaitMsBeforeAsync": 5000, "toolSummary": "Save ANTIGRAVITY env vars"}}]}
{"step_index": 2, "source": "MODEL", "type": "GENERIC", "status": "DONE", "created_at": "2026-11-18T10:25:18Z", "content": "Created At: 2026-11-18T18:25:18+08:00\nCommand exited with code 0.\n\nNo output from the command."}
{"step_index": 3, "source": "MODEL", "type": "PLANNER_RESPONSE", "status": "DONE", "created_at": "2026-11-18T10:25:18Z", "model": "gemini-3.8-flash-low", "input_tokens": 2218, "cache_read_tokens": 7172, "output_tokens": 1, "content": "done"}
{"step_index": 5, "source": "USER_EXPLICIT", "type": "USER_INPUT", "status": "DONE", "created_at": "2026-11-18T10:27:01Z", "content": "<USER_REQUEST>\n[amail] You have new mail (wake #2).\n</USER_REQUEST>\n<ADDITIONAL_METADATA>\nThe current local time is: 2026-11-18T18:27:01+08:00.\n</ADDITIONAL_METADATA>"}
{"step_index": 14, "source": "SYSTEM", "type": "SYSTEM_MESSAGE", "status": "DONE", "created_at": "2026-11-18T10:28:14Z", "content": "The following is a <SYSTEM_MESSAGE> not actually sent by the user."}
{"step_index": 22, "source": "SYSTEM_SDK", "type": "USER_INPUT", "status": "DONE", "created_at": "2026-11-18T10:29:49Z", "content": "<USER_REQUEST>\n[amail test] injected user message: also say the word banana\n</USER_REQUEST>\n<ADDITIONAL_METADATA>\nThe current local time is: 2026-11-18T18:29:49+08:00.\n</ADDITIONAL_METADATA>"}
{"step_index": 25, "source": "SYSTEM_SDK", "type": "EPHEMERAL_MESSAGE", "status": "DONE", "created_at": "2026-11-18T10:29:53Z", "content": "[amail] 1 new message for w1", "futureField": {"x": 1}}
{"step_index": 26, "source": "MODEL", "type": "PLANNER_RESPONSE", "status": "DONE", "created_at": "2026-11-18T10:29:53Z", "content": "Reading it.", "tool_calls": [{"name": "view_file", "args": {"AbsolutePath": "/w/a.txt"}}, {"name": "run_command", "args": {"CommandLine": "amail inbox"}}]}
{"step_index": 27, "source": "MODEL", "type": "GENERIC", "status": "DONE", "created_at": "2026-11-18T10:29:54Z", "content": "hello"}
{"step_index": 28, "source": "MODEL", "type": "GENERIC", "status": "DONE", "created_at": "2026-11-18T10:29:55Z", "content": "No unread mail."}
{"step_index": 29, "source": "MODEL", "type": "CHECKPOINT", "status": "DONE", "created_at": "2026-11-18T10:29:56Z"}
not json
"#;

    #[test]
    fn parses_a_transcript() {
        let mut p = AgyParser::default();
        let ev: Vec<TranscriptEvent> = LOG.lines().flat_map(|l| p.line(l)).collect();
        let got: Vec<(R, K, &str, Option<&str>)> = ev
            .iter()
            .map(|e| (e.role, e.kind, e.text.as_str(), e.tool.as_deref()))
            .collect();
        assert_eq!(
            got,
            vec![
                (
                    R::User,
                    K::Text,
                    "Run this shell command and then reply with one word, done: env | grep ANTIGRAVITY > log/env.txt",
                    None
                ),
                (
                    R::Assistant,
                    K::ToolCall,
                    "mkdir -p log && env | grep ANTIGRAVITY > log/env.txt",
                    Some("run_command")
                ),
                (
                    R::Tool,
                    K::ToolResult,
                    "Created At: 2026-11-18T18:25:18+08:00\nCommand exited with code 0.\n\nNo output from the command.",
                    Some("run_command")
                ),
                (R::Assistant, K::Text, "done", None),
                (
                    R::User,
                    K::Notice,
                    "[amail] You have new mail (wake #2).",
                    None
                ),
                (
                    R::System,
                    K::Notice,
                    "[amail test] injected user message: also say the word banana",
                    None
                ),
                (R::System, K::Notice, "[amail] 1 new message for w1", None),
                (R::Assistant, K::Text, "Reading it.", None),
                (
                    R::Assistant,
                    K::ToolCall,
                    "{\"AbsolutePath\":\"/w/a.txt\"}",
                    Some("view_file")
                ),
                (
                    R::Assistant,
                    K::ToolCall,
                    "amail inbox",
                    Some("run_command")
                ),
                (R::Tool, K::ToolResult, "hello", Some("view_file")),
                (
                    R::Tool,
                    K::ToolResult,
                    "No unread mail.",
                    Some("run_command")
                ),
            ]
        );
        assert_eq!(ev[0].ts.as_deref(), Some("2026-11-18T10:25:16Z"));
        let s = p.stats();
        assert_eq!(s.skipped, 1);
        // The `CHECKPOINT` step.
        assert_eq!(s.ignored, 1);
    }

    #[test]
    fn user_requests() {
        assert_eq!(
            user_request(
                "<USER_REQUEST>\nhi\n</USER_REQUEST>\n<ADDITIONAL_METADATA>\nt\n</ADDITIONAL_METADATA>"
            ),
            "hi"
        );
        assert_eq!(user_request("plain"), "plain");
        assert_eq!(
            user_request("text\n<ADDITIONAL_METADATA>\nt\n</ADDITIONAL_METADATA>"),
            "text"
        );
        assert_eq!(user_request("<USER_REQUEST>\nunclosed"), "unclosed");
        assert_eq!(user_request(""), "");
    }

    #[test]
    fn odd_steps_do_not_panic() {
        let mut p = AgyParser::default();
        for l in [
            "{}",
            r#"{"type": "PLANNER_RESPONSE", "tool_calls": "x"}"#,
            r#"{"type": "PLANNER_RESPONSE", "tool_calls": [{"args": null}, 3]}"#,
            r#"{"type": "GENERIC", "content": 5}"#,
            r#"{"type": "USER_INPUT", "content": ["x"]}"#,
            r#"{"type": "EPHEMERAL_MESSAGE"}"#,
            "[1,2]",
        ] {
            p.line(l);
        }
        assert_eq!(p.stats().skipped, 1);
    }
}
