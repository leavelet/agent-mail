//! Antigravity CLI seats without a model: fixture hook inputs, the real
//! `amail hook agy` and `amail inbox`, and a fake language server (the
//! RPCs the node calls). Every test runs on SQLite and (with
//! `AMAIL_TEST_PG_URL`) Postgres.

mod common;

use amail_node::adapter::agy::{ADDRESS_ENV, CONVERSATION_ENV, CSRF_ENV, PLUGIN_DIR};
use amail_store::testutil::Backend;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use common::*;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;

macro_rules! agy_tests {
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

agy_tests!(agy_hooks_wake_and_interrupt);

const CSRF: &str = "csrf-token-1";
const CONV: &str = "601130f6-5ef4-482a-bcfd-cd0a847d35b6";

#[derive(Default)]
struct Ls {
    /// `(method, body)` of every authorized call.
    calls: Mutex<Vec<(String, Value)>>,
    /// The conversation's run status `GetAllCascadeTrajectories` reports.
    status: Mutex<String>,
    /// What `ListBackgroundTasks` answers.
    tasks: Mutex<Value>,
}

/// A stand-in for agy's in-process language server: Connect RPC with JSON
/// bodies, the CSRF header required.
struct FakeLs {
    address: String,
    ls: Arc<Ls>,
    task: tokio::task::JoinHandle<()>,
}

impl FakeLs {
    async fn start() -> FakeLs {
        async fn rpc(
            State(ls): State<Arc<Ls>>,
            axum::extract::Path(method): axum::extract::Path<String>,
            headers: HeaderMap,
            body: String,
        ) -> Response {
            match headers
                .get("x-codeium-csrf-token")
                .and_then(|v| v.to_str().ok())
            {
                None => {
                    return (
                        StatusCode::UNAUTHORIZED,
                        r#"{"code":"unauthenticated","message":"unauthenticated: missing CSRF token"}"#,
                    )
                        .into_response();
                }
                Some(t) if t != CSRF => {
                    return (StatusCode::NOT_FOUND, "404 page not found").into_response();
                }
                Some(_) => {}
            }
            assert_eq!(
                headers.get("content-type").and_then(|v| v.to_str().ok()),
                Some("application/json")
            );
            let v: Value = serde_json::from_str(&body).unwrap();
            ls.calls.lock().unwrap().push((method.clone(), v));
            let out = match method.as_str() {
                "GetAllCascadeTrajectories" => json!({"trajectorySummaries": {
                    CONV: {"status": ls.status.lock().unwrap().clone(), "stepCount": 12},
                    "other-conversation": {"status": "CASCADE_RUN_STATUS_RUNNING"},
                }}),
                "ListBackgroundTasks" => ls.tasks.lock().unwrap().clone(),
                _ => json!({}),
            };
            axum::Json(out).into_response()
        }
        let ls = Arc::new(Ls::default());
        *ls.status.lock().unwrap() = "CASCADE_RUN_STATUS_IDLE".into();
        *ls.tasks.lock().unwrap() = json!({});
        let app = axum::Router::new()
            .route(
                "/exa.language_server_pb.LanguageServerService/{method}",
                axum::routing::post(rpc),
            )
            .with_state(ls.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        FakeLs { address, ls, task }
    }

    fn calls(&self, method: &str) -> Vec<Value> {
        self.ls
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, _)| m == method)
            .map(|(_, b)| b.clone())
            .collect()
    }

    fn set_status(&self, s: &str) {
        *self.ls.status.lock().unwrap() = s.to_string();
    }

    /// Background tasks of these conversations.
    fn set_tasks(&self, conversations: &[&str]) {
        let tasks: Vec<Value> = conversations
            .iter()
            .map(|c| {
                json!({"conversationId": c, "task": {"taskId": format!("{c}/task-39"),
                    "toolName": "run_command", "description": "sleep 600", "stepIndex": 39}})
            })
            .collect();
        *self.ls.tasks.lock().unwrap() = if tasks.is_empty() {
            json!({})
        } else {
            json!({ "tasks": tasks })
        };
    }
}

impl Drop for FakeLs {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn agy(name: &str, fields: &[(&str, Value)]) -> Value {
    with(fixture("agy", name), fields)
}

/// Run the real `amail <args>` as a command of the agent's shell would:
/// with the seat's credentials and the language server's variables.
async fn agent_command(h: &Harness, args: &[&str], env: Vec<(String, String)>) -> (bool, String) {
    let sock = h.sock.clone();
    let home = h.dir.path().join("agent-home");
    std::fs::create_dir_all(&home).unwrap();
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    tokio::task::spawn_blocking(move || {
        let mut c = std::process::Command::new(BIN);
        c.args(&args)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("AMAIL_NODE_SOCK", &sock)
            .envs(env)
            .stdin(std::process::Stdio::null());
        let out = c.output().unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned()
                + &String::from_utf8_lossy(&out.stderr),
        )
    })
    .await
    .unwrap()
}

