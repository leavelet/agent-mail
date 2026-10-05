//! Codex rollout files (`$CODEX_HOME/sessions/YYYY/MM/DD/rollout-*.jsonl`).
//!
//! Every line is `{"timestamp", "type", "payload", ...}`. The conversation is
//! taken from `event_msg` records of payload type `item_completed`:
//!
//! - `UserMessage` (`content[].text`) and `AgentMessage` (`content[].text`);
//! - `CommandExecution` (`parsed_cmd[].cmd` or `command`, `aggregated_output`,
//!   `exit_code`, `status`): a tool call and its result;
//! - `FileChange` (`changes`: path -> `unified_diff`): an `apply_patch` call
//!   and the diffs;
//! - `HookPrompt` (`fragments[].text`): a hook's continuation prompt.
//!
//! A `task_complete` event with an `error` (a turn that failed, e.g. the
//! model was refused) becomes an error line; `turn_aborted` a notice.
//!
//! Hook context (injected mail) arrives as a `response_item` message with
//! role `developer`; those starting with `[amail` are notices. Other
//! `response_item` records duplicate the items above and are skipped, as are
//! token counts, reasoning and settings.

use super::{ParseStats, Parser, event, is_notice, parse_object, str_at, text_event};
use amail_proto::{TranscriptEvent, TranscriptKind as K, TranscriptRole as R};
use serde_json::Value;

#[derive(Debug, Default)]
pub struct CodexParser {
    stats: ParseStats,
}

