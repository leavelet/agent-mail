//! Adopting existing harness sessions as seats: the team file's `session`,
//! `amail run --session` and `amail seat adopt`. Before a seat takes a
//! session id the node checks that the session exists on this host (in the
//! place the harness resumes it from) and that no other seat has it.

use super::*;
use crate::wire::SessionClaim;
use std::io::{BufRead, BufReader as StdBufReader, Read};
use std::path::Path;

/// How many bytes of a transcript's start are read to find its cwd.
const HEAD_BYTES: u64 = 256 * 1024;

/// What the check found about an adoptable session.
#[derive(Debug, Clone, Default)]
pub(super) struct Found {
    /// The directory the session was started in, when the harness records it.
    pub cwd: Option<String>,
    /// Something the person should know (the session belongs to another
    /// directory).
    pub warning: Option<String>,
}

/// The first `cwd` field among the first records of a JSONL file (Claude
/// Code records carry it at the top level, Codex's `session_meta` in its
/// payload).
fn jsonl_cwd(path: &Path) -> Option<String> {
    let f = std::fs::File::open(path).ok()?;
    let r = StdBufReader::new(f.take(HEAD_BYTES));
    for line in r.lines().take(200) {
        let Ok(line) = line else { break };
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        for c in [&v["cwd"], &v["payload"]["cwd"]] {
            if let Some(c) = c.as_str().filter(|c| !c.is_empty()) {
                return Some(c.to_string());
            }
        }
    }
    None
}

/// Claude Code: `<config dir>/projects/<cwd key>/<id>.jsonl`, under any cwd
/// key. Returns the transcript.
pub(super) fn find_claude(projects: &Path, id: &str) -> Option<PathBuf> {
    let dir = std::fs::read_dir(projects).ok()?;
    let mut found: Vec<PathBuf> = dir
        .flatten()
        .map(|e| e.path().join(format!("{id}.jsonl")))
        .filter(|p| p.is_file())
        .collect();
    found.sort();
    found.into_iter().next()
}

/// Whether the first line of a Codex rollout file is the `session_meta` of
/// thread `id`.
fn rollout_is(path: &Path, id: &str) -> bool {
    let Ok(f) = std::fs::File::open(path) else {
        return false;
    };
    let mut line = String::new();
    if StdBufReader::new(f.take(HEAD_BYTES))
        .read_line(&mut line)
        .is_err()
    {
        return false;
    }
    serde_json::from_str::<Value>(&line)
        .is_ok_and(|v| v["type"] == "session_meta" && v["payload"]["id"].as_str() == Some(id))
}

/// Codex: the rollout file under `<CODEX_HOME>/sessions` (in `YYYY/MM/DD`
/// directories) whose `session_meta` names thread `id`. Files whose name
/// contains the id are looked at first (Codex names them
/// `rollout-<time>-<id>.jsonl`), then every other rollout file.
pub(super) fn find_codex_rollout(sessions: &Path, id: &str) -> Option<PathBuf> {
    fn walk(dir: &Path, depth: u32, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            let Ok(t) = e.file_type() else { continue };
            if t.is_dir() && depth < 4 {
                walk(&p, depth + 1, out);
            } else if t.is_file() && p.extension().is_some_and(|x| x == "jsonl") {
                out.push(p);
            }
        }
    }
    let mut files = Vec::new();
    walk(sessions, 0, &mut files);
    files.sort();
    let named = |p: &PathBuf| {
        p.file_name()
            .is_some_and(|n| n.to_string_lossy().contains(id))
    };
    let (first, rest): (Vec<PathBuf>, Vec<PathBuf>) = files.into_iter().partition(named);
    first.into_iter().chain(rest).find(|p| rollout_is(p, id))
}

