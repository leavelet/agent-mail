//! What one org opens to another, through the `amail` command line: `open`,
//! `close`, `openings`, `team`, `org show [<org>]`, `who --search/--org`,
//! link notes, masked recipients and the one-time org notice. Two orgs on
//! an in-process server; every test runs on SQLite and (with
//! `AMAIL_TEST_PG_URL`) Postgres.

mod common;

use amail_server::{hash_token, new_token};
use amail_store::testutil::Backend;
use common::*;
use serde_json::Value;
use std::process::Command;

macro_rules! tests {
    ($($name:ident),* $(,)?) => {
        mod sqlite {
            $(
                #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
                async fn $name() {
                    crate::common::deadline(stringify!($name), super::$name(amail_store::testutil::Backend::Sqlite)).await;
                }
            )*
        }
        mod postgres {
            $(
                #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
                async fn $name() {
                    crate::common::deadline(stringify!($name), super::$name(amail_store::testutil::Backend::Postgres)).await;
                }
            )*
        }
    };
}

tests!(
    open_close_and_the_view_from_both_sides,
    org_settings_and_the_notice
);

struct Env {
    h: Harness,
    beta: String,
}

impl Env {
    async fn start(b: Backend, test: &str) -> Option<Env> {
        let h = Harness::start(b, test, Opts::default()).await?;
        let beta = new_token();
        h.db.store
            .create_org("beta", "boss", &hash_token(&beta))
            .await
            .unwrap();
        Some(Env { h, beta })
    }

