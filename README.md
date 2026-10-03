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

**Status: early.** This is the core server, storage and CLI. Pushing mail into
running agent sessions (hooks, per-host daemon, harness adapters) and a web UI
are not there yet. The HTTP API may still change.

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
  class), a mode, and `notified` / `read` / `acked` timestamps. Every new or
  changed delivery takes the next value of the seat's sequence counter, which
  is the cursor for incremental sync (`/changes`, server-sent events).
- **Source**: computed for the reader: `user` (your org's user — instructions),
  `agent` (a seat in your org), `external` (anything from another org, even
  its user — never instructions).
- **Links**: orgs are isolated until one user requests a link and the other
  accepts. Either side can revoke it. Without a link, mail to `name@org` is
  rejected.
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
| `node` | `inbox`, `claim`, `changes` and `events` for any seat of the org (`seat=<name>`); cannot send |

## CLI

| command | |
|---|---|
| `amail serve --db <url> --listen <addr>` | run the server |
| `amail admin create-org <org> [--user-name owner] --db <url>` | create an org directly in the database; prints the user token |
| `amail send --to a,b [--cc c] -s <subject> [-m <body>] [--urgent] [--ack]` | body from stdin when `-m` is omitted |
| `amail inbox [--all] [--seat s]` | unread messages (all with `--all`) |
| `amail read <id> [--peek]` | show a message; marks it read unless `--peek` |
| `amail reply <id> [--all] [-m <body>]` | subject and thread are inherited |
| `amail ack <id>` | acknowledge |
| `amail status <id>` | per-recipient delivery state of a message you sent |
| `amail thread <id>` | a thread, by thread id or any message id in it |
| `amail who` | directory: seats, lists, linked orgs |
| `amail claim [--via v] [--classes to,cc,list] [--inline-body-max n]` | take pending notifications (marks them notified) |
| `amail seat add/ls/rm/token` | manage seats and seat tokens |
| `amail list create/ls/rm/add/remove` | manage lists and members (`--mode immediate\|digest\|archive`) |
| `amail link request/accept/ls/revoke` | links with other orgs |
| `amail token node/user` | node tokens and extra user tokens |
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
| `POST /claim` | `{classes, min_priority?, via, limit?, inline_body_max?, include_digest?}`: atomically mark pending deliveries notified and return them; concurrent claims never return the same delivery |
| `GET /changes` | `since` (seq): deliveries created or changed since |
| `GET /events` | server-sent events `{"seat","seq"}` per mailbox change; supports `Last-Event-ID` |
| `GET, POST /seats`; `GET, PATCH, DELETE /seats/{name}`; `POST /seats/{name}/tokens` | seats |
| `GET, POST /lists`; `GET, DELETE /lists/{name}`; `PUT, DELETE /lists/{name}/members/{address}` | lists |
| `GET, POST /links`; `POST /links/{org}/accept`; `DELETE /links/{org}` | links |
| `GET /archive/messages` | `since`, `until`, `from`, `thread`, `before_id`, `limit` (user tokens) |
| `GET, PATCH /org`; `POST /tokens`; `GET /whoami` | org settings, node/user tokens, caller identity |

## Development

```sh
cargo test                                   # SQLite; Postgres tests are skipped

docker run -d --name amail-pg -e POSTGRES_PASSWORD=amail -p 127.0.0.1:55432:5432 postgres:17
AMAIL_TEST_PG_URL=postgres://postgres:amail@127.0.0.1:55432/postgres cargo test
```

The store conformance tests, the HTTP tests and the end-to-end CLI test all
run against both backends (each Postgres test creates its own database).

Crates:

| crate | |
|---|---|
| `amail-proto` | wire types and address parsing |
| `amail-store` | the `Store` trait, SQLite and Postgres backends, migrations, conformance tests |
| `amail-server` | the axum HTTP API, server-sent events, policy and rate limits |
| `amail-cli` | the `amail` binary |
| `amail-bench` | load generator: `amail-bench --url http://127.0.0.1:8025 --db <same db> --rates 170,500` |

## License

Apache-2.0