/// dsh: the session's directory under the seat's dsh home,
/// `<seat home>/sessions/<project key>/<encoded id>/`.
pub(super) fn find_dsh(seat_home: &Path, id: &str) -> Option<PathBuf> {
    let seg = crate::adapter::dsh::encode_segment(id);
    let dir = std::fs::read_dir(seat_home.join("sessions")).ok()?;
    let mut found: Vec<PathBuf> = dir
        .flatten()
        .map(|e| e.path().join(&seg))
        .filter(|p| p.is_dir())
        .collect();
    found.sort();
    found.into_iter().next()
}

fn same_dir(a: &str, b: &str) -> bool {
    let canon = |p: &str| std::fs::canonicalize(p).unwrap_or_else(|_| PathBuf::from(p));
    a == b || canon(a) == canon(b)
}

/// The seat other than `seat` that has session `id` on this node, if any:
/// it is the session a supervised seat resumes, or a registered or live
/// session of another seat.
pub(super) fn owner_of(st: &State, id: &str, seat: &str) -> Option<String> {
    st.saved
        .managed
        .iter()
        .find(|(name, m)| name.as_str() != seat && m.resume.as_deref() == Some(id))
        .map(|(name, _)| name.clone())
        .or_else(|| {
            st.saved
                .sessions
                .get(id)
                .filter(|r| r.seat != seat)
                .map(|r| r.seat.clone())
        })
        .or_else(|| {
            st.saved
                .live
                .iter()
                .find(|(name, l)| name.as_str() != seat && l.session_id.as_deref() == Some(id))
                .map(|(name, _)| name.clone())
        })
}

/// Record that supervised seat `seat` adopts session `id`: its next start
/// resumes it and tells it that it is now a seat. Sessions of harnesses
/// that tie hooks to seats by registration (Codex) are registered now,
/// because a resumed session has no registration marker.
pub(super) fn adopt_into(st: &mut State, seat: &str, id: &str, registers: bool) {
    if let Some(m) = st.saved.managed.get_mut(seat) {
        m.resume = Some(id.to_string());
        m.adopt_intro = true;
        m.context_lost = false;
        m.adopt_failed = None;
    }
    if registers {
        register(st, seat, id);
    }
    st.save_dirty = true;
}

/// Tie session `id` to `seat` for hook identification (Codex).
pub(super) fn register(st: &mut State, seat: &str, id: &str) {
    if st.saved.sessions.get(id).is_none_or(|r| r.seat != seat) {
        st.saved.sessions.insert(
            id.to_string(),
            SessionReg {
                seat: seat.to_string(),
                pid: None,
            },
        );
        st.save_dirty = true;
    }
}