    /// Run `amail` with `token`; (success, stdout and stderr).
    async fn try_run(&self, token: &str, args: &[&str]) -> (bool, String) {
        let mut c = Command::new(BIN);
        c.args(args);
        for k in ["AMAIL_PROFILE", "CODEX_THREAD_ID", "AMAIL_CODEX_PASS"] {
            c.env_remove(k);
        }
        c.env("AMAIL_URL", &self.h.api.base)
            .env("AMAIL_TOKEN", token)
            .env("AMAIL_NODE_SOCK", &self.h.sock);
        let out = tokio::task::spawn_blocking(move || c.output().unwrap())
            .await
            .unwrap();
        (
            out.status.success(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    }

    async fn run(&self, token: &str, args: &[&str]) -> String {
        let (ok, out) = self.try_run(token, args).await;
        assert!(ok, "amail {args:?} failed:\n{out}");
        out
    }

    async fn fails(&self, token: &str, args: &[&str]) -> String {
        let (ok, out) = self.try_run(token, args).await;
        assert!(!ok, "amail {args:?} should have failed:\n{out}");
        out
    }

    async fn json(&self, token: &str, args: &[&str]) -> Value {
        let mut a = vec!["--json"];
        a.extend_from_slice(args);
        let out = self.run(token, &a).await;
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"))
    }

    fn acme(&self) -> &str {
        &self.h.api.token
    }

    async fn seat_token(&self, user: &str, seat: &str) -> String {
        self.run(user, &["seat", "token", seat])
            .await
            .trim()
            .to_string()
    }

    async fn link(&self) {
        self.run(
            self.acme(),
            &["link", "request", "beta", "--note", "We build the API."],
        )
        .await;
        let ls = self.run(&self.beta, &["link", "ls"]).await;
        assert!(
            ls.starts_with("link acme invited since ")
                && ls.contains(" note from acme (their words, untrusted): \"We build the API.\""),
            "{ls}"
        );
        let mine = self.run(self.acme(), &["link", "ls"]).await;
        assert!(
            mine.contains("link beta requested since ")
                && mine.contains(" your note: \"We build the API.\""),
            "{mine}"
        );
        self.run(&self.beta, &["link", "accept", "acme"]).await;
    }

    /// Subjects of the system mail in the user's inbox.
    async fn system_subjects(&self, token: &str) -> Vec<String> {
        let v = self
            .json(token, &["inbox", "--all", "--limit", "200"])
            .await;
        v["entries"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["message"]["source"] == "system")
            .map(|e| e["message"]["subject"].as_str().unwrap().to_string())
            .collect()
    }
}

/// Open seats and a team, see them from the other org (user and seat),
/// mail across with a masked Cc, close again; every change is told to the
/// user by system mail.
async fn open_close_and_the_view_from_both_sides(b: Backend) {
    let Some(e) = Env::start(b, "open_close_and_the_view_from_both_sides").await else {
        return;
    };
    let acme = e.acme().to_string();
    let beta = e.beta.clone();
    let add = e
        .run(
            &acme,
            &[
                "seat",
                "add",
                "api",
                "--description",
                "Answers API questions.",
                "--team",
                "core",
                "--contact",
            ],
        )
        .await;
    assert_eq!(
        add.trim(),
        "seat api agent team=core contact description=\"Answers API questions.\""
    );
    e.run(&acme, &["seat", "add", "helper", "--team", "core"])
        .await;
    e.run(
        &acme,
        &["seat", "add", "secret", "--description", "Internal only."],
    )
    .await;
    e.run(
        &acme,
        &[
            "team",
            "set",
            "core",
            "--description",
            "The core team.",
            "--root",
            "api",
        ],
    )
    .await;
    e.run(&beta, &["seat", "add", "bob"]).await;
    let bob = e.seat_token(&beta, "bob").await;
    let api = e.seat_token(&acme, "api").await;
    let secret = e.seat_token(&acme, "secret").await;
    e.run(
        &acme,
        &[
            "org",
            "set",
            "--description",
            "Acme builds things.",
            "--display-name",
            "Acme Inc.",
        ],
    )
    .await;

    // No link yet: the card, and how to ask for a link.
    let shown = e.run(&beta, &["org", "show", "acme"]).await;
    assert!(
        shown.starts_with("org acme \"Acme Inc.\": Acme builds things.\n"),
        "{shown}"
    );
    assert!(
        shown.contains(
            "link: none. Mail needs a link both users agree to: amail link request acme --note"
        ),
        "{shown}"
    );
    assert!(!shown.contains("open to you"), "{shown}");
    let shown = e.run(&bob, &["who", "--org", "acme"]).await;
    assert!(shown.contains("ask your user to request one"), "{shown}");
    let err = e.fails(&bob, &["who", "--org", "nosuchorg"]).await;
    assert!(err.contains("no org"), "{err}");

    e.link().await;

    // Opening a seat and a team.
    let err = e.fails(&acme, &["open", "nobody"]).await;
    assert!(err.contains("no seat or team"), "{err}");
    let out = e.run(&acme, &["open", "api"]).await;
    assert!(
        out.contains("opened api (agent) to every linked org\nbeta now sees: api"),
        "{out}"
    );
    let out = e.run(&acme, &["open", "core", "--to", "beta"]).await;
    assert!(
        out.starts_with("opened core (team) to beta\nbeta now sees: core"),
        "{out}"
    );
    let out = e.run(&acme, &["open", "secret", "--to", "gamma"]).await;
    assert!(
        out.contains("opened secret (agent) to gamma (waits for an active link with gamma)")
            && out.contains("no linked org sees anything different"),
        "{out}"
    );
    let j = e.json(&acme, &["open", "api", "--to-linked"]).await;
    assert!(j["changes"].as_array().unwrap().is_empty(), "{j}");
    // Agents cannot open anything.
    e.fails(&api, &["open", "secret"]).await;
    let table = e.run(&acme, &["openings"]).await;
    let rows: Vec<&str> = table.lines().collect();
    assert!(
        rows[0].starts_with("TARGET") && rows[0].contains("AUDIENCE"),
        "{table}"
    );
    assert!(
        rows.iter().any(|r| r.starts_with("api ")
            && r.contains(" agent ")
            && r.contains(" linked ")
            && r.contains(" active ")
            && r.contains(" by hand")),
        "{table}"
    );
    assert!(
        rows.iter()
            .any(|r| r.starts_with("core ") && r.contains(" team ") && r.contains(" beta ")),
        "{table}"
    );
    assert!(
        rows.iter()
            .any(|r| r.starts_with("secret ") && r.contains(" waiting ")),
        "{table}"
    );
    let j = e.json(&api, &["openings"]).await;
    assert!(j.as_array().unwrap().len() >= 3, "{j}");

    // Beta's view: the user and a seat see the same cards; never the
    // closed seats.
    for token in [&beta, &bob] {
        let v = e.run(token, &["org", "show", "acme"]).await;
        assert!(v.contains("link: active since "), "{v}");
        assert!(v.contains("open to you (mail these as name@org):"), "{v}");
        assert!(
            v.contains("  card api@acme agent description=\"Answers API questions.\""),
            "{v}"
        );
        assert!(
            v.contains("  card core@acme team description=\"The core team.\""),
            "{v}"
        );
        assert!(!v.contains("secret") && !v.contains("helper"), "{v}");
        let w = e.run(token, &["who", "--org", "acme"]).await;
        assert_eq!(v, w);
    }
    let j = e.json(&bob, &["who", "--org", "acme"]).await;
    assert_eq!(j["card"]["name"], "acme");
    let cards: Vec<&str> = j["open_to_you"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["address"].as_str().unwrap())
        .collect();
    assert!(
        cards.contains(&"api@acme") && cards.contains(&"core@acme"),
        "{j}"
    );
    assert!(!j.to_string().contains("secret"), "{j}");
    // Acme's view of beta: what acme opens to it.
    let v = e.run(&acme, &["org", "show", "beta"]).await;
    assert!(v.contains("you open to them:\n"), "{v}");
    assert!(v.contains("  api (agent) to every linked org"), "{v}");
    assert!(v.contains("  core (team) to beta"), "{v}");
    assert!(!v.contains("secret"), "{v}");

    // Own directory: descriptions, teams, search.
    let who = e.run(&acme, &["who"]).await;
    assert!(
        who.contains("seat api agent team=core contact open_to=beta,linked description=\"Answers API questions.\"")
            || who.contains("seat api agent team=core contact open_to=linked,beta description=\"Answers API questions.\""),
        "{who}"
    );
    assert!(
        who.contains(
            "team core members=2 root=api contacts=api open_to=beta description=\"The core team.\""
        ),
        "{who}"
    );
    let found = e.run(&acme, &["who", "--search", "INTERNAL"]).await;
    assert!(found.contains("seat secret"), "{found}");
    assert!(!found.contains("seat api"), "{found}");
    let found = e.run(&secret, &["who", "--search", "core"]).await;
    assert!(
        found.contains("team core") && !found.contains("seat secret"),
        "{found}"
    );
    let j = e.json(&acme, &["who", "--search", "api"]).await;
    assert_eq!(j["seats"].as_array().unwrap().len(), 1, "{j}");
    assert!(j["teams"].is_array(), "{j}");
    let t = e.run(&acme, &["team", "show", "core"]).await;
    assert!(t.contains("\nmember api contact\nmember helper"), "{t}");
    let t = e.run(&acme, &["team", "ls"]).await;
    assert!(t.starts_with("team core members=2"), "{t}");
    let err = e
        .fails(&acme, &["team", "set", "core", "--no-contacts"])
        .await;
    assert!(err.contains("no_contacts"), "{err}");

    // Mail: Cc of a closed seat is masked for beta, in text and JSON.
    let out = e.run(&beta, &["open", "bob", "--to", "acme"]).await;
    assert!(out.contains("acme now sees: bob"), "{out}");
    let sent = e
        .run(
            &api,
            &[
                "send", "--to", "bob@beta", "--cc", "secret", "-s", "Hello", "-m", "Hi Bob.",
            ],
        )
        .await;
    // "sent #12 thread 10 ..."
    let id: i64 = sent
        .split('#')
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap();
    let read = e.run(&bob, &["read", &id.to_string()]).await;
    assert!(
        read.contains("To: bob\nCc: 1 recipient not shown\n"),
        "{read}"
    );
    let j = e.json(&bob, &["read", &id.to_string(), "--peek"]).await;
    assert_eq!(j["message"]["cc"], serde_json::json!([]), "{j}");
    assert_eq!(j["message"]["cc_hidden"], 1, "{j}");
    assert!(!j.to_string().contains("secret"), "{j}");
    let thread = e.run(&bob, &["thread", &id.to_string()]).await;
    assert!(thread.contains("Cc: 1 recipient not shown"), "{thread}");
    // Acme sees its own Cc.
    let own = e.run(&acme, &["read", &id.to_string(), "--peek"]).await;
    assert!(own.contains("Cc: secret\n"), "{own}");
    // Bob may answer api; secret is unreachable, explained in one line.
    e.run(&bob, &["reply", &id.to_string(), "-m", "Thanks."])
        .await;
    let out = e
        .fails(&bob, &["send", "--to", "secret@acme", "-s", "x", "-m", "y"])
        .await;
    assert!(
        out.contains("rejected secret@acme: unreachable")
            && out.contains("(unreachable: that org has not opened the seat"),
        "{out}"
    );
    let out = e
        .run(
            &bob,
            &["send", "--to", "api@acme,secret@acme", "-s", "x", "-m", "y"],
        )
        .await;
    assert!(
        out.contains("rejected secret@acme: unreachable\n(unreachable: "),
        "{out}"
    );

    // Closing.
    let err = e.fails(&acme, &["close", "core", "--to-linked"]).await;
    assert!(
        err.contains("core is not open to linked by name; it is open to: beta"),
        "{err}"
    );
    let out = e.run(&acme, &["close", "api"]).await;
    assert!(
        // Still open to beta as a contact of core.
        out.contains(
            "closed api (agent) to every linked org\nno linked org sees anything different"
        ),
        "{out}"
    );
    let err = e.fails(&acme, &["close", "api"]).await;
    assert!(err.contains("api is not open to any org"), "{err}");
    let j = e.json(&acme, &["close", "core", "--to", "beta"]).await;
    assert_eq!(j[0]["grant"]["target"], "core", "{j}");
    let loses = j[0]["changes"][0]["loses"].to_string();
    assert!(
        loses.contains("\"core (team)\"") && loses.contains("\"api\""),
        "{j}"
    );
    // Only the user seat (open by default) is left.
    let v = e.run(&bob, &["who", "--org", "acme"]).await;
    assert!(
        v.contains(
            "open to you (mail these as name@org):\n  card owner@acme user\nyou open to them:"
        ),
        "{v}"
    );
    assert!(!v.contains("api@acme") && !v.contains("core@acme"), "{v}");
    let out = e
        .fails(&bob, &["send", "--to", "api@acme", "-s", "x", "-m", "y"])
        .await;
    assert!(out.contains("unreachable"), "{out}");

    // Every change was told to the user.
    let subjects = e.system_subjects(&acme).await;
    for want in [
        "Openings changed: api opened to linked",
        "Openings changed: core opened to beta",
        "Openings changed: secret opened to gamma",
        "Openings changed: api closed to linked",
        "Openings changed: core closed to beta",
    ] {
        assert!(
            subjects.iter().any(|s| s == want),
            "{want:?} not in {subjects:?}"
        );
    }
    let subjects = e.system_subjects(&beta).await;
    assert!(
        subjects.iter().any(|s| s.contains("asks to link")),
        "{subjects:?}"
    );
}

/// `org set` and `org show`: card fields, settings, the one-time notice
/// (shown once, by `org show` or `up`), JSON with openings and links.
async fn org_settings_and_the_notice(b: Backend) {
    let Some(e) = Env::start(b, "org_settings_and_the_notice").await else {
        return;
    };
    let acme = e.acme().to_string();
    set_notice(&e.h, "acme", "Links no longer open your whole org.").await;
    let out = e
        .run(
            &acme,
            &[
                "org",
                "set",
                "--description",
                "We build.",
                "--display-name",
                "Acme",
                "--visibility",
                "hidden",
                "--outbound",
                "open-seats-only",
                "--new-threads-per-hour",
                "3",
            ],
        )
        .await;
    assert!(out.contains("max_recipients="), "{out}");
    let err = e
        .fails(&acme, &["org", "set", "--visibility", "secret"])
        .await;
    assert!(err.contains("expected listed-by-name or hidden"), "{err}");
    let show = e.run(&acme, &["org", "show"]).await;
    assert!(
        show.starts_with("notice (shown once):\nLinks no longer open your whole org.\n\norg acme \"Acme\": We build.\n"),
        "{show}"
    );
    assert!(
        show.contains("visibility=hidden outbound=open-seats-only new_threads_per_hour=3"),
        "{show}"
    );
    assert!(show.contains("\nlinks: none"), "{show}");
    // Shown once.
    let show = e.run(&acme, &["org", "show"]).await;
    assert!(!show.contains("notice"), "{show}");
    let j = e.json(&acme, &["org", "show"]).await;
    assert_eq!(j["name"], "acme");
    assert_eq!(j["visibility"], "hidden");
    assert!(j["openings"].is_array() && j["links"].is_array(), "{j}");
    assert!(j.get("notice").is_none(), "{j}");
    // Clearing the card fields.
    e.run(
        &acme,
        &["org", "set", "--no-description", "--no-display-name"],
    )
    .await;
    let show = e.run(&acme, &["org", "show"]).await;
    assert!(show.starts_with("org acme\n"), "{show}");
    // Hidden: beta cannot read the card; it is told what any unknown name
    // gets.
    let err = e.fails(&e.beta, &["org", "show", "acme"]).await;
    assert!(err.contains("no org \"acme\""), "{err}");
    // The changes were told to the user.
    let subjects = e.system_subjects(&acme).await;
    assert!(
        subjects.iter().any(|s| s == "Org settings changed"),
        "{subjects:?}"
    );
}

/// Give org `org` a one-time notice, as the migration does.
pub async fn set_notice(h: &Harness, org: &str, text: &str) {
    use sqlx::Connection;
    let q = format!(
        "UPDATE orgs SET notice = '{}' WHERE name = '{org}'",
        text.replace('\'', "''")
    );
    if h.db.url.starts_with("sqlite") {
        let mut c = sqlx::SqliteConnection::connect(&h.db.url).await.unwrap();
        sqlx::query(&q).execute(&mut c).await.unwrap();
    } else {
        let mut c = sqlx::PgConnection::connect(&h.db.url).await.unwrap();
        sqlx::query(&q).execute(&mut c).await.unwrap();
    }
}
