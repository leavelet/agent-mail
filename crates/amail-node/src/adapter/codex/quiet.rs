//! A narrow fallback for unavailable turn metadata. Only public lifecycle
//! fields are decoded; message text, tool bodies and encrypted content are
//! ignored by serde and are never mirrored or logged here.

use super::Codex;
use crate::adapter::{QuietFileProof, QuietTurnContext, QuietTurnProof, TurnInfo, TurnStatus};
use anyhow::Result;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

const HEAD_LIMIT: u64 = 512 << 10;
const TAIL_LIMIT: u64 = 4 << 20;
const MAX_TAIL_LIMIT: u64 = 64 << 20;

#[derive(Default, Deserialize)]
struct Fields {
    #[serde(rename = "type")]
    kind: Option<String>,
    id: Option<String>,
    turn_id: Option<String>,
    call_id: Option<String>,
    thread_id: Option<String>,
    item: Option<ItemMetadata>,
    #[serde(default, rename = "error", deserialize_with = "error_present")]
    terminal_error: bool,
}

/// Only routing/completion tags are decoded, never command/output/reasoning.
#[derive(Default, Deserialize)]
struct ItemMetadata {
    #[serde(rename = "type")]
    kind: Option<String>,
    id: Option<String>,
    status: Option<String>,
}

fn error_present<'de, D: serde::Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    // Preserve only presence. Do not decode or log error message contents.
    Ok(Option::<serde::de::IgnoredAny>::deserialize(d)?.is_some())
}

#[derive(Deserialize)]
struct Record {
    #[serde(rename = "type")]
    kind: String,
    timestamp: Option<String>,
    payload: Fields,
}

fn time(r: &Record) -> Option<i64> {
    r.timestamp
        .as_ref()?
        .parse::<jiff::Timestamp>()
        .ok()
        .map(|t| t.as_millisecond())
}

fn metadata_error(e: &anyhow::Error) -> bool {
    e.chain()
        .filter_map(|e| e.downcast_ref::<crate::codex_rpc::RpcError>())
        .any(|e| {
            e.code == Some(-32603)
                && e.message.starts_with("failed to list thread history:")
                && e.message.contains("failed to read thread metadata")
                && e.message.contains("(code: 11)")
                && e.message.contains("database disk image is malformed")
        })
}

