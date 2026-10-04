//! DeepSeek Harness seats without a model: fixture hook inputs, a fake
//! `dsh web` host (its HTTP API) and the real `amail dsh-host` wrapper.
//! Every test runs on SQLite and (with `AMAIL_TEST_PG_URL`) Postgres.

mod common;

use amail_node::adapter::dsh::{Endpoint, transcript_path};
use amail_store::testutil::Backend;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use common::*;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

macro_rules! dsh_tests {
    ($($name:ident),* $(,)?) => {
        mod sqlite {
            $(
                #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
                async fn $name() {
                    super::$name(amail_store::testutil::Backend::Sqlite).await;
                }
            )*
        }
        mod postgres {
            $(
                #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
                async fn $name() {
                    super::$name(amail_store::testutil::Backend::Postgres).await;
                }
            )*
        }
    };
}

dsh_tests!(dsh_hooks_and_wake, dsh_host_wrapper);

const TOKEN: &str = "launch-token";
const COOKIE: &str = "dsh_session=signed";

/// API calls a fake host received: `(method, args)`.
type Calls = Arc<Mutex<Vec<(String, Value)>>>;

/// A stand-in for a `dsh web` host: the token exchange, `session/create`
/// and `session/prompt`.
struct FakeHost {
    url: String,
    calls: Calls,
    task: tokio::task::JoinHandle<()>,
}

impl FakeHost {
    async fn start() -> FakeHost {
        async fn index(Query(q): Query<HashMap<String, String>>) -> Response {
            if q.get("token").map(String::as_str) != Some(TOKEN) {
                return StatusCode::UNAUTHORIZED.into_response();
            }
            (
                StatusCode::SEE_OTHER,
                [
                    (header::LOCATION, "./".to_string()),
                    (header::SET_COOKIE, format!("{COOKIE}; Path=/; HttpOnly")),
                ],
            )
                .into_response()
        }
        async fn api(
            State(calls): State<Calls>,
            axum::extract::Path(endpoint): axum::extract::Path<String>,
            headers: HeaderMap,
            body: String,
        ) -> Response {
            let cookie = headers
                .get(header::COOKIE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            if !cookie.split("; ").any(|c| c == COOKIE) {
                return StatusCode::UNAUTHORIZED.into_response();
            }
            let v: Value = serde_json::from_str(&body).unwrap();
            assert_eq!(v["type"], "client-request");
            assert_eq!(v["method"], endpoint.as_str());
            let args = v["payload"]["args"].clone();
            calls.lock().unwrap().push((endpoint.clone(), args.clone()));
            let value = match endpoint.as_str() {
                "session/create" => json!({
                    "sessionId": args["request"]["sessionId"].as_str().unwrap_or("session-new"),
                    "agentPreset": "standard",
                }),
                "session/prompt" => json!({"accepted": true}),
                _ => json!({}),
            };
            axum::Json(json!({
                "type": "server-response", "rpcId": v["rpcId"],
                "result": {"ok": true, "value": value},
            }))
            .into_response()
        }
        let calls: Calls = Arc::default();
        let app = axum::Router::new()
            .route("/", axum::routing::get(index))
            .route("/api/{*endpoint}", axum::routing::post(api))
            .with_state(calls.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        FakeHost { url, calls, task }
    }

    fn calls(&self, method: &str) -> Vec<Value> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, _)| m == method)
            .map(|(_, a)| a["request"].clone())
            .collect()
    }
}