impl Node {
    /// Check that `c.seat` can adopt session `c.session` of harness
    /// `c.harness` on this host.
    pub(super) async fn check_claim(&self, c: &SessionClaim) -> Result<Found, String> {
        crate::team::check_session_id(&c.session)?;
        let h = Harness::parse(&c.harness)
            .ok_or_else(|| format!("seat {}: unknown harness {:?}", c.seat, c.harness))?;
        if let Some(other) = owner_of(&self.lock(), &c.session, &c.seat) {
            return Err(format!(
                "seat {}: session {} already belongs to seat {other} on this node; a session \
                 can be the session of one seat only",
                c.seat, c.session
            ));
        }
        let id = c.session.as_str();
        match h {
            Harness::Claude => {
                let projects = self.cfg.claude_config_dir.join("projects");
                let path = find_claude(&projects, id).ok_or_else(|| {
                    format!(
                        "seat {}: no Claude Code session {id} on this host: no transcript \
                         {}/*/{id}.jsonl (Claude Code keeps its sessions under the \
                         projects directory of the node's Claude config dir, \
                         $CLAUDE_CONFIG_DIR or ~/.claude)",
                        c.seat,
                        projects.display()
                    )
                })?;
                let cwd = jsonl_cwd(&path);
                let warning = cwd.as_deref().filter(|w| !same_dir(w, &c.cwd)).map(|w| {
                    format!(
                        "seat {}: Claude Code session {id} was started in {w}, not in the \
                         seat's working directory {}; Claude Code resumes a session from the \
                         directory it was started in, so the resume can fail (the seat then \
                         starts a new session); set the seat's cwd to {w} to resume it there",
                        c.seat, c.cwd
                    )
                });
                Ok(Found { cwd, warning })
            }
            Harness::Codex => {
                let home = crate::adapter::codex::home_in(&self.cfg.state_dir);
                let sessions = home.join("sessions");
                if let Some(path) = find_codex_rollout(&sessions, id) {
                    return Ok(Found {
                        cwd: jsonl_cwd(&path),
                        warning: None,
                    });
                }
                // The shared app-server may know the thread (for example
                // one it keeps elsewhere than the rollout files).
                let socket = home
                    .join("app-server-control")
                    .join("app-server-control.sock");
                if socket.exists()
                    && let Ok(r) = crate::codex_rpc::socket_calls(
                        &socket,
                        &[("thread/read", json!({"threadId": id}))],
                    )
                    .await
                    && let Some(t) = r.first().map(|v| &v["thread"]).filter(|t| t.is_object())
                {
                    return Ok(Found {
                        cwd: t["cwd"].as_str().map(str::to_string),
                        warning: None,
                    });
                }
                Err(format!(
                    "seat {}: no Codex session {id} on this host: no rollout file under {} \
                     starts with that id's session_meta. The node's Codex seats use their own \
                     CODEX_HOME ({}), not ~/.codex: to adopt a conversation from another Codex \
                     home, copy its rollout file (sessions/YYYY/MM/DD/rollout-...-{id}.jsonl) to \
                     the same place under {}",
                    c.seat,
                    sessions.display(),
                    home.display(),
                    sessions.display()
                ))
            }
            Harness::Dsh => {
                let home = crate::adapter::dsh::seat_home_in(&self.cfg.state_dir, &c.seat);
                match find_dsh(&home, id) {
                    Some(_) => Ok(Found::default()),
                    None => Err(format!(
                        "seat {}: no dsh session {id} for this seat: no session directory under \
                         {} (each dsh seat has its own dsh home; a session of another dsh home \
                         must be copied there first)",
                        c.seat,
                        home.join("sessions").display()
                    )),
                }
            }
        }
    }

    /// Check several claims, and that no session is claimed twice.
    pub(super) async fn check_claims(
        &self,
        claims: &[SessionClaim],
    ) -> Result<Vec<(SessionClaim, Found)>, Vec<String>> {
        let mut errors = Vec::new();
        let mut out = Vec::new();
        let mut seen: BTreeMap<&str, &str> = BTreeMap::new();
        for c in claims {
            if let Some(first) = seen.insert(&c.session, &c.seat) {
                errors.push(format!(
                    "seats {first} and {} both name session {}; a session can be the session \
                     of one seat only",
                    c.seat, c.session
                ));
                continue;
            }
            match self.check_claim(c).await {
                Ok(f) => out.push((c.clone(), f)),
                Err(e) => errors.push(e),
            }
        }
        if errors.is_empty() {
            Ok(out)
        } else {
            Err(errors)
        }
    }

    /// `CheckSessions`: claims of seats whose session is new to the node.
    pub(super) async fn sup_check_sessions(&self, claims: Vec<SessionClaim>) -> Value {
        // A seat that adopted the session already is not checked again; it
        // answers with the directory it runs in, so that `amail up` keeps
        // a seat without `cwd` there.
        let mut known = Vec::new();
        let claims: Vec<SessionClaim> = {
            let st = self.lock();
            claims
                .into_iter()
                .filter(|c| {
                    let m = st
                        .saved
                        .managed
                        .get(&c.seat)
                        .filter(|m| m.adopted.as_deref() == Some(c.session.as_str()));
                    if let Some(m) = m {
                        known.push(json!({
                            "seat": c.seat,
                            "session": c.session,
                            "cwd": m.cwd,
                            "warning": Value::Null,
                        }));
                    }
                    m.is_none()
                })
                .collect()
        };
        match self.check_claims(&claims).await {
            Ok(found) => {
                known.extend(found_json(&found));
                json!({"sessions": known})
            }
            Err(errors) => json!({"error": errors.join("\n")}),
        }
    }

