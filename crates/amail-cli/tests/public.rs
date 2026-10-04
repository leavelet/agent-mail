//! Public mode from the command line: a real `amail serve --public`
//! process, the operator's `amail admin` commands on its database, and a
//! user who registers with an invite code, logs in, works with a seat,
//! rotates the token and deletes the org. Also `amail init`, `amail
//! doctor` and `amail node start/stop`, and that the server's log holds no
//! tokens, invite codes or mail bodies. SQLite and (with
//! `AMAIL_TEST_PG_URL`) Postgres.

use amail_store::testutil::{Backend, fresh_url};
use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::Duration;

const BIN: &str = env!("CARGO_BIN_EXE_amail");

/// Variables that would point the commands at the caller's own setup.
const CLEAR: &[&str] = &[
    "AMAIL_URL",
    "AMAIL_TOKEN",
    "AMAIL_PROFILE",
    "AMAIL_DB",
    "AMAIL_NODE_TOKEN",
    "AMAIL_NODE_SOCK",
    "AMAIL_STATE_DIR",
    "AMAIL_CONFIG_DIR",
    "AMAIL_SEAT",
    "CODEX_THREAD_ID",
    "CODEX_HOME",
    "CLAUDE_CONFIG_DIR",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
];

struct Proc(Child);

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A sandbox: a home directory and config directory of its own.
struct Env {
    dir: tempfile::TempDir,
    extra: Vec<(String, String)>,
}

impl Env {
    fn new() -> Env {
        Env {
            dir: tempfile::tempdir().unwrap(),
            extra: Vec::new(),
        }
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    fn config_dir(&self) -> PathBuf {
        self.dir.path().join("config")
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args);
        for k in CLEAR {
            c.env_remove(k);
        }
        std::fs::create_dir_all(self.home()).unwrap();
        c.env("HOME", self.home())
            .env("AMAIL_CONFIG_DIR", self.config_dir())
            .env("AMAIL_NODE_SOCK", self.dir.path().join("node.sock"))
            .env("AMAIL_STATE_DIR", self.dir.path().join("state"))
            .current_dir(self.dir.path());
        for (k, v) in &self.extra {
            c.env(k, v);
        }
        c
    }

    fn run_with(&self, args: &[&str], env: &[(&str, &str)], stdin: Option<&str>) -> Output {
        let mut c = self.cmd(args);
        for (k, v) in env {
            c.env(k, v);
        }
        c.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = c.spawn().unwrap();
        let mut input = child.stdin.take().unwrap();
        if let Some(s) = stdin {
            input.write_all(s.as_bytes()).unwrap();
        }
        drop(input);
        child.wait_with_output().unwrap()
    }