/// A complete start/end pair in the bounded tail is required. An unknown
/// lifecycle event, a failed/aborted turn, a later prompt or partial JSON
/// cannot stand in for normal completion.
fn completed_in_tail(tail: &str, p: &QuietTurnContext) -> Option<TurnInfo> {
    if !tail.ends_with('\n') {
        return None;
    }
    let mut started: Option<(String, i64)> = None;
    let mut completed = None;
    let mut invalid = false;
    let mut pending_calls = BTreeSet::new();
    let mut paired_calls = BTreeSet::new();
    let mut completed_calls: BTreeSet<(String, String)> = BTreeSet::new();
    for line in tail.lines().filter(|l| !l.trim().is_empty()) {
        let r: Record = serde_json::from_str(line).ok()?;
        if r.kind != "event_msg" {
            // Once completed, any subsequent non-metadata record might be a
            // new input whose task_started has not reached disk yet.
            if completed.is_some() {
                invalid = true;
                completed = None;
            } else if started.is_some() && r.kind == "response_item" {
                match r.payload.kind.as_deref() {
                    Some("function_call" | "custom_tool_call") => {
                        let id = r.payload.call_id.filter(|s| !s.is_empty())?;
                        if !pending_calls.insert(id) {
                            invalid = true;
                        }
                    }
                    Some("function_call_output" | "custom_tool_call_output") => {
                        let id = r.payload.call_id.filter(|s| !s.is_empty())?;
                        if !pending_calls.remove(&id) {
                            invalid = true;
                        } else {
                            paired_calls.insert(id);
                        }
                    }
                    Some("message" | "reasoning" | "compaction") => {}
                    _ => invalid = true,
                }
            }
            continue;
        }
        match r.payload.kind.as_deref()? {
            "task_started" => {
                let id = r.payload.turn_id.clone().filter(|s| !s.is_empty())?;
                started = Some((id, time(&r)?));
                completed = None;
                invalid = false;
                pending_calls.clear();
                paired_calls.clear();
            }
            "task_complete" => {
                let Some((id, at)) = started.as_ref() else {
                    // The bounded window may start inside an earlier turn.
                    continue;
                };
                let end = time(&r)?;
                if invalid
                    || r.payload.terminal_error
                    || !pending_calls.is_empty()
                    || r.payload.turn_id.as_deref() != Some(id.as_str())
                    || end < *at
                {
                    invalid = true;
                    completed = None;
                    completed_calls.retain(|(turn, _)| turn != id);
                    continue;
                }
                for call in &paired_calls {
                    completed_calls.insert((id.clone(), call.clone()));
                }
                completed = Some((
                    TurnInfo {
                        status: TurnStatus::Completed,
                        started_at: Some(at.div_euclid(1000)),
                        id: Some(id.clone()),
                    },
                    end,
                ));
            }
            "token_count" => {}
            "agent_message" | "agent_reasoning" if completed.is_none() => {}
            // A settings notification within an existing turn does not
            // start or complete work. Its configuration body is ignored.
            "thread_settings_applied" if started.is_some() && completed.is_none() => {}
            // Current Codex emits item completion notifications alongside
            // response_item records. They are not turn completion evidence;
            // ignore their bodies only within the same open turn.
            "item_completed"
                if completed.is_none()
                    && started
                        .as_ref()
                        .is_some_and(|(id, _)| r.payload.turn_id.as_ref() == Some(id)) => {}
            // A delayed command notification is not new work when its exact
            // thread/turn/call already has a matched response and normal end.
            "item_completed"
                if !r.payload.terminal_error
                    && r.payload.thread_id.as_deref() == Some(p.target.as_str())
                    && r.payload.item.as_ref().is_some_and(|item| {
                        item.kind.as_deref() == Some("CommandExecution")
                            && item.status.as_deref() == Some("completed")
                            && r.payload
                                .turn_id
                                .as_ref()
                                .zip(item.id.as_ref())
                                .is_some_and(|(turn, call)| {
                                    !turn.is_empty()
                                        && !call.is_empty()
                                        && completed_calls.contains(&(turn.clone(), call.clone()))
                                })
                    }) => {}
            _ => {
                invalid = true;
                completed = None;
                // A later unknown/setting record invalidates the current
                // observation, not the fact that an earlier call returned.
                // Only an explicit failure/abort revokes that turn's cache.
                if r.payload.terminal_error
                    || matches!(r.payload.kind.as_deref(), Some("error" | "turn_aborted"))
                {
                    if let Some(turn) = r
                        .payload
                        .turn_id
                        .as_ref()
                        .or_else(|| started.as_ref().map(|(id, _)| id))
                    {
                        completed_calls.retain(|(id, _)| id != turn);
                    } else {
                        completed_calls.clear();
                    }
                }
            }
        }
    }
    if invalid {
        return None;
    }
    completed
        .filter(|(_, end)| {
            (p.restoring || (*end >= p.busy_since && *end >= p.last_hook))
                && *end <= jiff::Timestamp::now().as_millisecond()
        })
        .map(|(turn, _)| turn)
}

/// A missing current or referenced older turn start can be helped by reading
/// an earlier bounded window. This is only an I/O hint, never acceptance.
/// Do not spend more I/O trying to turn a parse error or failure into success.
fn needs_earlier_evidence(tail: &str) -> bool {
    let mut starts = BTreeSet::new();
    let mut referenced = BTreeSet::new();
    for line in tail.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(r) = serde_json::from_str::<Record>(line) else {
            return false;
        };
        if r.kind == "event_msg"
            && (r.payload.terminal_error
                || matches!(r.payload.kind.as_deref(), Some("error" | "turn_aborted")))
        {
            return false;
        }
        if r.kind == "event_msg" && r.payload.kind.as_deref() == Some("task_started") {
            let Some(id) = r.payload.turn_id.filter(|id| !id.is_empty()) else {
                return false;
            };
            starts.insert(id);
        } else if r.kind == "event_msg"
            && r.payload.kind.as_deref() == Some("item_completed")
            && r.payload.item.as_ref().is_some_and(|i| {
                i.kind.as_deref() == Some("CommandExecution")
                    && i.status.as_deref() == Some("completed")
                    && i.id.as_ref().is_some_and(|id| !id.is_empty())
            })
            && r.payload.turn_id.as_ref().is_some_and(|id| !id.is_empty())
        {
            referenced.insert(r.payload.turn_id.unwrap());
        }
    }
    starts.is_empty() || referenced.iter().any(|id| !starts.contains(id))
}