impl Parser for CodexParser {
    fn line(&mut self, line: &str) -> Vec<TranscriptEvent> {
        let Some(v) = parse_object(line, &mut self.stats) else {
            return Vec::new();
        };
        let ts = str_at(&v, "timestamp").map(str::to_string);
        let null = Value::Null;
        let p = v.get("payload").unwrap_or(&null);
        let out = match (str_at(&v, "type"), str_at(p, "type")) {
            (Some("event_msg"), Some("item_completed")) => match p.get("item") {
                Some(item) => item_events(item, ts),
                None => None,
            },
            (Some("event_msg"), Some("turn_aborted")) => Some(vec![event(
                ts,
                R::System,
                K::Notice,
                "turn interrupted",
                None,
            )]),
            (Some("response_item"), Some("message")) if str_at(p, "role") == Some("developer") => {
                let text = blocks_text(p.get("content"));
                // Only mail-system context; other developer messages are
                // instructions the harness adds.
                Some(if is_notice(&text) {
                    vec![event(ts, R::System, K::Notice, &text, None)]
                } else {
                    Vec::new()
                })
            }
            // A turn that failed (a model or API error): `task_complete`
            // with `error.message`.
            (Some("event_msg"), Some("task_complete")) => Some(
                match p.get("error").filter(|e| !e.is_null()).map(|e| {
                    str_at(e, "message")
                        .map(str::to_string)
                        .unwrap_or_else(|| e.to_string())
                }) {
                    Some(msg) => {
                        let mut e = event(
                            ts,
                            R::System,
                            K::Notice,
                            &format!("turn failed: {msg}"),
                            None,
                        );
                        e.error = true;
                        vec![e]
                    }
                    None => Vec::new(),
                },
            ),
            // Duplicates of `item_completed` items, or bookkeeping.
            (Some("response_item"), _)
            | (Some("event_msg"), Some("task_started" | "token_count"))
            | (Some("session_meta" | "turn_context" | "world_state" | "token_usage_record"), _) => {
                Some(Vec::new())
            }
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

/// `[{"type": "text"|"Text"|"input_text"|..., "text": ...}]` joined.
fn blocks_text(c: Option<&Value>) -> String {
    match c {
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|b| str_at(b, "text").or_else(|| b.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    }
}

/// Events of one completed item; `None` for unknown item types.
fn item_events(item: &Value, ts: Option<String>) -> Option<Vec<TranscriptEvent>> {
    let nonempty = |e: TranscriptEvent| {
        if e.text.trim().is_empty() {
            vec![]
        } else {
            vec![e]
        }
    };
    Some(match str_at(item, "type")? {
        "UserMessage" => nonempty(text_event(ts, R::User, &blocks_text(item.get("content")))),
        "AgentMessage" => nonempty(text_event(
            ts,
            R::Assistant,
            &blocks_text(item.get("content")),
        )),
        "HookPrompt" => {
            let text = match item.get("fragments") {
                Some(f) => blocks_text(Some(f)),
                None => String::new(),
            };
            nonempty(event(ts, R::System, K::Notice, &text, None))
        }
        "CommandExecution" => {
            let parsed: Vec<&str> = item
                .get("parsed_cmd")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|c| str_at(c, "cmd")).collect())
                .unwrap_or_default();
            let cmd = if !parsed.is_empty() {
                parsed.join(" && ")
            } else {
                match item.get("command") {
                    Some(Value::Array(a)) => a
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" "),
                    Some(Value::String(s)) => s.clone(),
                    _ => String::new(),
                }
            };
            let output = str_at(item, "aggregated_output")
                .or_else(|| str_at(item, "formatted_output"))
                .or_else(|| str_at(item, "stdout"))
                .unwrap_or_default();
            let failed = item
                .get("exit_code")
                .and_then(Value::as_i64)
                .is_some_and(|c| c != 0)
                || matches!(str_at(item, "status"), Some("failed" | "declined"));
            let tool = Some("exec".to_string());
            let mut result = event(ts.clone(), R::Tool, K::ToolResult, output, tool.clone());
            result.error = failed;
            vec![event(ts, R::Assistant, K::ToolCall, &cmd, tool), result]
        }
        "FileChange" => {
            let (mut paths, mut diffs) = (Vec::new(), Vec::new());
            if let Some(Value::Object(changes)) = item.get("changes") {
                for (path, c) in changes {
                    let kind = str_at(c, "type").unwrap_or("update");
                    paths.push(format!("{kind} {path}"));
                    if let Some(d) = str_at(c, "unified_diff").or_else(|| str_at(c, "content")) {
                        diffs.push(format!("--- {path}\n{d}"));
                    }
                }
            }
            let tool = Some("apply_patch".to_string());
            let mut result = event(
                ts.clone(),
                R::Tool,
                K::ToolResult,
                &diffs.join("\n"),
                tool.clone(),
            );
            result.error = matches!(str_at(item, "status"), Some("failed" | "declined"));
            vec![
                event(ts, R::Assistant, K::ToolCall, &paths.join("\n"), tool),
                result,
            ]
        }
        // Reasoning is encrypted or summarized elsewhere; nothing to show.
        "Reasoning" => Vec::new(),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Hand-written records shaped like Codex 0.160 rollout files.
    const SAMPLE: &str = r#"{"timestamp":"2026-10-04T10:00:00.000Z","ordinal":0,"type":"session_meta","payload":{"id":"t1","cwd":"/w"}}
{"timestamp":"2026-10-04T10:00:00.100Z","ordinal":1,"type":"event_msg","payload":{"type":"task_started","turn_id":"r1"}}
{"timestamp":"2026-10-04T10:00:00.200Z","ordinal":2,"type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"You are a coding agent."}]}}
{"timestamp":"2026-10-04T10:00:00.300Z","ordinal":3,"type":"event_msg","payload":{"type":"item_completed","thread_id":"t1","turn_id":"r1","item":{"type":"UserMessage","id":"i1","content":[{"type":"text","text":"Write the README.","text_elements":[]}]}}}
{"timestamp":"2026-10-04T10:00:00.400Z","ordinal":4,"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Write the README."}]}}
{"timestamp":"2026-10-04T10:00:00.500Z","ordinal":5,"type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"[amail] 1 new message for cx:\n#9 lead: rebase first"}]}}
{"timestamp":"2026-10-04T10:00:01.000Z","ordinal":6,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"AgentMessage","id":"i2","content":[{"type":"Text","text":"Checking the tree."}],"phase":"commentary"}}}
{"timestamp":"2026-10-04T10:00:01.100Z","ordinal":7,"type":"response_item","payload":{"type":"custom_tool_call","call_id":"c1","name":"exec","input":"ls"}}
{"timestamp":"2026-10-04T10:00:02.000Z","ordinal":8,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"CommandExecution","id":"i3","command":["/bin/bash"],"parsed_cmd":[{"type":"list_files","cmd":"ls -la"}],"status":"completed","aggregated_output":"README.md\nsrc\n","exit_code":0}}}
{"timestamp":"2026-10-04T10:00:02.500Z","ordinal":9,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"CommandExecution","id":"i4","command":["false"],"status":"failed","formatted_output":"boom","exit_code":1}}}
{"timestamp":"2026-10-04T10:00:03.000Z","ordinal":10,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"FileChange","id":"i5","changes":{"/w/README.md":{"type":"update","unified_diff":"@@ -1 +1 @@\n-old\n+new\n","move_path":null}},"status":"completed"}}}
{"timestamp":"2026-10-04T10:00:03.500Z","ordinal":11,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"Reasoning","id":"i6"}}}
{"timestamp":"2026-10-04T10:00:04.000Z","ordinal":12,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"HookPrompt","id":"i7","fragments":[{"text":"[amail] 2 new messages for cx","hookRunId":"h"}]}}}
{"timestamp":"2026-10-04T10:00:04.500Z","ordinal":13,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage","id":"i8","content":[{"type":"text","text":"[amail] You have new mail (wake #1)."}]}}}
{"timestamp":"2026-10-04T10:00:05.000Z","ordinal":14,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"WebSearch","id":"i9"}}}
{"timestamp":"2026-10-04T10:00:05.500Z","ordinal":15,"type":"event_msg","payload":{"type":"turn_aborted","reason":"interrupted"}}
{"timestamp":"2026-10-04T10:00:06.000Z","ordinal":16,"type":"event_msg","payload":{"type":"token_count","info":{}}}
{"timestamp":"2026-10-04T10:00:06.100Z","ordinal":17,"type":"brand_new_record","payload":{}}
garbage {
{"timestamp":"2026-10-04T10:00:07.000Z","ordinal":18,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"AgentMessage","content":[{"type":"Text","text":"README written."}]}}}
"#;

    #[test]
    fn codex_sample() {
        let mut p = CodexParser::default();
        let evs: Vec<TranscriptEvent> = SAMPLE.lines().flat_map(|l| p.line(l)).collect();
        let short: Vec<(R, K, &str, Option<&str>, bool)> = evs
            .iter()
            .map(|e| (e.role, e.kind, e.text.as_str(), e.tool.as_deref(), e.error))
            .collect();
        assert_eq!(
            short,
            vec![
                (R::User, K::Text, "Write the README.", None, false),
                (
                    R::System,
                    K::Notice,
                    "[amail] 1 new message for cx:\n#9 lead: rebase first",
                    None,
                    false
                ),
                (R::Assistant, K::Text, "Checking the tree.", None, false),
                (R::Assistant, K::ToolCall, "ls -la", Some("exec"), false),
                (
                    R::Tool,
                    K::ToolResult,
                    "README.md\nsrc\n",
                    Some("exec"),
                    false
                ),
                (R::Assistant, K::ToolCall, "false", Some("exec"), false),
                (R::Tool, K::ToolResult, "boom", Some("exec"), true),
                (
                    R::Assistant,
                    K::ToolCall,
                    "update /w/README.md",
                    Some("apply_patch"),
                    false
                ),
                (
                    R::Tool,
                    K::ToolResult,
                    "--- /w/README.md\n@@ -1 +1 @@\n-old\n+new\n",
                    Some("apply_patch"),
                    false
                ),
                (
                    R::System,
                    K::Notice,
                    "[amail] 2 new messages for cx",
                    None,
                    false
                ),
                (
                    R::User,
                    K::Notice,
                    "[amail] You have new mail (wake #1).",
                    None,
                    false
                ),
                (R::System, K::Notice, "turn interrupted", None, false),
                (R::Assistant, K::Text, "README written.", None, false),
            ]
        );
        assert_eq!(evs[0].ts.as_deref(), Some("2026-10-04T10:00:00.300Z"));
        let s = p.stats();
        assert_eq!(s.skipped, 1);
        // WebSearch item and the unknown record type.
        assert_eq!(s.ignored, 2);
    }

    /// Shaped like codex-cli 0.160 records of a turn the model refused and
    /// of one that ended normally.
    #[test]
    fn codex_failed_turn_is_an_error_line() {
        let mut p = CodexParser::default();
        let failed = r#"{"timestamp":"2026-10-05T03:23:00.100Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"t1","last_agent_message":null,"error":{"message":"{\"type\":\"error\",\"status\":400,\"error\":{\"message\":\"The 'gpt-bogus' model is not supported\"}}"},"newField":1}}"#;
        let ok = r#"{"timestamp":"2026-10-05T03:23:59.000Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"t2","last_agent_message":"done","duration_ms":7365}}"#;
        let evs = p.line(failed);
        assert_eq!(evs.len(), 1);
        assert!(evs[0].error);
        assert_eq!(evs[0].kind, K::Notice);
        assert!(
            evs[0].text.starts_with("turn failed: ") && evs[0].text.contains("gpt-bogus"),
            "{}",
            evs[0].text
        );
        assert!(p.line(ok).is_empty());
        assert_eq!(p.stats().ignored, 0);
    }

    #[test]
    fn codex_odd_shapes_do_not_panic() {
        let mut p = CodexParser::default();
        for l in [
            r#"{"type":"event_msg"}"#,
            r#"{"type":"event_msg","payload":{"type":"item_completed"}}"#,
            r#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"CommandExecution"}}}"#,
            r#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"FileChange","changes":[]}}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"developer","content":7}}"#,
            r#"{"payload":null}"#,
        ] {
            p.line(l);
        }
        let big = "y".repeat(5000);
        let l = serde_json::json!({"type":"event_msg","payload":{"type":"item_completed",
            "item":{"type":"CommandExecution","command":"cat big","aggregated_output":big,"exit_code":0}}})
        .to_string();
        let evs = p.line(&l);
        assert_eq!(evs[0].text, "cat big");
        assert!(evs[1].truncated);
    }
}
