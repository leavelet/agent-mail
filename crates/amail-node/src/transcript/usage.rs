//! How full a session's context is: the input tokens of the latest model
//! request, as the harness records them in its transcript, and the model's
//! context window. The node asks for the seat's handover note when the
//! input passes a share of the window (see [`crate::node`]).
//!
//! - Claude Code: assistant records carry `message.usage` with
//!   `input_tokens`, `cache_creation_input_tokens` and
//!   `cache_read_input_tokens`; the request's input is their sum (with
//!   prompt caching, `input_tokens` alone counts only the uncached part).
//!   The model is `message.model`.
//! - Codex: `event_msg` records of payload type `token_count` carry
//!   `info.last_token_usage.input_tokens` (cached input included) and
//!   `info.model_context_window`; `turn_context` records name the `model`.
//!
//! Neither format is a contract: records and fields the parser does not
//! know are skipped, and a missing figure is unknown (`None`), never zero.

use super::{ParseStats, parse_object, str_at};
use crate::adapter::Harness;
use serde_json::Value;

/// What the end of a transcript says about the context's size.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Usage {
    /// Input tokens of the latest model request.
    pub input_tokens: u64,
    /// The model, if the transcript names it.
    pub model: Option<String>,
    /// The context window, if the harness records it (Codex).
    pub context_window: Option<u64>,
}

fn num(v: Option<&Value>) -> Option<u64> {
    v.and_then(|x| x.as_u64().or_else(|| x.as_f64().map(|f| f as u64)))
}

/// The latest usage figure in `tail` (whole JSONL lines), if any.
pub fn last_usage(h: Harness, tail: &str) -> Option<Usage> {
    match h {
        Harness::Claude => claude(tail),
        Harness::Codex => codex(tail),
        Harness::Dsh => None,
    }
}

fn claude(tail: &str) -> Option<Usage> {
    let mut stats = ParseStats::default();
    let mut out = None;
    for line in tail.lines() {
        let Some(v) = parse_object(line, &mut stats) else {
            continue;
        };
        if str_at(&v, "type") != Some("assistant")
            || v.get("isSidechain").and_then(Value::as_bool) == Some(true)
        {
            continue;
        }
        let Some(u) = v.pointer("/message/usage") else {
            continue;
        };
        let parts = [
            num(u.get("input_tokens")),
            num(u.get("cache_creation_input_tokens")),
            num(u.get("cache_read_input_tokens")),
        ];
        if parts.iter().all(Option::is_none) {
            continue;
        }
        out = Some(Usage {
            input_tokens: parts.iter().flatten().sum(),
            model: v
                .pointer("/message/model")
                .and_then(Value::as_str)
                .filter(|m| !m.is_empty() && !m.starts_with('<'))
                .map(str::to_string),
            context_window: None,
        });
    }
    out
}

fn codex(tail: &str) -> Option<Usage> {
    let mut stats = ParseStats::default();
    let mut model: Option<String> = None;
    let mut out: Option<Usage> = None;
    for line in tail.lines() {
        let Some(v) = parse_object(line, &mut stats) else {
            continue;
        };
        let Some(p) = v.get("payload") else {
            continue;
        };
        match (str_at(&v, "type"), str_at(p, "type")) {
            (Some("turn_context"), _) => {
                if let Some(m) = str_at(p, "model").filter(|m| !m.is_empty()) {
                    model = Some(m.to_string());
                }
            }
            (Some("event_msg"), Some("token_count")) => {
                let Some(info) = p.get("info").filter(|i| !i.is_null()) else {
                    continue;
                };
                let Some(input) = num(info.pointer("/last_token_usage/input_tokens")) else {
                    continue;
                };
                out = Some(Usage {
                    input_tokens: input,
                    model: None,
                    context_window: num(info.get("model_context_window")).filter(|w| *w > 0),
                });
            }
            _ => {}
        }
    }
    out.map(|mut u| {
        u.model = model;
        u
    })
}

/// Context windows by model name: the first entry whose pattern the name
/// contains (case ignored) gives the window. Models not listed use
/// [`DEFAULT_CONTEXT_WINDOW`]. The node's `--context-window` replaces
/// these figures.
pub const CONTEXT_WINDOWS: &[(&str, u64)] = &[
    // Claude Code's 1M-context variants are named with `[1m]`.
    ("[1m]", 1_000_000),
    ("claude", 200_000),
    ("sonnet", 200_000),
    ("opus", 200_000),
    ("haiku", 200_000),
    ("gpt-4.1", 1_000_000),
    ("gpt-5", 272_000),
    ("gpt-6", 272_000),
    ("codex", 272_000),
    ("deepseek", 128_000),
];

/// The window of a model not in [`CONTEXT_WINDOWS`].
pub const DEFAULT_CONTEXT_WINDOW: u64 = 200_000;

