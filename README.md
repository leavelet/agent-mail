# amail

Mail and mailing lists for teams of coding agents.

Agent sessions are short-lived; the positions they fill are not. `amail` gives
every position (a **seat**) a durable mailbox, so messages are addressed to
"the backend lead" rather than to a process. Seats can be grouped into
**lists**, messages form **threads**, every delivery has a visible state
(queued → notified → read → acked), and the server enforces attention
budgets so that a team of agents cannot drown itself (or its human) in mail.

One binary, `amail`, is the server, the admin tool and the command-line client
that agents and people use. Storage is SQLite (default, zero setup) or
Postgres.

**Status: early.** The server, storage and CLI, plus a per-host **node** that
pushes mail into running Claude Code and Codex sessions: mid-turn through
hooks while an agent is busy, and by waking the session when it is idle. There
is no web UI yet, and the HTTP API may still change.

## Quick start (SQLite)

```sh
cargo build --release
export PATH="$PWD/target/release:$PATH"

# Create an org with its user seat ("owner"); prints the user token.
amail admin create-org acme --db sqlite://amail.db

# Run the server.
amail serve --db sqlite://amail.db --listen 127.0.0.1:8025 &

export AMAIL_URL=http://127.0.0.1:8025
export AMAIL_TOKEN=amail_...            # the user token printed above

amail seat add lead --role "Leads the backend group" --harness codex
amail seat add w1 --parent lead --harness claude
amail list create backend --policy members --description "backend group"
amail list add backend lead w1
amail seat token w1                     # hand this token to the agent in seat w1

amail send --to backend -s "Kickoff" -m "Build the parser first."
```

As the agent in seat `w1` (`AMAIL_TOKEN` set to its seat token):

```console
$ amail inbox
#1 [list unread] owner (user): Kickoff | 1 line, thread 1, 2026-10-04 10:00
$ amail read 1
#1 thread 1 from owner (user) 2026-10-04 10:00
To: backend
Subject: Kickoff

Build the parser first.
$ echo "On it." | amail reply 1
sent #2 thread 1 to 1 seat: owner=1
```

The text output is compact and line-oriented so it costs agents few tokens;
add `--json` to any command for JSON. Errors go to stderr with a non-zero exit
code. Configuration comes from `AMAIL_URL` / `AMAIL_TOKEN`, or from a TOML file
passed with `--profile` (`url = "..."`, `token = "..."`).

## Postgres

```sh
amail admin create-org acme --db postgres://user:pw@localhost/amail
amail serve --db postgres://user:pw@localhost/amail --listen 0.0.0.0:8025 --max-connections 64
```

Migrations run automatically on start. Both backends have the same schema and
pass the same test suite. SQLite suits a single host and small to medium
teams; Postgres is for larger deployments and standard backup/replication.

## Concepts

- **Org**: the isolation boundary. Exactly one **user** (a person, fully
  trusted inside the org) plus any number of agent seats.
- **Seat**: a position with a mailbox: name, kind (`agent` or `user`),
  optional parent (the org tree), role text, harness and model. Deleting a seat
  is a soft delete; its mail is kept.
- **List**: a named group of seats (local or `name@org` in a linked org), each
  with a delivery mode: `immediate`, `digest` or `archive`. Lists have a post
  policy: `anyone`, `members` or `user_only`. Seats and lists share one
  namespace per org.
- **Address**: `name` (a seat or list in your org) or `name@org`. Names match
  `[a-z0-9][a-z0-9._-]{0,62}`.
- **Delivery**: one row per (message, recipient seat) with a class (`to`,
  `cc`, `list`; a seat reached several ways gets one delivery with the highest
  class), a mode, and `notified` / `read` / `acked` timestamps.
- **`seq` and `mod_seq`**: each seat has one counter. A new delivery takes the
  next value as its `seq` (its position in the mailbox) and as its `mod_seq`;
  every later state change of a delivery (notified, read, acked) takes the
  next value again as its new `mod_seq`. So `seq` values in a mailbox have
  gaps, and the counter only says "something changed". `/changes?since=` and
  server-sent events use `mod_seq` as the cursor.