impl Drop for FakeHost {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn dsh(name: &str, sid: &str) -> Value {
    with(fixture("dsh", name), &[("session_id", json!(sid))])
}

async fn launch(h: &Harness, seat: &str, cwd: &Path) -> Value {
    let plan = h
        .node
        .as_ref()
        .unwrap()
        .node
        .handle(amail_node::wire::Request::Launch {
            seat: seat.into(),
            cwd: cwd.to_string_lossy().into(),
            pid: std::process::id(),
            harness: None,
            extra: vec![],
            pane: None,
            mux_session: None,
        })
        .await;
    assert!(plan.get("error").is_none(), "{plan}");
    plan
}

/// Launch plan, hook state machine, injection, wake-up through the host API,
/// the wrapper's `Interrupt`, and hooks of other sessions.
async fn dsh_hooks_and_wake(b: Backend) {
    let Some(h) = Harness::start(b, "dsh_hooks_and_wake", Opts::default()).await else {
        return;
    };
    h.seat("ds", "dsh").await;
    let cwd = h.dir.path().join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let plan = launch(&h, "ds", &cwd).await;
    assert_eq!(plan["program"], BIN);
    let args: Vec<String> = serde_json::from_value(plan["args"].clone()).unwrap();
    assert_eq!(args[0], "dsh-host");
    let sep = args.iter().position(|a| a == "--").unwrap();
    assert_eq!(&args[sep + 1..sep + 4], ["dsh", "web", "--no-open"]);
    assert!(args.ends_with(&["--port".into(), "0".into()]), "{args:?}");
    let prompt = &args[args.iter().position(|a| a == "--prompt").unwrap() + 1];
    assert!(prompt.starts_with("[amail]"), "{prompt}");
    let env = &plan["env"];
    assert_eq!(env["AMAIL_SEAT"], "ds");
    assert_eq!(env["DSH_PERMISSION_MODE"], "danger-full-access");
    assert!(
        env["DSH_HOME"]
            .as_str()
            .unwrap()
            .ends_with("state/dsh-home")
    );
    let endpoint = env["AMAIL_DSH_ENDPOINT"].as_str().unwrap().to_string();
    let patch_path = &args[args.iter().position(|a| a == "--patch").unwrap() + 1];
    let patch = std::fs::read_to_string(patch_path).unwrap();
    assert!(
        patch.contains("You are ds.") && patch.contains("test-model"),
        "{patch}"
    );
    let home_patch =
        std::fs::read_to_string(h.dir.path().join("state/dsh-home/cordis.patch.yml")).unwrap();
    assert!(home_patch.contains("dsh-hooks-codex"), "{home_patch}");

    // The wrapper's endpoint file, for a host that is up.
    let host = FakeHost::start().await;
    let sid = "session-00000000-0000-4000-8000-00000000d5d5";
    Endpoint {
        pid: std::process::id(),
        url: host.url.clone(),
        token: TOKEN.into(),
        session: Some(sid.into()),
        cwd: cwd.to_string_lossy().into(),
    }
    .write(Path::new(&endpoint))
    .unwrap();
    let env: Vec<(String, String)> = vec![
        ("AMAIL_SEAT".into(), "ds".into()),
        ("AMAIL_DSH_ENDPOINT".into(), endpoint.clone()),
    ];

    // SessionStart (created, no prompt yet): idle.
    assert_eq!(
        h.hook("dsh", env.clone(), dsh("SessionStart", sid)).await,
        Value::Null
    );
    assert_eq!(h.state("ds").await, "idle");
    assert_eq!(h.seat_status("ds").await["harness"], "dsh");

    // Mail to an idle seat: queued as a prompt through the host API.
    let m1 = h.api.send(&["ds"], &[], "first", false).await;
    h.wait("wake-up prompt", || async {
        !host.calls("session/prompt").is_empty()
    })
    .await;
    let p = &host.calls("session/prompt")[0];
    assert_eq!(p["sessionId"], sid);
    assert_eq!(p["mode"], "queue");
    let text = p["content"][0]["text"].as_str().unwrap();
    assert!(text.starts_with("[amail] You have new mail"), "{text}");

    // The turn starts: UserPromptSubmit shows the mail.
    let out = h
        .hook("dsh", env.clone(), dsh("UserPromptSubmit", sid))
        .await;
    assert_eq!(
        out["hookSpecificOutput"]["hookEventName"],
        "UserPromptSubmit"
    );
    assert_eq!(injected_ids(&hook_text(&out).unwrap()), vec![m1]);
    assert_eq!(h.state("ds").await, "busy");

    // Busy: PostToolUse injects new mail; no wake-up.
    let m2 = h.api.send(&["ds"], &[], "second", false).await;
    h.wait_pending("ds", 1, 0, 0).await;
    let out = h.hook("dsh", env.clone(), dsh("PostToolUse", sid)).await;
    assert_eq!(injected_ids(&hook_text(&out).unwrap()), vec![m2]);

    // Stop with pending mail: blocked with the mail; then idle.
    let m3 = h.api.send(&["ds"], &[], "third", false).await;
    h.wait_pending("ds", 1, 0, 0).await;
    let out = h.hook("dsh", env.clone(), dsh("Stop", sid)).await;
    assert_eq!(out["decision"], "block");
    assert_eq!(injected_ids(out["reason"].as_str().unwrap()), vec![m3]);
    assert_eq!(
        h.hook("dsh", env.clone(), dsh("Stop", sid)).await,
        Value::Null
    );
    assert_eq!(h.state("ds").await, "idle");
    assert_eq!(host.calls("session/prompt").len(), 1);

    // A session the person opened in the same host is not the seat's.
    let other = "session-00000000-0000-4000-8000-0000000000ff";
    assert_eq!(
        h.hook("dsh", env.clone(), dsh("UserPromptSubmit", other))
            .await,
        Value::Null
    );
    assert_eq!(h.state("ds").await, "idle");

    // A turn that ended without Stop (reported by the wrapper): idle.
    h.hook("dsh", env.clone(), dsh("UserPromptSubmit", sid))
        .await;
    assert_eq!(h.state("ds").await, "busy");
    let interrupt = json!({"hook_event_name": "Interrupt", "session_id": sid,
                           "cwd": cwd.to_string_lossy()});
    assert_eq!(h.hook("dsh", env.clone(), interrupt).await, Value::Null);
    assert_eq!(h.state("ds").await, "idle");
}

/// `amail dsh-host` with a fake `dsh`: publishes the endpoint, creates the
/// session and queues the first prompt, reports a cancelled turn, cleans up;
/// with `--resume` it loads the session and sends nothing.
async fn dsh_host_wrapper(b: Backend) {
    let Some(h) = Harness::start(b, "dsh_host_wrapper", Opts::default()).await else {
        return;
    };
    h.seat("dw", "dsh").await;
    let host = FakeHost::start().await;
    let work = h.dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let work = work.canonicalize().unwrap();
    let home = h.dir.path().join("dsh-home");
    let fake = h.dir.path().join("fake-dsh");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\necho \"fake dsh $*\"\necho \"dsh web: {}/?token={TOKEN}\"\nexec sleep 30\n",
            host.url
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let endpoint = h.dir.path().join("dw.json");
    let spawn = |extra: &[&str]| {
        let mut c = tokio::process::Command::new(BIN);
        c.args(["dsh-host", "--seat", "dw", "--endpoint"])
            .arg(&endpoint)
            .args(extra)
            .args(["--", fake.to_str().unwrap(), "web"])
            .current_dir(&work)
            .env("AMAIL_NODE_SOCK", &h.sock)
            .env("DSH_HOME", &home)
            .env("CLAUDE_CODE_SESSION_ID", "leak")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        c.spawn().unwrap()
    };

    let mut child = spawn(&["--prompt", "hello"]);
    h.wait("endpoint with session", || async {
        Endpoint::read(&endpoint).is_some_and(|e| e.session.as_deref() == Some("session-new"))
    })
    .await;
    let ep = Endpoint::read(&endpoint).unwrap();
    assert_eq!(ep.pid, child.id().unwrap());
    assert_eq!(ep.url, host.url);
    assert_eq!(ep.token, TOKEN);
    h.wait("first prompt", || async {
        !host.calls("session/prompt").is_empty()
    })
    .await;
    let created = host.calls("session/create");
    assert_eq!(created[0]["cwd"], work.to_string_lossy().as_ref());
    assert!(created[0].get("sessionId").is_none());
    assert_eq!(
        host.calls("session/prompt")[0]["content"][0]["text"],
        "hello"
    );

    // The seat's session, busy; the person cancels the turn in the GUI.
    let env: Vec<(String, String)> = vec![
        ("AMAIL_SEAT".into(), "dw".into()),
        (
            "AMAIL_DSH_ENDPOINT".into(),
            endpoint.to_string_lossy().into(),
        ),
    ];
    h.hook("dsh", env.clone(), dsh("SessionStart", "session-new"))
        .await;
    h.hook("dsh", env.clone(), dsh("UserPromptSubmit", "session-new"))
        .await;
    assert_eq!(h.state("dw").await, "busy");
    // The hook's pid is the wrapper's.
    assert_eq!(
        h.seat_status("dw").await["session"]["pid"],
        child.id().unwrap()
    );
    let log = transcript_path(&home, &work.to_string_lossy(), "session-new");
    std::fs::create_dir_all(log.parent().unwrap()).unwrap();
    std::fs::write(
        &log,
        "{\"type\":\"turn/start\",\"seq\":1,\"time\":1,\"data\":{\"turn\":1}}\n\
         {\"type\":\"turn/end\",\"seq\":2,\"time\":2,\"data\":{\"turn\":1,\"reason\":{\"kind\":\"aborted\"}}}\n",
    )
    .unwrap();
    h.wait("idle after the cancelled turn", || async {
        h.state("dw").await == "idle"
    })
    .await;

    // Stopping the wrapper stops the host and removes the endpoint.
    // SAFETY: signalling our child.
    unsafe { libc::kill(child.id().unwrap() as i32, libc::SIGTERM) };
    let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(!status.success());
    assert!(!endpoint.exists());

    // Resume: the session is loaded by id; no prompt is sent.
    let mut child = spawn(&["--resume", "session-old"]);
    h.wait("resumed endpoint", || async {
        Endpoint::read(&endpoint).is_some_and(|e| e.session.as_deref() == Some("session-old"))
    })
    .await;
    assert_eq!(host.calls("session/create")[1]["sessionId"], "session-old");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(host.calls("session/prompt").len(), 1);
    child.kill().await.unwrap();
}