/// Launch plan and plugin, hooks and injection, learning the language
/// server's address from an `amail` command, wake-up, prompt and
/// interrupt through the language server.
async fn agy_hooks_wake_and_interrupt(b: Backend) {
    // The restricted local policy: hooks and the session's variables are
    // taken only from below the process launched for the seat.
    let opts = Opts {
        check_callers: Some(true),
        local_policy: Some(amail_proto::LocalPolicy::Restricted),
        ..Opts::default()
    };
    let Some(h) = Harness::start(b, "agy_hooks_wake_and_interrupt", opts).await else {
        return;
    };
    let seat_api = h.seat("ag", "agy").await;
    let cwd = h.dir.path().join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    // The test process stands for the launched `agy`: the hooks and
    // commands it starts are the session's.
    let plan = h
        .node
        .as_ref()
        .unwrap()
        .node
        .handle(amail_node::wire::Request::Launch {
            seat: "ag".into(),
            cwd: cwd.to_string_lossy().into(),
            pid: std::process::id(),
            harness: None,
            extra: vec![],
            pane: None,
            mux_session: None,
            permission_mode: Some(amail_proto::PermissionMode::Unattended),
            git_access: None,
            dsh_jail: None,
            session: None,
        })
        .await;
    assert!(plan.get("error").is_none(), "{plan}");
    assert_eq!(plan["program"], "agy");
    let args: Vec<String> = serde_json::from_value(plan["args"].clone()).unwrap();
    assert_eq!(&args[..2], ["--model", "test-model"]);
    assert!(args.iter().any(|a| a == "--dangerously-skip-permissions"));
    assert_eq!(args[args.len() - 2], "-i");
    assert!(args.last().unwrap().contains("You are ag."), "{args:?}");
    assert_eq!(plan["env"]["AMAIL_SEAT"], "ag");
    let hooks = std::fs::read_to_string(cwd.join(PLUGIN_DIR).join("hooks.json")).unwrap();
    assert!(hooks.contains(&format!("{BIN} hook agy")), "{hooks}");
    assert!(cwd.join(PLUGIN_DIR).join("rules/AGENTS.md").is_file());

    let env = vec![("AMAIL_SEAT".to_string(), "ag".to_string())];
    // A new conversation starts: idle; the answer shows nothing.
    assert_eq!(
        h.hook("agy", env.clone(), agy("SessionStart", &[])).await,
        Value::Null
    );
    assert_eq!(h.state("ag").await, "idle");
    assert_eq!(h.seat_status("ag").await["harness"], "agy");
    // The hooks name no pid: the session's process is the launched one.
    assert_eq!(
        h.seat_status("ag").await["session"]["pid"],
        std::process::id()
    );

    // The agent runs `amail inbox` (its first prompt says so): the command
    // works and tells the node where the language server is.
    let ls = FakeLs::start().await;
    let ls_env = |seat: &str| {
        vec![
            ("AMAIL_SEAT".to_string(), seat.to_string()),
            ("AMAIL_URL".to_string(), h.api.base.clone()),
            ("AMAIL_TOKEN".to_string(), seat_api.token.clone()),
            (ADDRESS_ENV.to_string(), ls.address.clone()),
            (CSRF_ENV.to_string(), CSRF.to_string()),
            (CONVERSATION_ENV.to_string(), CONV.to_string()),
        ]
    };
    // Naming another seat: ignored (the command still works).
    let (ok, out) = agent_command(&h, &["inbox"], ls_env("nobody")).await;
    assert!(ok, "{out}");
    let (ok, out) = agent_command(&h, &["inbox"], ls_env("ag")).await;
    assert!(ok, "{out}");
    // Another harness's variables from this session: refused.
    let refused = socket_request(
        &h.sock,
        amail_node::wire::Request::SessionEnv {
            harness: "codex".into(),
            env: ls_env("ag").into_iter().collect(),
        },
    )
    .await;
    assert!(refused.get("error").is_some(), "{refused}");

    // Mail to the idle seat: a message to the conversation through the
    // language server.
    let m1 = h.api.send(&["ag"], &[], "first", false).await;
    h.wait("wake-up message", || async {
        !ls.calls("SendUserCascadeMessage").is_empty()
    })
    .await;
    let w = &ls.calls("SendUserCascadeMessage")[0];
    assert_eq!(w["cascadeId"], CONV);
    assert_eq!(w["deliveryStrategy"], "MESSAGE_DELIVERY_STRATEGY_WHEN_IDLE");
    let wake = w["items"][0]["text"].as_str().unwrap().to_string();
    assert!(wake.starts_with("[amail] You have new mail"), "{wake}");

    // The turn starts (the first model call): the mail is injected as an
    // ephemeral message.
    let first = agy(
        "PreInvocation",
        &[(
            "lastUserInput",
            json!(format!(
                "<USER_REQUEST>\n{wake}\n</USER_REQUEST>\n<ADDITIONAL_METADATA>\nt\n</ADDITIONAL_METADATA>"
            )),
        )],
    );
    let out = h.hook("agy", env.clone(), first).await;
    let text = out["injectSteps"][0]["ephemeralMessage"]
        .as_str()
        .unwrap_or_else(|| panic!("{out}"));
    assert_eq!(injected_ids(text), vec![m1]);
    assert_eq!(h.state("ag").await, "busy");

    // Busy: `PostToolUse` cannot inject; it records the tool call and
    // leaves the mail for the next model call.
    let m2 = h.api.send(&["ag"], &[], "second", false).await;
    h.wait_pending("ag", 1, 0, 0).await;
    assert_eq!(
        h.hook("agy", env.clone(), agy("PostToolUse", &[])).await,
        Value::Null
    );
    let tools = node_worklog(&h, "ag", "tool").await;
    assert!(
        tools[0]["text"]
            .as_str()
            .unwrap()
            .starts_with("run_command: cd /home/u/work/w1 && amail task done t3"),
        "{tools:?}"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    h.wait_pending("ag", 1, 0, 0).await;
    let out = h
        .hook(
            "agy",
            env.clone(),
            agy("PreInvocation", &[("invocationNum", json!(1))]),
        )
        .await;
    assert_eq!(
        injected_ids(out["injectSteps"][0]["ephemeralMessage"].as_str().unwrap()),
        vec![m2]
    );
    // A model call without new mail: nothing to say.
    assert_eq!(
        h.hook(
            "agy",
            env.clone(),
            agy("PreInvocation", &[("invocationNum", json!(2))])
        )
        .await,
        Value::Null
    );
    assert_eq!(node_worklog(&h, "ag", "tool").await.len(), 1);

    // Stop with pending mail: the turn goes on with it; then idle.
    let m3 = h.api.send(&["ag"], &[], "third", false).await;
    h.wait_pending("ag", 1, 0, 0).await;
    let out = h.hook("agy", env.clone(), agy("Stop", &[])).await;
    assert_eq!(out["decision"], "continue");
    assert_eq!(injected_ids(out["reason"].as_str().unwrap()), vec![m3]);
    assert_eq!(
        h.hook("agy", env.clone(), agy("Stop", &[])).await,
        Value::Null
    );
    assert_eq!(h.state("ag").await, "idle");
    assert_eq!(ls.calls("SendUserCascadeMessage").len(), 1);

    // A command moved to the background: `Stop` with `fullyIdle: false`
    // ends the turn too (agy fires no hook if the command fails or is
    // killed). When the command completes, agy runs a short turn of its
    // own, whose `lastUserInput` is the earlier prompt.
    let prompt = agy(
        "PreInvocation",
        &[("lastUserInput", json!("run the tests"))],
    );
    assert_eq!(
        h.hook("agy", env.clone(), prompt.clone()).await,
        Value::Null
    );
    assert_eq!(h.state("ag").await, "busy");
    assert_eq!(
        h.hook(
            "agy",
            env.clone(),
            agy("Stop", &[("fullyIdle", json!(false))])
        )
        .await,
        Value::Null
    );
    assert_eq!(h.state("ag").await, "idle");
    h.hook("agy", env.clone(), agy("PostToolUse", &[])).await;
    h.hook("agy", env.clone(), prompt.clone()).await;
    assert_eq!(h.state("ag").await, "busy");
    h.hook("agy", env.clone(), agy("Stop", &[])).await;
    assert_eq!(h.state("ag").await, "idle");

    // A wake-up whose mail is gone ends at once; the same wake text again
    // (agy repeats the last prompt for such a completion turn) is not a
    // wake-up: nothing is injected, and the turn is logged as a plain one.
    let stale = agy(
        "PreInvocation",
        &[(
            "lastUserInput",
            json!("[amail] You have new mail (wake #900)."),
        )],
    );
    let out = h.hook("agy", env.clone(), stale.clone()).await;
    assert!(
        out["injectSteps"][0]["ephemeralMessage"]
            .as_str()
            .is_some_and(|t| t.starts_with("[amail] Nothing is waiting")),
        "{out}"
    );
    h.hook("agy", env.clone(), agy("Stop", &[])).await;
    assert_eq!(h.hook("agy", env.clone(), stale).await, Value::Null);
    h.hook("agy", env.clone(), agy("Stop", &[])).await;
    // Turn starts so far: the first wake-up and #900; "run the tests", the
    // completion turn and the repeated #900.
    let starts = |text: &'static str| {
        let h = &h;
        async move {
            h.api.get("/seats/ag/worklog?limit=200").await["entries"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|e| e["kind"] == "turn_start" && e["text"] == text)
                .count()
        }
    };
    h.wait("turn starts in the worklog", || async {
        starts("turn started by a wake-up").await == 2 && starts("turn started").await == 3
    })
    .await;

    // A hook in another seat's name is refused.
    let other = vec![("AMAIL_SEAT".to_string(), "nobody".to_string())];
    assert_eq!(
        h.hook("agy", other, agy("PreInvocation", &[])).await,
        Value::Null
    );

    // The user's prompt: a message to the conversation too.
    h.api
        .post("/seats/ag/prompt", json!({"text": "look at the build"}))
        .await;
    h.wait("prompt message", || async {
        ls.calls("SendUserCascadeMessage").len() == 2
    })
    .await;
    let p = &ls.calls("SendUserCascadeMessage")[1];
    assert_eq!(
        p["items"][0]["text"],
        "[amail] Prompt from the user: look at the build"
    );

    // Interrupt while the conversation runs: cancelled, and the turn ends.
    h.hook("agy", env.clone(), prompt.clone()).await;
    assert_eq!(h.state("ag").await, "busy");
    ls.set_status("CASCADE_RUN_STATUS_RUNNING");
    h.api.post("/seats/ag/interrupt", json!({})).await;
    h.wait("cancel", || async {
        !ls.calls("CancelCascadeInvocation").is_empty()
    })
    .await;
    assert_eq!(ls.calls("CancelCascadeInvocation")[0]["cascadeId"], CONV);
    assert_eq!(
        ls.calls("CancelCascadeInvocation")[0]["killBackgroundTasks"],
        true
    );
    assert_eq!(
        ls.calls("GetAllCascadeTrajectories")[0]["excludeSubtrajectories"],
        true
    );
    h.wait("idle after the interrupt", || async {
        h.state("ag").await == "idle"
    })
    .await;

    // Interrupt while it is idle, with a background task of another
    // conversation only: asked, not cancelled.
    ls.set_status("CASCADE_RUN_STATUS_IDLE");
    ls.set_tasks(&["other-conversation"]);
    h.api.post("/seats/ag/interrupt", json!({})).await;
    h.wait("status asked", || async {
        ls.calls("GetAllCascadeTrajectories").len() == 2
    })
    .await;
    let logged = || async {
        h.api.get("/seats/ag/worklog?limit=200").await["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| {
                e["kind"] == "interrupt"
                    && e["text"]
                        .as_str()
                        .is_some_and(|t| t.contains("no turn was running"))
            })
    };
    h.wait("not interrupted", logged).await;
    assert_eq!(ls.calls("CancelCascadeInvocation").len(), 1);
    assert_eq!(ls.calls("ListBackgroundTasks").len(), 1);

    // Idle status, but a command of the conversation runs in the
    // background: cancelled, with its background tasks.
    ls.set_tasks(&["other-conversation", CONV]);
    h.api.post("/seats/ag/interrupt", json!({})).await;
    h.wait("background cancel", || async {
        ls.calls("CancelCascadeInvocation").len() == 2
    })
    .await;
    let c = &ls.calls("CancelCascadeInvocation")[1];
    assert_eq!(c["cascadeId"], CONV);
    assert_eq!(c["killBackgroundTasks"], true);

    // A wrong token: the language server's answer is the error.
    let a = amail_node::adapter::agy::Agy::new(amail_node::adapter::AdapterEnv {
        state_dir: h.dir.path().join("state"),
        amail_exe: BIN.into(),
        node_socket: h.sock.clone(),
        server_url: h.api.base.clone(),
        claude_bin: "claude".into(),
        codex_bin: "codex".into(),
        codex_sandbox: None,
        dsh_bin: "dsh".into(),
        dsh_env: Default::default(),
        agy_bin: "agy".into(),
        codex_auth: h.dir.path().join("auth.json"),
        codex_pass: String::new(),
    });
    let bad = amail_node::adapter::agy::Target {
        address: ls.address.clone(),
        token: "old-token".into(),
        conversation: CONV.into(),
    };
    use amail_node::adapter::Adapter;
    let e = a.wake(&bad.to_target(), "x").await.unwrap_err();
    assert!(format!("{e:#}").contains("HTTP 404"), "{e:#}");
    assert!(a.wake("not a target", "x").await.is_err());
}