    fn ok(&self, args: &[&str], env: &[(&str, &str)], stdin: Option<&str>) -> String {
        let o = self.run_with(args, env, stdin);
        assert!(
            o.status.success(),
            "amail {} failed: {}{}",
            args.join(" "),
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8(o.stdout).unwrap()
    }

    fn fails(&self, args: &[&str], env: &[(&str, &str)]) -> String {
        let o = self.run_with(args, env, None);
        assert!(!o.status.success(), "amail {} should fail", args.join(" "));
        String::from_utf8_lossy(&o.stderr).into_owned()
    }

    fn json(&self, args: &[&str], env: &[(&str, &str)]) -> Value {
        let mut a = vec!["--json"];
        a.extend_from_slice(args);
        serde_json::from_str(&self.ok(&a, env, None)).unwrap()
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn wait_healthy(url: &str) {
    for _ in 0..200 {
        if Command::new(BIN)
            .args(["healthcheck", url])
            .output()
            .is_ok_and(|o| o.status.success())
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("server at {url} did not come up");
}

fn serve(env: &Env, db: &str, extra: &[&str], log: &Path) -> (Proc, String) {
    let port = free_port();
    let url = format!("http://127.0.0.1:{port}");
    let log = std::fs::File::create(log).unwrap();
    let mut args = vec!["serve", "--db", db];
    let listen = format!("127.0.0.1:{port}");
    args.extend(["--listen", &listen]);
    args.extend_from_slice(extra);
    let child = env
        .cmd(&args)
        .env("RUST_LOG", "debug")
        .stdout(log.try_clone().unwrap())
        .stderr(log)
        .spawn()
        .unwrap();
    let p = Proc(child);
    wait_healthy(&url);
    (p, url)
}

#[test]
fn onboarding_sqlite() {
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(onboarding(Backend::Sqlite));
}

#[test]
fn onboarding_postgres() {
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(onboarding(Backend::Postgres));
}

/// From an invite code to the first mail, on the command line; then the
/// operator's commands; then deleting the org.
async fn onboarding(backend: Backend) {
    let Some((db, _guard)) = fresh_url(backend, "cli_onboarding").await else {
        return;
    };
    let env = Env::new();
    let log = env.dir.path().join("serve.log");
    let (_server, url) = serve(&env, &db, &["--public"], &log);

    // The operator makes invite codes (stored hashed) and an operator token.
    let inv = env.json(
        &[
            "admin",
            "invite",
            "create",
            "--count",
            "2",
            "--note",
            "first users",
            "--expires",
            "7d",
            "--db",
            &db,
        ],
        &[],
    );
    let codes: Vec<String> = inv["invites"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["code"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(codes.len(), 2);
    assert!(codes.iter().all(|c| c.starts_with("inv_")));
    let ls = env.ok(&["admin", "invite", "ls", "--db", &db], &[], None);
    assert_eq!(ls.matches(" open ").count(), 2, "{ls}");
    let op = env.json(
        &[
            "admin",
            "operator-token",
            "create",
            "--label",
            "me",
            "--db",
            &db,
        ],
        &[],
    );
    let op_token = op["token"].as_str().unwrap().to_string();

    // A user registers: the token is printed and saved as a login.
    let reg = env.json(
        &["register", &url, "--invite", &codes[0], "--org", "acme"],
        &[],
    );
    let user_token = reg["token"].as_str().unwrap().to_string();
    assert_eq!(reg["org"], "acme");
    let cfg_file = env.config_dir().join("config.toml");
    let mode = std::fs::metadata(&cfg_file).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(mode.mode() & 0o777, 0o600);
    // The same code does not work twice.
    let err = env.fails(
        &["register", &url, "--invite", &codes[0], "--org", "acme2"],
        &[],
    );
    assert!(err.contains("invalid_invite"), "{err}");

    // Later commands use the saved login: seats, a seat token, the first
    // mail.
    let org = env.json(&["org", "show"], &[]);
    assert_eq!(org["name"], "acme");
    env.ok(&["seat", "add", "w1", "--role", "first worker"], &[], None);
    let w1 = env
        .ok(&["seat", "token", "w1"], &[], None)
        .trim()
        .to_string();
    env.ok(
        &[
            "send",
            "--to",
            "w1",
            "-s",
            "Kickoff",
            "-m",
            "the body of the first mail",
        ],
        &[],
        None,
    );
    // An agent's environment wins over the saved login.
    let as_w1 = [("AMAIL_URL", url.as_str()), ("AMAIL_TOKEN", w1.as_str())];
    let inbox = env.ok(&["inbox"], &as_w1, None);
    assert!(inbox.contains("owner (USER): Kickoff"), "{inbox}");
    let id = inbox
        .trim_start_matches('#')
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();
    let read = env.ok(&["read", &id], &as_w1, None);
    assert!(read.contains("the body of the first mail"), "{read}");
    env.ok(&["reply", &id, "-m", "on it"], &as_w1, None);
    let mine = env.ok(&["inbox"], &[], None);
    assert!(mine.contains("w1 (agent): Re: Kickoff"), "{mine}");

    // A second login (another org) and switching between them.
    let reg2 = env.json(
        &[
            "register", &url, "--invite", &codes[1], "--org", "beta", "--name", "b",
        ],
        &[],
    );
    assert_eq!(reg2["org"], "beta");
    assert_eq!(env.json(&["org", "show"], &[])["name"], "beta");
    let profiles = env.ok(&["profile", "ls"], &[], None);
    assert!(profiles.contains("* b "), "{profiles}");
    let acme_profile = format!("acme@127.0.0.1:{}", url.rsplit(':').next().unwrap());
    env.ok(&["profile", "use", &acme_profile], &[], None);
    assert_eq!(env.json(&["org", "show"], &[])["name"], "acme");
    assert_eq!(
        env.json(&["--profile", "b", "org", "show"], &[])["name"],
        "beta"
    );
    // `amail login` with a token on stdin.
    env.ok(
        &["login", &url, "--name", "acme-again"],
        &[],
        Some(&format!("{user_token}\n")),
    );
    assert_eq!(env.json(&["org", "show"], &[])["name"], "acme");
    let err = env.fails(&["login", &url], &[]);
    assert!(err.contains("no token"), "{err}");

    // Rotation: the old token stops working, the saved logins follow.
    let new_token = env.ok(&["token", "rotate"], &[], None).trim().to_string();
    assert_ne!(new_token, user_token);
    let cfg = std::fs::read_to_string(&cfg_file).unwrap();
    assert!(cfg.contains(&new_token) && !cfg.contains(&user_token));
    let old = [
        ("AMAIL_URL", url.as_str()),
        ("AMAIL_TOKEN", user_token.as_str()),
    ];
    env.fails(&["org", "show"], &old);
    assert_eq!(env.json(&["org", "show"], &[])["name"], "acme");

    // The operator: statistics (no mail content), suspension, token reset.
    let stats = env.ok(&["admin", "stats", "--db", &db], &[], None);
    assert!(
        stats.contains("2 orgs (0 suspended), 1 agent seats"),
        "{stats}"
    );
    assert!(!stats.contains("Kickoff"), "{stats}");
    let st = env.json(&["admin", "stats", "--db", &db], &[]);
    assert_eq!(st["list"][0]["name"], "acme");
    assert_eq!(st["list"][0]["messages"], 2);
    let http = reqwest::Client::new();
    let ops = http
        .get(format!("{url}/v1/operator/stats"))
        .bearer_auth(&op_token)
        .send()
        .await
        .unwrap();
    assert_eq!(ops.status(), 200);
    env.ok(&["admin", "org", "suspend", "acme", "--db", &db], &[], None);
    let err = env.fails(&["org", "show"], &[]);
    assert!(err.contains("org_suspended"), "{err}");
    env.ok(&["admin", "org", "resume", "acme", "--db", &db], &[], None);
    env.ok(&["org", "show"], &[], None);
    let reset = env
        .ok(&["admin", "token", "reset", "acme", "--db", &db], &[], None)
        .trim()
        .to_string();
    env.fails(&["org", "show"], &[]);
    let by_reset = [("AMAIL_URL", url.as_str()), ("AMAIL_TOKEN", reset.as_str())];
    assert_eq!(env.json(&["org", "show"], &by_reset)["name"], "acme");
    // The agent's seat token is not affected.
    env.ok(&["inbox"], &as_w1, None);

    // The user deletes the org (confirmed with its name).
    let err = env.fails(&["org", "delete", "--confirm", "acm"], &by_reset);
    assert!(err.contains("does not match"), "{err}");
    let err = env.fails(&["org", "delete"], &by_reset);
    assert!(err.contains("--confirm acme"), "{err}");
    env.ok(&["org", "delete", "--confirm", "acme"], &by_reset, None);
    env.fails(&["inbox"], &as_w1);
    let orgs = env.ok(&["admin", "org", "ls", "--db", &db], &[], None);
    assert!(!orgs.contains("acme") && orgs.contains("beta"), "{orgs}");
    env.ok(
        &[
            "admin",
            "org",
            "delete",
            "beta",
            "--confirm",
            "beta",
            "--db",
            &db,
        ],
        &[],
        None,
    );

    // The server's log never holds a token, an invite code or a body.
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(text.contains("registered org acme"), "{text}");
    for secret in [
        user_token.as_str(),
        new_token.as_str(),
        reset.as_str(),
        w1.as_str(),
        op_token.as_str(),
        codes[0].as_str(),
        codes[1].as_str(),
        "the body of the first mail",
    ] {
        assert!(!text.contains(secret), "the log contains {secret}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn webhook_secrets_stay_out_of_logs() {
    let Some((db, _guard)) = fresh_url(Backend::Sqlite, "cli_webhook_log").await else {
        return;
    };
    let env = Env::new();
    let log = env.dir.path().join("serve.log");
    // Self-hosted: private webhook targets are allowed; this one refuses
    // connections.
    let (_server, url) = serve(&env, &db, &[], &log);
    let token = env.json(&["admin", "create-org", "acme", "--db", &db], &[])["token"]
        .as_str()
        .unwrap()
        .to_string();
    let me = [("AMAIL_URL", url.as_str()), ("AMAIL_TOKEN", token.as_str())];
    let dead = free_port();
    let hook = format!("http://127.0.0.1:{dead}/services/T000/B000/s3cr3t-path?key=s3cr3t-query");
    env.ok(&["org", "set", "--webhook-url", &hook], &me, None);
    env.ok(&["seat", "add", "a"], &me, None);
    let a = env
        .ok(&["seat", "token", "a"], &me, None)
        .trim()
        .to_string();
    let as_a = [("AMAIL_URL", url.as_str()), ("AMAIL_TOKEN", a.as_str())];
    env.ok(
        &[
            "send",
            "--to",
            "owner",
            "-s",
            "question",
            "-m",
            "private body",
        ],
        &as_a,
        None,
    );
    // Three attempts (1 s and 5 s apart), then a warning.
    for _ in 0..200 {
        if std::fs::read_to_string(&log).unwrap().contains("giving up") {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(text.contains("giving up"), "{text}");
    assert!(text.contains(&format!("http://127.0.0.1:{dead}")), "{text}");
    for secret in ["s3cr3t", "private body", token.as_str(), a.as_str()] {
        assert!(!text.contains(secret), "the log contains {secret}");
    }
}

#[test]
fn init_and_doctor() {
    let env = Env::new();
    // Stand-ins for the harnesses, at a verified and another version.
    let bin = env.dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    for (name, out) in [
        ("claude", "2.1.289 (Claude Code)"),
        ("codex", "codex-cli 0.170.2"),
    ] {
        let p = bin.join(name);
        std::fs::write(&p, format!("#!/bin/sh\necho '{out}'\n")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = format!("{}:/usr/bin:/bin", bin.display());
    let claude_dir = env.dir.path().join("claude-config");
    std::fs::create_dir_all(&claude_dir).unwrap();
    let cwd = env.dir.path().canonicalize().unwrap();
    std::fs::write(
        claude_dir.join(".claude.json"),
        serde_json::json!({ "projects": { cwd.to_string_lossy(): { "hasTrustDialogAccepted": true } } })
            .to_string(),
    )
    .unwrap();
    let vars = [
        ("PATH", path.as_str()),
        ("CLAUDE_CONFIG_DIR", claude_dir.to_str().unwrap()),
    ];

    let out = env.ok(&["init"], &[], None);
    assert!(out.contains("wrote amail.toml"), "{out}");
    let team = std::fs::read_to_string(env.dir.path().join("amail.toml")).unwrap();
    assert!(team.contains("template = \"staged-merge\""));
    env.fails(&["init"], &[]);
    env.ok(&["init", "--force"], &[], None);

    // No login, no node: explained; the exit status says whether anything
    // is broken (here: nothing, only warnings).
    let o = env.run_with(&["doctor"], &vars, None);
    let text = String::from_utf8_lossy(&o.stdout).into_owned();
    assert!(
        o.status.success(),
        "{text}{}",
        String::from_utf8_lossy(&o.stderr)
    );
    for want in [
        "ok   platform: linux",
        "warn login:",
        "-> amail login <server URL>",
        "info node: not running",
        "ok   claude: 2.1.289 (verified: Claude Code 2.1.x)",
        "warn codex: 0.170.2, not a verified version (codex-cli 0.160.x)",
        "ok   claude trust:",
        "ok   team file: amail.toml: 3 seats",
    ] {
        assert!(text.contains(want), "missing {want:?} in:\n{text}");
    }
    // A server that is down is an error.
    let down = format!("http://127.0.0.1:{}", free_port());
    let o = env.run_with(
        &["doctor"],
        &[
            ("PATH", path.as_str()),
            ("AMAIL_URL", down.as_str()),
            ("AMAIL_TOKEN", "amail_x"),
        ],
        None,
    );
    assert!(!o.status.success());
    let text = String::from_utf8_lossy(&o.stdout);
    assert!(text.contains("FAIL server:"), "{text}");
    // So is a broken team file.
    std::fs::write(env.dir.path().join("amail.toml"), "[team]\nnope = 1\n").unwrap();
    let o = env.run_with(&["doctor"], &vars, None);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stdout).contains("FAIL team file"));
}

#[test]
fn node_start_and_stop() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let Some((db, _guard)) = rt.block_on(fresh_url(Backend::Sqlite, "cli_node_service")) else {
        return;
    };
    let env = Env::new();
    let log = env.dir.path().join("serve.log");
    let (_server, url) = serve(&env, &db, &[], &log);
    let token = env.json(&["admin", "create-org", "acme", "--db", &db], &[])["token"]
        .as_str()
        .unwrap()
        .to_string();
    // The node takes the server from the saved login.
    env.ok(&["login", &url], &[], Some(&format!("{token}\n")));
    let saved = env.ok(&["node", "token", "--save"], &[], None);
    assert!(saved.starts_with("amail_"));
    let fake = env.dir.path().join("fake-harness");
    std::fs::write(&fake, "#!/bin/sh\nexit 0\n").unwrap();
    let fake = fake.to_str().unwrap();
    let out = env.ok(
        &[
            "node",
            "start",
            "--claude-bin",
            fake,
            "--codex-bin",
            fake,
            "--label",
            "test-host",
        ],
        &[],
        None,
    );
    assert!(out.contains("node started"), "{out}");
    let status = env.ok(&["node", "status"], &[], None);
    assert!(
        status.contains(&format!("node connected to server {url}")),
        "{status}"
    );
    let err = env.fails(&["node", "start"], &[]);
    assert!(err.contains("already running"), "{err}");
    let out = env.ok(&["node", "stop"], &[], None);
    assert!(out.contains("node stopped"), "{out}");
    env.fails(&["node", "status"], &[]);
    let nlog = std::fs::read_to_string(env.dir.path().join("state/node.log")).unwrap();
    assert!(!nlog.contains(&token), "the node log contains the token");
}