    /// `amail seat adopt`.
    pub(super) async fn sup_adopt(&self, seat: &str, session: &str, restart: bool) -> Value {
        let Some(m) = self.lock().saved.managed.get(seat).cloned() else {
            return json!({"error": format!(
                "seat {seat} is not supervised by this node: amail up starts it (with \
                 session = \"{session}\" in the team file), or amail run {seat} --session \
                 {session} starts it by hand"
            )});
        };
        let claim = SessionClaim {
            seat: seat.to_string(),
            harness: m.harness.clone(),
            session: session.to_string(),
            cwd: m.cwd.clone(),
        };
        let found = match self.check_claim(&claim).await {
            Ok(f) => f,
            Err(e) => return json!({"error": e}),
        };
        let registers =
            Harness::parse(&m.harness).is_some_and(|h| self.adapter(h).registers_sessions());
        let (running, restarting) = {
            let mut st = self.lock();
            adopt_into(&mut st, seat, session, registers);
            let running = m.pid.is_some_and(|pid| alive(pid, m.pid_start));
            let restarting = running && restart;
            if restarting {
                self.begin_stop(&mut st, seat, supervise::Intent::Restart);
            }
            (running, restarting)
        };
        tracing::info!("seat {seat}: adopts session {session}");
        self.save_now();
        self.sup_kick.notify_one();
        json!({
            "seat": seat,
            "session": session,
            "cwd": found.cwd,
            "warning": found.warning,
            "running": running,
            "restarting": restarting,
        })
    }
}

pub(super) fn found_json(found: &[(SessionClaim, Found)]) -> Vec<Value> {
    found
        .iter()
        .map(|(c, f)| {
            json!({
                "seat": c.seat,
                "session": c.session,
                "cwd": f.cwd,
                "warning": f.warning,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_sessions_on_disk() {
        let d = tempfile::tempdir().unwrap();
        // Claude Code.
        let projects = d.path().join("projects");
        std::fs::create_dir_all(projects.join("-w-a")).unwrap();
        std::fs::write(
            projects.join("-w-a").join("s1.jsonl"),
            "{\"type\":\"summary\"}\n{\"type\":\"user\",\"cwd\":\"/w/a\",\"sessionId\":\"s1\"}\n",
        )
        .unwrap();
        let p = find_claude(&projects, "s1").unwrap();
        assert_eq!(jsonl_cwd(&p).as_deref(), Some("/w/a"));
        assert!(find_claude(&projects, "s2").is_none());
        // Codex: matched by the session_meta, not only by the name.
        let day = d.path().join("sessions").join("2026").join("10").join("26");
        std::fs::create_dir_all(&day).unwrap();
        std::fs::write(
            day.join("rollout-2026-10-26T10-00-00-t1.jsonl"),
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"t1\",\"cwd\":\"/w/b\"}}\n",
        )
        .unwrap();
        std::fs::write(
            day.join("rollout-2026-10-26T11-00-00-other.jsonl"),
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"t2\"}}\n",
        )
        .unwrap();
        std::fs::write(
            day.join("rollout-2026-10-26T12-00-00-t3.jsonl"),
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"t9\"}}\n",
        )
        .unwrap();
        let sessions = d.path().join("sessions");
        let p = find_codex_rollout(&sessions, "t1").unwrap();
        assert_eq!(jsonl_cwd(&p).as_deref(), Some("/w/b"));
        assert!(find_codex_rollout(&sessions, "t2").is_some());
        assert!(find_codex_rollout(&sessions, "t3").is_none());
        // dsh.
        let home = d.path().join("dsh");
        std::fs::create_dir_all(home.join("sessions").join("--w-c--").join("session-7")).unwrap();
        assert!(find_dsh(&home, "session-7").is_some());
        assert!(find_dsh(&home, "session-8").is_none());
    }
}