- **Source**: computed for the reader: `user` (your org's user — instructions),
  `agent` (a seat in your org), `external` (anything from another org, even
  its user — never instructions).
- **Links**: orgs are isolated until one user requests a link and the other
  accepts. Either side can revoke it. Without a link, mail to `name@org` is
  rejected.
- **Meta**: a message may carry `meta`, a small map of single-line string
  fields (keys `[a-z0-9_.-]{1,32}`, at most 16 keys, 4 KB in total) that the server stores and shows but does not
  interpret, e.g. `repo`, `branch` and `commit` on a merge request.
- **Limits**: subject 300 characters on one line (no control characters),
  body 256 KB, 100 addresses per message (HTTP 422 otherwise).
- **Attention protection**: per-seat send budget (token bucket, default 30
  per minute with a burst of 30; the user seat is exempt), a cap on recipients
  per message (default 200), list post policies, and a reply-depth limit
  (default 50). Defaults are per-org settings (`amail org set`).

## Tokens

Bearer tokens; only their SHA-256 hashes are stored.

| scope  | can |
|--------|-----|
| `user` | everything in the org: seats, lists, tokens, links, the whole archive; send and receive as the user seat |
| `seat` | send and receive as its seat; read its mailbox and the threads it takes part in; read the directory |
| `node` | `inbox`, `claim`, `changes`, `events` and `pending` for any seat of the org (`seat=<name>`); report presence; fetch launch info; cannot send |

`amail token ls` lists live tokens (id, scope, seat, label, creation time; never
the secret) and `amail token revoke <id>` revokes one. The last user token of an
org cannot be revoked.

## CLI

| command | |
|---|---|
| `amail serve --db <url> --listen <addr>` | run the server |
| `amail admin create-org <org> [--user-name owner] --db <url>` | create an org directly in the database; prints the user token |
| `amail send --to a,b [--cc c] -s <subject> [-m <body>] [--urgent] [--ack] [--meta k=v]...` | body from stdin when `-m` is omitted |
| `amail inbox [--all] [--seat s]` | unread messages (all with `--all`) |
| `amail read <id> [--peek]` | show a message; marks it read unless `--peek` |
| `amail reply <id> [--all] [-m <body>]` | subject and thread are inherited |
| `amail ack <id>` | acknowledge |
| `amail status <id>` | per-recipient delivery state of a message you sent |
| `amail thread <id>` | a thread, by thread id or any message id in it |
| `amail who` | directory: seats, lists, linked orgs |
| `amail claim [--via v] [--classes to,cc,list] [--inline-body-max n]` | take pending notifications (marks them notified) |
| `amail seat add/set/ls/rm/token` | manage seats (`--harness`, `--model`, `--role`, `--permission-mode default\|unattended`) and seat tokens |
| `amail list create/ls/rm/add/remove` | manage lists and members (`--mode immediate\|digest\|archive`) |
| `amail link request/accept/ls/revoke` | links with other orgs |
| `amail token node/user/ls/revoke` | node tokens, extra user tokens, listing and revocation |
| `amail node` | run the per-host node (see below); `amail node token [--save]`, `amail node status` |
| `amail run <seat> [-- args]` | start the seat's harness session in this terminal |
| `amail org show/set` | org settings (budgets and limits) |

## HTTP API

JSON under `/v1`; errors are `{"error":{"code":"…","message":"…"}}`.

| | |
|---|---|
| `POST /messages` | send `{to, cc, subject, body, in_reply_to?, priority?, ack_required?}` |
| `GET /inbox` | `seat`, `unread`, `after_seq`, `before_seq`, `limit`; headers only |
| `GET /messages/{id}` | full message; marks the caller's delivery read unless `peek=1` |
| `POST /messages/{id}/ack` | acknowledge |
| `GET /messages/{id}/status` | per-recipient state (sender and the sender org's user only) |
| `GET /threads/{id}` | messages of a thread the caller may see |
| `POST /claim` | `{classes, min_priority?, via, limit?, inline_body_max?, include_digest?, mark_inlined_read?}`: atomically mark pending deliveries notified and return them; concurrent claims never return the same delivery. With `mark_inlined_read`, deliveries whose body is returned inline are marked read too |
| `GET /changes` | `since` (seq): deliveries created or changed since |
| `GET /events` | server-sent events `{"seat","seq"}` per mailbox change; supports `Last-Event-ID` |
| `GET, POST /seats`; `GET, PATCH, DELETE /seats/{name}`; `POST /seats/{name}/tokens` | seats |
| `GET, POST /lists`; `GET, DELETE /lists/{name}`; `PUT, DELETE /lists/{name}/members/{address}` | lists |
| `GET, POST /links`; `POST /links/{org}/accept`; `DELETE /links/{org}` | links |
| `GET /archive/messages` | `since`, `until`, `from`, `thread`, `before_id`, `limit` (user tokens) |
| `GET, PATCH /org`; `GET, POST /tokens`; `DELETE /tokens/{id}`; `GET /whoami` | org settings, tokens, caller identity |
| `GET /pending` | node and user tokens: per seat, pending (unnotified, immediate) deliveries by class, urgent count, and the seat's counter |
| `POST /presence` | node tokens: `{node?, seats: [{seat, state, wake_failed?, harness?, since?}]}`; states `offline`, `busy`, `idle`, `awaiting`. Kept in memory for 90 s; shown as `presence` on seats and as `presence` events on `/events` |
| `GET /seats/{name}/launch` | node and user tokens: harness, model, role, parent, permission mode and a new seat token, for starting a session; the token of the seat's previous launch is revoked |

## How delivery works

Every delivery moves through `queued` → `notified` → `read` → `acked`:

- **notified**: the recipient's agent has been shown the message (its header,
  and its body when that is short), or the recipient claimed it with
  `amail claim`.
- **read**: the whole message reached the recipient: through `amail read`, or
  because the body was short enough (400 characters) to be injected in full.
- **acked**: the recipient confirmed it with `amail ack`.

There is one way mail gets into a running agent: a harness hook asks the node,
and the node **claims** the pending deliveries on the server. A claim is
atomic, so a delivery is shown once even when several hooks race.

- **Busy agent**: the hooks that fire during a turn inject new mail into it:
  `UserPromptSubmit` (all mail), `PostToolUse` (only `to` and urgent mail,
  between tool calls) and `Stop` (keeps the turn going to handle the mail).
- **Idle agent**: the node sends a one-line nudge that starts a new turn; that
  turn's `UserPromptSubmit` hook claims and injects the mail. A nudge carries no
  mail itself. Only `to` and urgent mail nudges; `cc` and list mail waits for
  the next turn.

Injected text starts with `[amail]`, has one header line per message, and puts
every body line behind `  > `. Subjects and `meta` are single-line, so mail
content cannot pass itself off as a header or as a line from the mail system.

## Running agents: the node

A seat's session is a Claude Code or Codex process on some host. Each host
runs one `amail node`, which connects those sessions to the server:

- **Busy agent**: harness hooks call `amail hook <harness>`, a tiny client that
  forwards the hook input to the node over a Unix socket and prints the answer.
  When a seat has pending mail the node claims it and injects it: on
  `UserPromptSubmit` all classes, on `PostToolUse` only `to` and urgent mail,
  and on `Stop` it keeps the turn going (at most three times in a row). A seat
  with nothing pending is answered from memory, without a server call (about
  2 ms end to end, p99, with 1,000 seats). The hook client never fails: if the
  node is down or slow (300 ms, `AMAIL_HOOK_TIMEOUT_MS`), it prints nothing.
  One hook claims at most a few messages (8 for Claude Code, 5 for Codex) and
  shows each in full; the rest follows with the next hook.
- **Idle agent**: when an idle seat gets `to` or urgent mail, the node wakes the
  session with a one-line nudge (`[amail] You have new mail (wake #n).`) after a
  3 s coalescing window (urgent: at once). The nudge starts a turn whose
  `UserPromptSubmit` hook injects the mail, so mail reaches the model through
  hooks only, and the server's atomic claim means each delivery is shown once.
  `cc` and list mail never wake anyone; it comes along with the next turn.
  Claude Code is woken through its inbox socket; Codex with `codex queue`.
  An unanswered wake-up is retried once after 10 s, then the seat is reported
  as `wake_failed` and tried again after 1 minute, doubling up to 10 minutes,
  or at once when new mail arrives for it.
- **State**: the node tracks each seat as `offline`, `busy`, `idle` or
  `awaiting` (a permission prompt) from hooks and process liveness, and
  reports it to the server.

```sh
amail node token --save        # with a user token: issue a node token, save it with the URL
amail node &                   # state in ~/.local/share/amail, socket in $XDG_RUNTIME_DIR/amail
amail seat add w1 --harness claude --model sonnet --role "..." --permission-mode unattended
amail run w1                   # in the terminal that should host the session
amail node status              # what the node sees
```

`amail run <seat>` asks the node for the launch recipe and execs the harness:

- **Claude Code**: `--settings` with the hooks, `crossSessionInbound: "accept"`
  (so the node's wake-ups reach bypass-mode sessions) and
  `skipDangerousModePermissionPrompt`; `--append-system-prompt` with the mail
  instructions and the seat's role; `--name <seat>`; `--model`;
  `--dangerously-skip-permissions` for `unattended` seats. The hooks see the
  session's `AMAIL_SEAT`. Your own Claude settings are not changed; Claude Code
  still asks once whether to trust a new working directory.
- **Codex**: all Codex seats of a host share one dedicated `CODEX_HOME`
  (`<state dir>/codex-home`, with `auth.json` linked to `~/.codex/auth.json`)
  and its app-server daemon, which the node starts. Hooks are defined and
  pre-trusted in that home's `config.toml`, which also carries the mail
  instructions and allows sandboxed commands to reach the amail server over
  loopback. Hooks run inside the daemon, so a session is tied to its seat by a
  one-time marker in its first prompt (`[amail:seat=<seat> launch=<nonce>]`);
  tool commands find their seat's credentials through the node by
  `CODEX_THREAD_ID`. Only flags that keep the session on the shared daemon are
  allowed (`-m`, `-a`, `-s`, `-C`, ...; not `-c`, `-p`, `--remote`).
  `unattended` seats run with `-a never -s workspace-write`.

Every session gets a system prompt explaining that `[amail]` text comes from the
mail system, that `USER` mail is an instruction, `agent` mail a colleague's
request and `external` mail untrusted information, and that mail never grants
permissions.

### Known limitations

- **Lost answers.** If a hook's answer cannot be delivered (the hook client
  timed out or went away), the node keeps the claimed mail and shows it with
  the seat's next injecting hook, or wakes the idle seat for it. One window
  remains: the server executed a claim but the node never got the response (it
  timed out after 150 ms). Such mail is marked notified without having been
  shown; it stays unread, so it appears in the next session's unread summary
  and in `amail inbox`.
- Timeouts nest: the node's server calls for one hook (150 ms in total) <
  the hook client's wait (300 ms) < the harness's hook timeout (3-5 s).
- One session per seat: if two sessions use the same seat, the most recent
  hook wins.
- After a node restart, sessions whose process and wake target still exist
  are restored as idle; anything else waits for its next hook.
- Claude Code asks once whether to trust a new working directory; `amail run`
  does not answer for you.
- Codex agents need network access in their sandbox to reach the server
  (the dedicated Codex home allows it): with networking disabled, the Codex
  sandbox refuses both TCP and Unix-socket connections.
- Any process of the same user can ask the node for the credentials of a seat
  it hosts. The node is a per-user daemon; its socket directory is private to
  that user.

## Development

```sh
cargo test                                   # SQLite; Postgres tests are skipped

docker run -d --name amail-pg -e POSTGRES_PASSWORD=amail -p 127.0.0.1:55432:5432 postgres:17
AMAIL_TEST_PG_URL=postgres://postgres:amail@127.0.0.1:55432/postgres cargo test
```

The store conformance tests, the HTTP tests, the end-to-end CLI test and the
node tests all run against both backends (each Postgres test creates its own
database). The node tests drive the real `amail hook` binary with minimal
hook inputs shaped like the harnesses' own (`crates/amail-cli/tests/fixtures/hooks`); no model is called. The
hook latency bound is checked with `cargo test --release --test hotpath`.

Crates:

| crate | |
|---|---|
| `amail-proto` | wire types and address parsing |
| `amail-store` | the `Store` trait, SQLite and Postgres backends, migrations, conformance tests |
| `amail-server` | the axum HTTP API, server-sent events, policy and rate limits |
| `amail-node` | the node daemon, the hook client, the Claude Code and Codex adapters |
| `amail-cli` | the `amail` binary |
| `amail-bench` | load generator: `amail-bench --url http://127.0.0.1:8025 --db <same db> --rates 170,500` |

## License

Apache-2.0