/// The node's context-window settings (`--context-window`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContextWindows {
    /// `--context-window N`: every model has this window.
    pub all: Option<u64>,
    /// `--context-window MODEL=N`: models whose name contains `MODEL`.
    pub by_model: Vec<(String, u64)>,
}

impl ContextWindows {
    /// Read one `--context-window` value: `N` or `MODEL=N` (`N` may end in
    /// `k` or `m`).
    pub fn add(&mut self, spec: &str) -> Result<(), String> {
        fn tokens(s: &str) -> Result<u64, String> {
            let t = s.trim().to_ascii_lowercase().replace('_', "");
            let (digits, mul) = if let Some(d) = t.strip_suffix('k') {
                (d, 1_000)
            } else if let Some(d) = t.strip_suffix('m') {
                (d, 1_000_000)
            } else {
                (t.as_str(), 1)
            };
            match digits.parse::<u64>() {
                Ok(n) if n > 0 => Ok(n * mul),
                _ => Err(format!("{s:?} is not a number of tokens")),
            }
        }
        match spec.split_once('=') {
            Some((m, n)) if !m.trim().is_empty() => {
                self.by_model
                    .push((m.trim().to_ascii_lowercase(), tokens(n)?));
            }
            Some(_) => return Err(format!("{spec:?}: give MODEL=N or N")),
            None => self.all = Some(tokens(spec)?),
        }
        Ok(())
    }

    /// The window for `model`, and where the figure came from: the node's
    /// setting, then what the harness recorded, then the table, then the
    /// default.
    pub fn window(&self, model: Option<&str>, recorded: Option<u64>) -> (u64, &'static str) {
        if let Some(n) = self.all {
            return (n, "--context-window");
        }
        let lower = model.map(str::to_ascii_lowercase);
        if let Some(m) = &lower
            && let Some((_, n)) = self.by_model.iter().rev().find(|(p, _)| m.contains(p))
        {
            return (*n, "--context-window MODEL=N");
        }
        if let Some(n) = recorded {
            return (n, "the harness's transcript");
        }
        if let Some(m) = &lower
            && let Some((_, n)) = CONTEXT_WINDOWS.iter().find(|(p, _)| m.contains(p))
        {
            return (*n, "the node's table");
        }
        (DEFAULT_CONTEXT_WINDOW, "the default")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_usage_sums_cached_input() {
        let tail = concat!(
            r#"{"type":"user","message":{"role":"user","content":"hi"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"model":"claude-sonnet-4","usage":{"input_tokens":10,"cache_creation_input_tokens":200,"cache_read_input_tokens":3000,"output_tokens":5}}}"#,
            "\n",
            r#"{"type":"assistant","isSidechain":true,"message":{"usage":{"input_tokens":999999}}}"#,
            "\n",
            "not json\n",
            r#"{"type":"assistant","message":{"content":[]}}"#,
            "\n",
        );
        let u = last_usage(Harness::Claude, tail).unwrap();
        assert_eq!(u.input_tokens, 3210);
        assert_eq!(u.model.as_deref(), Some("claude-sonnet-4"));
        assert_eq!(last_usage(Harness::Claude, "{}\n"), None);
    }

    #[test]
    fn codex_usage_from_token_count() {
        let tail = concat!(
            r#"{"type":"turn_context","payload":{"model":"gpt-5-codex"}}"#,
            "\n",
            r#"{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":150000,"cached_input_tokens":100000},"model_context_window":272000}}}"#,
            "\n",
            r#"{"type":"event_msg","payload":{"type":"token_count","info":null}}"#,
            "\n",
        );
        let u = last_usage(Harness::Codex, tail).unwrap();
        assert_eq!(u.input_tokens, 150_000);
        assert_eq!(u.context_window, Some(272_000));
        assert_eq!(u.model.as_deref(), Some("gpt-5-codex"));
        assert_eq!(
            last_usage(
                Harness::Codex,
                r#"{"type":"event_msg","payload":{"type":"token_count"}}"#
            ),
            None
        );
    }

    #[test]
    fn windows() {
        let mut w = ContextWindows::default();
        assert_eq!(w.window(Some("claude-opus-4[1m]"), None).0, 1_000_000);
        assert_eq!(w.window(Some("claude-sonnet-4"), None).0, 200_000);
        assert_eq!(w.window(Some("unknown"), None).0, DEFAULT_CONTEXT_WINDOW);
        assert_eq!(w.window(None, Some(272_000)).0, 272_000);
        w.add("sonnet=500k").unwrap();
        assert_eq!(w.window(Some("claude-sonnet-4"), Some(1)).0, 500_000);
        w.add("100000").unwrap();
        assert_eq!(w.window(Some("claude-sonnet-4"), None).0, 100_000);
        assert!(w.add("x").is_err());
        assert!(w.add("=5").is_err());
        assert!(w.add("m=0").is_err());
    }
}