/// Read a bounded window through one file handle and reject any observed
/// replacement, append, partial record, ownership or session mismatch.
fn snapshot(home: &Path, p: &QuietTurnContext) -> std::io::Result<Option<QuietTurnProof>> {
    if !crate::transcript::path_under(&p.transcript, &home.join("sessions")) {
        return Ok(None);
    }
    let path = p.transcript.canonicalize()?;
    let mut f = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)?;
    let meta = f.metadata()?;
    if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() } {
        return Ok(None);
    }
    let before = QuietFileProof::new(path.clone(), &meta);
    let mut head = Vec::new();
    BufReader::new((&mut f).take(HEAD_LIMIT)).read_until(b'\n', &mut head)?;
    if !head.ends_with(b"\n") {
        return Ok(None);
    }
    let Ok(first) = serde_json::from_slice::<Record>(&head) else {
        return Ok(None);
    };
    if first.kind != "session_meta" || first.payload.id.as_deref() != Some(p.target.as_str()) {
        return Ok(None);
    }
    let mut limit = TAIL_LIMIT;
    let turn = loop {
        let start = meta.len().saturating_sub(limit);
        f.seek(SeekFrom::Start(start))?;
        let mut bytes = Vec::new();
        (&mut f).take(limit).read_to_end(&mut bytes)?;
        let whole = if start == 0 {
            bytes.as_slice()
        } else if let Some(n) = bytes.iter().position(|b| *b == b'\n') {
            &bytes[n + 1..]
        } else {
            &[]
        };
        let Ok(text) = std::str::from_utf8(whole) else {
            return Ok(None);
        };
        if let Some(turn) = completed_in_tail(text, p) {
            break turn;
        }
        if start == 0 || limit >= MAX_TAIL_LIMIT || !needs_earlier_evidence(text) {
            return Ok(None);
        }
        limit = (limit * 2).min(MAX_TAIL_LIMIT);
    };
    if QuietFileProof::new(path, &f.metadata()?) != before || !before.still_current(&p.transcript) {
        return Ok(None);
    }
    Ok(Some(QuietTurnProof { turn, file: before }))
}

fn idle_empty(thread: &Value, queue: &Value, p: &QuietTurnContext) -> bool {
    let t = &thread["thread"];
    t["id"].as_str() == Some(p.target.as_str())
        && t["status"]["type"].as_str() == Some("idle")
        && t["path"].as_str().is_some_and(|s| {
            Path::new(s).canonicalize().ok() == p.transcript.canonicalize().ok()
                && p.transcript.canonicalize().is_ok()
        })
        && queue["data"].as_array().is_some_and(|a| a.is_empty())
        && queue.get("nextCursor") == Some(&Value::Null)
}

