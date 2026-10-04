//! DeepSeek Harness session logs
//! (`$DSH_HOME/sessions/<project>/<session>/session.v4.jsonl`, written
//! uncompressed because amail's dsh home sets `compression: none`).
//!
//! Every line is `{"type", "seq", "time" (Unix ms), "data", ...}`:
//!
//! - `user/message`: `data.content[].text`; `data.source.kind` is `user` for
//!   a person (or an API prompt), `hooks-*` for hook context (injected mail)
//!   and e.g. `runtime-context` for text the harness adds (skipped);
//! - `assistant/message`: `data.message.content[]` blocks; `text` blocks are
//!   the reply (reasoning and tool-call blocks are skipped: tool calls come
//!   from `tool/call`);
//! - `tool/call`: `data.name`, `data.arguments` (a JSON string);
//! - `tool/result`: `data.message.content[].text`, `data.message.isError`;
//! - `turn/end`: `data.reason.kind` (`completed`; `aborted` or
//!   `interrupted` when cancelled or cut off; `error`).
//!
//! Streaming, accounting and hook bookkeeping records are skipped.

use super::{ParseStats, Parser, ToolNames, event, is_notice, parse_object, str_at, text_event};
use amail_proto::{TranscriptEvent, TranscriptKind as K, TranscriptRole as R};
use serde_json::Value;

#[derive(Debug, Default)]
pub struct DshParser {
    stats: ParseStats,
    tools: ToolNames,
}

fn texts(c: Option<&Value>) -> String {
    match c {
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter(|b| str_at(b, "type").is_none_or(|t| t == "text"))
            .filter_map(|b| str_at(b, "text"))
            .collect::<Vec<_>>()
            .join("\n"),
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    }
}

fn ts(v: &Value) -> Option<String> {
    let ms = v.get("time")?.as_i64()?;
    let t = jiff::Timestamp::from_millisecond(ms).ok()?;
    Some(t.to_string())
}

/// A tool call's text: the shell command when there is one, else the
/// arguments.
fn call_text(args: Option<&Value>) -> String {
    let parsed = match args {
        Some(Value::String(s)) => {
            serde_json::from_str::<Value>(s).unwrap_or(Value::String(s.clone()))
        }
        Some(v) => v.clone(),
        None => Value::Null,
    };
    if let Some(cmd) = str_at(&parsed, "command") {
        return cmd.to_string();
    }
    match parsed {
        Value::String(s) => s,
        Value::Null => String::new(),
        v => v.to_string(),
    }
}