pub(super) async fn recover(
    codex: &Codex,
    p: &QuietTurnContext,
    error: Option<&anyhow::Error>,
) -> Result<Option<QuietTurnProof>> {
    let eligible = match error {
        Some(error) => metadata_error(error),
        None => p.restoring,
    };
    if !eligible {
        tracing::debug!(target: "amail_quiet_proof", thread = %p.target,
            stage = "history_gate", "quiet completion proof not eligible");
        return Ok(None);
    }
    let home = codex.home();
    let ctx = p.clone();
    let read = move || snapshot(&home, &ctx);
    let Some(before) = tokio::task::spawn_blocking(read).await?? else {
        tracing::debug!(target: "amail_quiet_proof", thread = %p.target,
            stage = "snapshot_before", "quiet completion proof unavailable");
        return Ok(None);
    };
    // All four reads use one connection to the original configured backend.
    // No turn content is requested, and no queue item is consumed.
    let r = crate::codex_rpc::socket_calls(
        &codex.control_socket(),
        &[
            (
                "thread/read",
                json!({"threadId": p.target, "includeTurns": false}),
            ),
            ("thread/queue/list", json!({"threadId": p.target})),
            (
                "thread/read",
                json!({"threadId": p.target, "includeTurns": false}),
            ),
            ("thread/queue/list", json!({"threadId": p.target})),
        ],
    )
    .await?;
    if r.len() != 4 || !idle_empty(&r[0], &r[1], p) || !idle_empty(&r[2], &r[3], p) {
        tracing::debug!(target: "amail_quiet_proof", thread = %p.target,
            stage = "backend_observation", "quiet completion proof unavailable");
        return Ok(None);
    }
    let home = codex.home();
    let ctx = p.clone();
    let after = tokio::task::spawn_blocking(move || snapshot(&home, &ctx)).await??;
    let matched = after.filter(|after| *after == before);
    if matched.is_none() {
        tracing::debug!(target: "amail_quiet_proof", thread = %p.target,
            stage = "snapshot_after", "quiet completion proof changed");
    }
    Ok(matched)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(path: std::path::PathBuf) -> QuietTurnContext {
        QuietTurnContext {
            target: "s".into(),
            transcript: path,
            busy_since: 1000,
            last_hook: 2000,
            restoring: false,
        }
    }

    fn line(kind: &str, id: &str, seconds: u32) -> String {
        json!({"type":"event_msg", "timestamp":format!("1970-01-01T00:00:{seconds:02}Z"),
            "payload":{"type":kind,"turn_id":id}})
        .to_string()
            + "\n"
    }

    fn completed() -> String {
        line("task_started", "t", 1) + &line("task_complete", "t", 3)
    }

    #[test]
    fn only_the_structured_metadata_corruption_error_is_eligible() {
        let message = "failed to list thread history: thread-store internal error: failed to read thread metadata: error returned from database: (code: 11) database disk image is malformed";
        let rpc = |code: i64, msg: &str| -> anyhow::Error {
            crate::codex_rpc::RpcError::new(json!({"code":code,"message":msg})).into()
        };
        assert!(metadata_error(&rpc(-32603, message).context("outer")));
        assert!(!metadata_error(&rpc(-32001, message)));
        assert!(!metadata_error(&rpc(-32603, "cyber_policy refusal")));
        assert!(!metadata_error(&rpc(
            -32603,
            "failed to read thread metadata: timeout"
        )));
        assert!(!metadata_error(&anyhow::anyhow!(message)));
    }

    #[test]
    fn completion_requires_a_fresh_matched_normal_pair() {
        let p = p("/unused".into());
        assert!(completed_in_tail(&completed(), &p).is_some());
        let previous = line("task_complete", "partial-previous", 0)
            + &line("task_started", "previous", 0)
            + &line("task_complete", "previous", 1)
            + "{\"type\":\"turn_context\",\"payload\":{}}\n"
            + &completed();
        assert_eq!(
            completed_in_tail(&previous, &p),
            completed_in_tail(&completed(), &p)
        );
        for text in [
            line("task_complete", "t", 3),
            line("task_started", "t", 1) + &line("task_complete", "other", 3),
            line("task_started", "t", 1) + &line("task_complete", "t", 1),
            line("task_started", "t", 1) + &line("error", "t", 2) + &line("task_complete", "t", 3),
            completed() + &line("task_started", "next", 4),
            completed() + &line("turn_aborted", "t", 4),
            completed() + "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\"}}\n",
            completed().trim_end().to_string(),
            completed() + "not-json\n",
            line("task_started", "t", 1)
                + "{\"type\":\"response_item\",\"payload\":{\"type\":\"function_call\",\"call_id\":\"call\"}}\n"
                + &line("task_complete", "t", 3),
        ] {
            assert!(
                completed_in_tail(&text, &p).is_none(),
                "accepted an unproven completion"
            );
        }
    }

    #[test]
    fn irrelevant_private_fields_are_not_used_as_lifecycle_evidence() {
        let text = line("task_started", "t", 1)
            + "{\"type\":\"response_item\",\"payload\":{\"type\":\"reasoning\",\"encrypted_content\":\"ignored\",\"content\":[{\"text\":\"task_complete\"}]}}\n"
            + &line("task_complete", "t", 3);
        assert!(completed_in_tail(&text, &p("/unused".into())).is_some());
        let paired = line("task_started", "t", 1)
            + "{\"type\":\"response_item\",\"payload\":{\"type\":\"function_call\",\"call_id\":\"call\",\"arguments\":\"not decoded\"}}\n"
            + "{\"type\":\"response_item\",\"payload\":{\"type\":\"function_call_output\",\"call_id\":\"call\",\"output\":\"not decoded\"}}\n"
            + &line("task_complete", "t", 3);
        assert!(completed_in_tail(&paired, &p("/unused".into())).is_some());
    }

    #[test]
    fn terminal_error_presence_is_never_normal_completion() {
        for error in [
            json!({"message":"HTTP 400 Invalid model name", "codex_error_info":"other"}),
            json!({}),
            json!("failure"),
            json!(false),
        ] {
            let terminal = json!({"type":"event_msg", "timestamp":"1970-01-01T00:00:03Z",
                "payload":{"type":"task_complete", "turn_id":"t", "error":error}})
            .to_string()
                + "\n";
            assert!(
                completed_in_tail(
                    &(line("task_started", "t", 1) + &terminal),
                    &p("/unused".into())
                )
                .is_none()
            );
        }
        let terminal = "{\"type\":\"event_msg\",\"timestamp\":\"1970-01-01T00:00:03Z\",\"payload\":{\"type\":\"task_complete\",\"turn_id\":\"t\",\"error\":null}}\n";
        assert!(
            completed_in_tail(
                &(line("task_started", "t", 1) + terminal),
                &p("/unused".into())
            )
            .is_some()
        );
    }

    #[test]
    fn real_public_lifecycle_shape_accepts_only_same_turn_item_notifications() {
        // Projected from an actual normal turn: only structural fields,
        // mapped IDs and normalized timestamps; no message/item/tool bodies.
        let text = include_str!("fixtures/quiet-completed.jsonl");
        let ctx = p("/unused".into());
        assert_eq!(
            completed_in_tail(text, &ctx).unwrap().id.as_deref(),
            Some("t")
        );
        assert!(
            completed_in_tail(&(text.to_string() + &line("item_completed", "t", 20)), &ctx)
                .is_none()
        );
        for id in ["other", ""] {
            let wrong = line("task_started", "t", 1)
                + &line("item_completed", id, 2)
                + &line("task_complete", "t", 3);
            assert!(completed_in_tail(&wrong, &ctx).is_none());
        }
        let unknown = line("task_started", "t", 1)
            + &line("unknown_event", "t", 2)
            + &line("task_complete", "t", 3);
        assert!(completed_in_tail(&unknown, &ctx).is_none());
    }

    #[test]
    fn long_normal_turn_reads_back_to_its_start_without_relaxing_failures() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sessions")).unwrap();
        let path = dir.path().join("sessions/long.jsonl");
        let head = json!({"type":"session_meta", "payload":{"id":"s"}}).to_string() + "\n";
        // A large ignored world-state body pushes the start out of 4 MiB.
        let large = json!({"type":"world_state", "payload":{"ignored":"x".repeat((TAIL_LIMIT+1024) as usize)}}).to_string()+"\n";
        let text = head
            + &line("task_started", "t", 1)
            + &large
            + &line("thread_settings_applied", "t", 2)
            + &line("task_complete", "t", 3);
        std::fs::write(&path, &text).unwrap();
        assert!(snapshot(dir.path(), &p(path.clone())).unwrap().is_some());
        let failed = text.replace(
            "\"type\":\"task_complete\"",
            "\"error\":{\"message\":\"failure\"},\"type\":\"task_complete\"",
        );
        std::fs::write(&path, failed).unwrap();
        assert!(snapshot(dir.path(), &p(path)).unwrap().is_none());
        assert!(!needs_earlier_evidence(&completed()));
        assert!(!needs_earlier_evidence("not-json\n"));
        assert!(!needs_earlier_evidence(&line("turn_aborted", "t", 3)));
        assert!(
            completed_in_tail(
                &(completed() + &line("thread_settings_applied", "t", 4)),
                &p("/unused".into())
            )
            .is_none()
        );
    }

    #[test]
    fn snapshot_requires_registered_session_path_and_complete_file() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = dir.path().join("sessions");
        std::fs::create_dir(&sessions).unwrap();
        let path = sessions.join("rollout.jsonl");
        let head = json!({"type":"session_meta","payload":{"id":"s"}}).to_string() + "\n";
        std::fs::write(&path, head.clone() + &completed()).unwrap();
        assert!(snapshot(dir.path(), &p(path.clone())).unwrap().is_some());
        let mut wrong = p(path.clone());
        wrong.target = "other".into();
        assert!(snapshot(dir.path(), &wrong).unwrap().is_none());
        std::fs::write(&path, head + &completed() + "{\"type\":").unwrap();
        assert!(snapshot(dir.path(), &p(path.clone())).unwrap().is_none());
        let outside = dir.path().join("outside.jsonl");
        std::fs::rename(&path, &outside).unwrap();
        assert!(snapshot(dir.path(), &p(outside)).unwrap().is_none());
    }

    #[test]
    fn late_commands_require_the_exact_completed_turn_and_call_pair() {
        let response = |kind: &str, call: &str| {
            json!({"type":"response_item",
            "payload":{"type":kind,"call_id":call}})
            .to_string()
                + "\n"
        };
        let late = |thread: &str, turn: &str, call: &str, kind: &str, status: &str| {
            json!({"type":"event_msg","timestamp":"1970-01-01T00:00:09Z",
                "payload":{"type":"item_completed","thread_id":thread,"turn_id":turn,
                "item":{"type":kind,"id":call,"status":status}}})
            .to_string()
                + "\n"
        };
        let ctx = p("/unused".into());
        let old = line("task_started", "old", 1)
            + &response("function_call", "c")
            + &response("function_call_output", "c")
            + &line("task_complete", "old", 3)
            + &line("thread_settings_applied", "old", 3);
        let current = line("task_started", "new", 4)
            + &response("function_call", "n")
            + &response("function_call_output", "n");
        let old_late = late("s", "old", "c", "CommandExecution", "completed");
        let text = old.clone()
            + &current
            + &old_late
            + &line("task_complete", "new", 6)
            + &old_late
            + &late("s", "new", "n", "CommandExecution", "completed");
        assert_eq!(
            completed_in_tail(&text, &ctx).unwrap().id.as_deref(),
            Some("new")
        );
        let normal = old.clone() + &current + &line("task_complete", "new", 6);
        for (thread, turn, call, kind, status) in [
            ("other", "old", "c", "CommandExecution", "completed"),
            ("s", "unknown", "c", "CommandExecution", "completed"),
            ("s", "old", "missing", "CommandExecution", "completed"),
            ("s", "old", "c", "AgentMessage", "completed"),
            ("s", "old", "c", "CommandExecution", "inProgress"),
        ] {
            assert!(
                completed_in_tail(
                    &(normal.clone() + &late(thread, turn, call, kind, status)),
                    &ctx
                )
                .is_none()
            );
        }
        let unpaired = old.replace(&response("function_call_output", "c"), "");
        assert!(
            completed_in_tail(
                &(unpaired + &current + &line("task_complete", "new", 6) + &old_late),
                &ctx
            )
            .is_none()
        );
        let failed = old.replace(
            "\"type\":\"task_complete\"",
            "\"error\":{},\"type\":\"task_complete\"",
        );
        assert!(
            completed_in_tail(
                &(failed + &current + &line("task_complete", "new", 6) + &old_late),
                &ctx
            )
            .is_none()
        );
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sessions")).unwrap();
        let path = dir.path().join("sessions/late.jsonl");
        let head = json!({"type":"session_meta","payload":{"id":"s"}}).to_string() + "\n";
        let gap = json!({"type":"world_state","payload":{"ignored":"x".repeat((TAIL_LIMIT+1024) as usize)}}).to_string()+"\n";
        // The newest start is in 4 MiB, but the referenced old turn is not.
        std::fs::write(
            &path,
            head + &old + &gap + &current + &line("task_complete", "new", 6) + &old_late,
        )
        .unwrap();
        assert!(snapshot(dir.path(), &p(path)).unwrap().is_some());
    }

    #[test]
    fn complete_real_late_notification_windows_preserve_past_call_evidence() {
        let mut ctx = p("/unused".into());
        ctx.restoring = true;
        for text in [
            include_str!("fixtures/quiet-u00-late.jsonl"),
            include_str!("fixtures/quiet-u11-late.jsonl"),
        ] {
            assert_eq!(
                completed_in_tail(text, &ctx).unwrap().id.as_deref(),
                Some("current")
            );
        }
    }

    /// Maintenance-only replay of a body-free projection. No backend calls,
    /// presence changes, or raw transcript contents are exposed.
    #[test]
    #[ignore = "requires an explicitly supplied public-metadata fixture"]
    fn audit_public_metadata_projection() {
        let fixture = std::path::PathBuf::from(
            std::env::var("AMAIL_QUIET_PUBLIC_FIXTURE").expect("public fixture path"),
        );
        let name = fixture.file_name().unwrap().to_string_lossy();
        assert!(name.ends_with("-public-metadata.jsonl")
            || name.ends_with("-window-preserved-metadata.jsonl"));
        let text = std::fs::read_to_string(&fixture).unwrap();
        let first: Record = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        let target = first.payload.id.unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sessions")).unwrap();
        let path = dir.path().join("sessions/projected.jsonl");
        std::fs::write(&path, &text).unwrap();
        let ctx = QuietTurnContext {
            target,
            transcript: path,
            busy_since: std::env::var("AMAIL_QUIET_BUSY_MS").unwrap().parse().unwrap(),
            last_hook: std::env::var("AMAIL_QUIET_HOOK_MS").unwrap().parse().unwrap(),
            restoring: false,
        };
        for (index, line) in text.lines().enumerate() {
            if let Err(error) = serde_json::from_str::<Record>(line) {
                panic!("metadata parse rejected line {} category {:?} column {}",
                    index + 1, error.classify(), error.column());
            }
        }
        let turn = completed_in_tail(&text, &ctx);
        let proof = snapshot(dir.path(), &ctx).unwrap();
        eprintln!("public projection: records={}, matched_normal={}, snapshot={}, earlier_hint={}",
            text.lines().count(), turn.is_some(), proof.is_some(), needs_earlier_evidence(&text));
        assert!(turn.is_some(), "public lifecycle proof rejected");
        assert!(proof.is_some(), "public file proof rejected");
    }

    #[test]
    fn backend_identity_idle_and_full_queue_response_are_all_required() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout.jsonl");
        std::fs::File::create(&path).unwrap();
        let p = p(path);
        let thread = json!({"thread":{"id":"s","status":{"type":"idle"},"path":p.transcript}});
        let queue = json!({"data":[],"nextCursor":null});
        assert!(idle_empty(&thread, &queue, &p));
        assert!(!idle_empty(
            &thread,
            &json!({"data":[{}],"nextCursor":null}),
            &p
        ));
        assert!(!idle_empty(
            &thread,
            &json!({"data":[],"nextCursor":"more"}),
            &p
        ));
        assert!(!idle_empty(&thread, &json!({"data":[]}), &p));
        let mut active = thread.clone();
        active["thread"]["status"]["type"] = json!("active");
        assert!(!idle_empty(&active, &queue, &p));
        active = thread.clone();
        active["thread"]["id"] = json!("other");
        assert!(!idle_empty(&active, &queue, &p));
    }
}