impl Parser for DshParser {
    fn line(&mut self, line: &str) -> Vec<TranscriptEvent> {
        let Some(v) = parse_object(line, &mut self.stats) else {
            return Vec::new();
        };
        let ts = ts(&v);
        let null = Value::Null;
        let d = v.get("data").unwrap_or(&null);
        let nonempty = |e: TranscriptEvent| {
            if e.text.trim().is_empty() {
                vec![]
            } else {
                vec![e]
            }
        };
        let out = match str_at(&v, "type") {
            Some("user/message") => {
                let text = texts(d.get("content"));
                let kind = d.get("source").and_then(|s| str_at(s, "kind"));
                Some(match kind {
                    Some("user") | None => nonempty(text_event(ts, R::User, &text)),
                    Some(k) if k.starts_with("hooks") => {
                        if is_notice(&text) {
                            nonempty(event(ts, R::System, K::Notice, &text, None))
                        } else {
                            Vec::new()
                        }
                    }
                    Some(_) => Vec::new(),
                })
            }
            Some("assistant/message") => {
                let msg = d.get("message").unwrap_or(&null);
                Some(nonempty(text_event(
                    ts,
                    R::Assistant,
                    &texts(msg.get("content")),
                )))
            }
            Some("tool/call") => {
                let name = str_at(d, "name").unwrap_or("tool").to_string();
                if let Some(id) = str_at(d, "callId") {
                    self.tools.insert(id, &name);
                }
                Some(vec![event(
                    ts,
                    R::Assistant,
                    K::ToolCall,
                    &call_text(d.get("arguments")),
                    Some(name),
                )])
            }
            Some("tool/result") => {
                let msg = d.get("message").unwrap_or(&null);
                let tool = str_at(msg, "toolCallId").and_then(|id| self.tools.get(id));
                let mut e = event(ts, R::Tool, K::ToolResult, &texts(msg.get("content")), tool);
                e.error = msg.get("isError").and_then(Value::as_bool).unwrap_or(false);
                Some(vec![e])
            }
            Some("turn/end") => {
                let reason = d.get("reason").unwrap_or(&null);
                Some(match str_at(reason, "kind") {
                    Some("completed") | None => Vec::new(),
                    Some("aborted" | "interrupted") => {
                        vec![event(ts, R::System, K::Notice, "turn interrupted", None)]
                    }
                    Some(k) => {
                        let msg = reason
                            .get("error")
                            .and_then(|e| str_at(e, "message"))
                            .unwrap_or(k);
                        vec![event(
                            ts,
                            R::System,
                            K::Notice,
                            &format!("turn ended: {msg}"),
                            None,
                        )]
                    }
                })
            }
            Some(_) => None,
            None => None,
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

    const LOG: &str = r#"{"type":"session","version":4,"id":"session-1","createdAt":1700000000000,"cwd":"/w","isSeeded":false,"delegationDepth":0}
{"type":"turn/start","seq":4,"time":1700000000100,"data":{"turn":1}}
{"type":"user/message","seq":10,"time":1700000000200,"data":{"content":[{"type":"text","text":"Fix the typo."}],"source":{"kind":"user","rpcId":"r1"},"role":"user","id":"u1"}}
{"type":"user/message","seq":11,"time":1700000000201,"data":{"content":[{"type":"text","text":"Current runtime context."}],"source":{"kind":"runtime-context"},"role":"user","id":"u2"}}
{"type":"user/message","seq":12,"time":1700000000202,"data":{"content":[{"type":"text","text":"[amail] 1 new message for w"}],"source":{"kind":"hooks-codex"},"role":"user","id":"u3"}}
{"type":"chunk","seq":13,"time":1700000000300,"data":{}}
{"type":"assistant/message","seq":20,"time":1700000000400,"data":{"turn":1,"step":1,"message":{"role":"assistant","content":[{"type":"reasoning","text":"thinking"},{"type":"text","text":"Looking."},{"type":"tool-call","id":"c1","name":"bash","arguments":"{}"}]}}}
{"type":"tool/call","seq":21,"time":1700000000401,"data":{"turn":1,"step":1,"callId":"c1","name":"bash","arguments":"{\"command\": \"ls\", \"description\": \"List\"}"}}
{"type":"tool/result","seq":26,"time":1700000000500,"data":{"turn":1,"step":1,"message":{"role":"tool","toolCallId":"c1","content":[{"type":"text","text":"a.txt\n"}],"isError":false}},"sourceEventSeqs":[21]}
{"type":"tool/call","seq":27,"time":1700000000600,"data":{"callId":"c2","name":"edit","arguments":"{\"path\": \"a.txt\"}"}}
{"type":"tool/result","seq":28,"time":1700000000700,"data":{"message":{"toolCallId":"c2","content":[{"type":"text","text":"no such file"}],"isError":true,"futureField":1}}}
{"type":"turn/end","seq":30,"time":1700000000800,"data":{"turn":1,"reason":{"kind":"completed"}}}
{"type":"turn/end","seq":40,"time":1700000000900,"data":{"turn":2,"reason":{"kind":"aborted","reason":{"kind":"user"}}}}
{"type":"turn/end","seq":50,"time":1700000001000,"data":{"turn":3,"reason":{"kind":"error","error":{"message":"transport failed","code":"TRANSPORT"}}}}
not json
"#;

    #[test]
    fn parses_a_session() {
        let mut p = DshParser::default();
        let ev: Vec<TranscriptEvent> = LOG.lines().flat_map(|l| p.line(l)).collect();
        let got: Vec<(R, K, &str, Option<&str>)> = ev
            .iter()
            .map(|e| (e.role, e.kind, e.text.as_str(), e.tool.as_deref()))
            .collect();
        assert_eq!(
            got,
            vec![
                (R::User, K::Text, "Fix the typo.", None),
                (R::System, K::Notice, "[amail] 1 new message for w", None),
                (R::Assistant, K::Text, "Looking.", None),
                (R::Assistant, K::ToolCall, "ls", Some("bash")),
                (R::Tool, K::ToolResult, "a.txt\n", Some("bash")),
                (
                    R::Assistant,
                    K::ToolCall,
                    "{\"path\":\"a.txt\"}",
                    Some("edit")
                ),
                (R::Tool, K::ToolResult, "no such file", Some("edit")),
                (R::System, K::Notice, "turn interrupted", None),
                (R::System, K::Notice, "turn ended: transport failed", None),
            ]
        );
        assert!(ev[6].error && !ev[4].error);
        assert_eq!(ev[0].ts.as_deref(), Some("2023-11-14T22:13:20.2Z"));
        let s = p.stats();
        assert_eq!(s.skipped, 1);
        // `session`, `turn/start` and `chunk` records.
        assert_eq!(s.ignored, 3);
    }
}
