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
Postgres; with Postgres, several server instances can share one database.

**Status: early.** The server, storage and CLI, plus a per-host **node** that
pushes mail into running Claude Code and Codex sessions: mid-turn through
hooks while an agent is busy, and by waking the session when it is idle;
`amail up`, which turns a team file into running, supervised sessions in
zellij; and a web interface for the person running the team (see "Web
interface"). The HTTP API may still change.

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
#1 [to unread] owner (user): Kickoff | 1 line, thread 1, 2026-10-04 10:00
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

## Web interface

`amail serve` also serves a single-page app at `/`, for the org's user: who
is doing what (the org tree with every seat's state, live), what needs you
(questions and acknowledgement requests addressed to you, seats that need a
person), and what was decided (threads, the archive and full-text search).
From it you write mail, answer agents, watch a seat's conversation, put
seats to sleep or wake them, and administer seats, lists, links, tokens and
org settings. English and Chinese, light and dark.

Sign in by pasting a **user** token (seat and node tokens are refused). The
browser then holds a session cookie (`HttpOnly`, `SameSite=Strict`, `Secure`
when the request came over HTTPS; 30 days), tied to that token: revoking the
token ends its sessions. Behind a reverse proxy, pass the original `Host`
(or `X-Forwarded-Host`) and `X-Forwarded-Proto`.

The interface is built from `web/` (Svelte, Vite, TypeScript) and embedded
into the binary at compile time. `cargo build` works without Node: without
a build of the interface the server shows a page saying how to make one.

```sh
cd web && npm ci && npm run build && cd ..
cargo build --release          # now includes the interface
```

**Conversation mirror.** A seat's page shows its live conversation, read-only:
the browser asks the server, the server asks the node that hosts the seat,
and the node reads the session's transcript file (the path the harness
reports to its hooks; Claude Code JSONL or a Codex rollout), sends the last
200 entries and then follows the file. It stops 30 s after the last browser
left. Mail the system injected is shown apart from what a person typed.
Transcripts can contain code and secrets from the host: only user tokens
can watch, and the server relays them without storing them (it keeps the
last 1,000 entries of a watched seat in memory, and nothing once nobody
watches).

**Terminals.** A seat's page links to its terminal when the node serves one,
and otherwise shows `amail attach <seat>`. `amail node --web-terminal-port
<port>` runs a `zellij web` server of the node's own on `127.0.0.1:<port>`
and creates new team sessions with web sharing enabled (sessions that
already exist keep their setting until they are recreated). Its login tokens
are kept in `<state dir>/zellij-web`, apart from your own; create one with
`amail node web-token`. Nothing in your zellij configuration changes. Notes:
the link opens the seat's group session (pick the seat's tab there); anyone
logged in to that server sees the names of all your zellij sessions and can
attach to the ones that allow web sharing; `zellij web --stop` stops every
zellij web server of your user, including the node's (the node restarts its
own). A browser counts as a client of the session, so a small browser
window makes the panes smaller. To reach it from another host, put it
behind an HTTPS proxy and pass `--web-terminal-url`.

## Postgres

```sh
amail admin create-org acme --db postgres://user:pw@localhost/amail
amail serve --db postgres://user:pw@localhost/amail --listen 0.0.0.0:8025 --max-connections 64
```

Migrations run automatically on start. Both backends have the same schema and
pass the same test suite. Full-text search on Postgres uses the `pg_trgm`
extension, which the migration creates (it is a trusted extension: the
database owner may create it). SQLite suits a single host and small to medium
teams; Postgres is for larger deployments and standard backup/replication.

A SQLite database serves one `amail serve` process: a second one on the same
file stops with an error (it holds a lock on `<db>.serve.lock`).

### Running several instances

Any number of `amail serve` processes may share one Postgres database; put
any HTTP load balancer in front of them (no stickiness needed). Event streams
(`/events`, conversation mirrors) reconnect to any instance on their own.

```sh
amail serve --db postgres://... --listen 10.0.0.5:8025 --advertise-url http://10.0.0.5:8025
amail serve --db postgres://... --listen 10.0.0.6:8025 --advertise-url http://10.0.0.6:8025
```

The database stays the only source of truth. What a single process used to
keep to itself is handled like this:

| state | with several instances |
|---|---|
| mailbox events (`/events`) and their replay buffer | every instance sends each event (org, seat, counter) to the others with Postgres `NOTIFY` on one channel, after the change is committed, batched; each instance feeds them into its own buffer. Event ids are per instance: a client that reconnects to another instance gets a snapshot of the counters instead of a replay. An instance that loses its listening connection (or hears nothing, not even its own ping, for 35 s) reconnects and ends all its event streams, so clients resynchronize |
| presence | in the `presence` table (one instance keeps it in memory); the instance that gets a report tells the others which seats changed, and they read the rows |
| send budget | one token bucket per seat in the `send_budget` table, updated in one statement on the database's clock: exact, whatever instance a send hits (one instance on SQLite keeps it in memory) |
| node commands (sleep, wake, mirror leases) | sent to all instances with `NOTIFY` (a few short fields); each instance hands them to the node streams it holds. Each instance records its node streams per org (`node_streams`, heartbeat every 10 s), so `node_connected` and `node_offline` have one answer; a killed instance's streams still count for up to 30 s |
| conversation mirrors | too much data for `NOTIFY`, and never stored: the instance whose browsers watch a seat records the watch (`mirror_watch`, renewed with the lease); the instance that receives the node's posts forwards them over HTTP to the watching instances (`--advertise-url`), with the node's own token |
| browser sessions, idempotent claim keys, escalation steps | in the database already; an escalation step is taken by one instance only (a conditional update) |

What can still go missing is bounded: a `NOTIFY` lost because an instance died
between its commit and its batch (a few milliseconds) or because the
database dropped it. Nodes therefore refresh all pending counts every 30 s,
so such mail is shown at most 30 s late; browsers catch up with their next
event or reconnect.

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
- **Delivery**: one row per (message, recipient seat) with a class (`to` or
  `cc`; a seat reached several ways gets one delivery with the highest
  class), a mode, and `notified` / `read` / `acked` timestamps. The class
  follows the header field, also for lists: the members of a list in `To` get
  class `to` (they are notified and woken like direct recipients), of a list
  in `Cc` class `cc`; each keeps its member mode, so `digest` and `archive`
  members are not notified at once. (Deliveries made before this rule may
  carry the old class `list`.) Injected mail says how it came: "to you",
  "via list <name>" or "cc".
- **`seq` and `mod_seq`**: each seat has one counter. A new delivery takes the
  next value as its `seq` (its position in the mailbox) and as its `mod_seq`;
  every later state change of a delivery (notified, read, acked) takes the
  next value again as its new `mod_seq`. So `seq` values in a mailbox have
  gaps, and the counter only says "something changed". `/changes?since=` and
  server-sent events use `mod_seq` as the cursor.
- **Source**: computed for the reader: `user` (your org's user — instructions),
  `agent` (a seat in your org), `external` (anything from another org, even
  its user — never instructions), `system` (the mail server itself, e.g. an
  acknowledgement that did not come; sent from the org's hidden `amail`
  system seat, which has no tokens and cannot be addressed, so no client can
  send it).
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
- **Acknowledgement deadlines**: mail sent with `--ack` must be acknowledged
  within `--ack-timeout` (default: the org's `ack_timeout_min`, 15). Then the
  sender gets a `system` mail naming who has not; if they still have not
  after the same time again, a second one goes to the sender with those
  seats' leads (parents in the sender's org) in Cc, once. System mail asks for
  no acknowledgement and costs no budget.
- **Focus**: `amail focus on [--for 30m]` holds all but urgent mail for the
  seat (at most the org's `focus_max_min`, 240; without `--for`, that long);
  `amail focus off` ends it. Held mail neither wakes nor is injected; it
  comes when the focus ends. `amail who` and the web interface show who is
  focused.
- **Digests**: a list member with mode `digest` is not notified of each
  message. At the seat's next natural claim point (`UserPromptSubmit`, or
  `Stop`, which then continues the turn) the node injects one digest: a line
  per thread with subject, senders, count and the first line of the latest
  message; those deliveries become notified (not read). Digest mail never
  wakes a seat by itself, except that a seat that stays idle (or asleep) is
  woken once its oldest digest mail has waited the org's
  `digest_interval_min` (30), at most once per interval.
- **Send receipts** name recipients of your org that will not see the mail at
  once: asleep (and whether this mail starts them), offline, focused (until
  when), or not answering wake-ups.

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
| `amail serve --db <url> --listen <addr> [--advertise-url <url>]` | run the server (`--advertise-url`: see "Running several instances") |
| `amail admin create-org <org> [--user-name owner] --db <url>` | create an org directly in the database; prints the user token |
| `amail send --to a,b [--cc c] -s <subject> [-m <body>] [--urgent] [--ack [--ack-timeout 30m]] [--meta k=v]...` | body from stdin when `-m` is omitted |
| `amail focus on [--for 30m] [--seat s]` / `amail focus off` | hold all but urgent mail for a while (user tokens: any seat) |
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
| `amail node` | run the per-host node (see below; `--web-terminal-port` for browser terminals); `amail node token [--save]`, `amail node status`, `amail node web-token` |
| `amail run <seat> [-- args]` | start the seat's harness session in this terminal |
| `amail up [-f amail.toml] [--only g,s] [--no-start] [--prune]` | bring a team up from a team file (see "Teams") |
| `amail down [--only g,s]` / `amail ps` | stop the team's sessions / one line per seat |
| `amail attach <group or seat>` / `amail sleep <seat>` / `amail wake <seat>` | attach to a group's zellij session / put a seat to sleep / start it now |
| `amail org show/set` | org settings (budgets and limits, `--ack-timeout-min`, `--focus-max-min`, `--digest-interval-min`, `--webhook-url`, `--no-webhook`) |

## HTTP API

JSON under `/v1`; errors are `{"error":{"code":"…","message":"…"}}`.
Requests authenticate with `Authorization: Bearer <token>` or the web
interface's session cookie; with the cookie, everything but `GET` needs the
header `X-Amail-Csrf: 1`, and an `Origin` header must name the server.

| | |
|---|---|
| `POST /messages` | send `{to, cc, subject, body, in_reply_to?, priority?, ack_required?, ack_timeout_s?, meta?}`; the answer lists each address with its seat count (and for a list in `To`, `wakes`: its members notified at once) and `notes` on recipients that will not see it at once |
| `GET /inbox` | `seat`, `unread`, `after_seq`, `before_seq`, `limit`; headers only |
| `GET /messages/{id}` | full message; marks the caller's delivery read unless `peek=1` |
| `POST /messages/{id}/ack` | acknowledge |
| `GET /messages/{id}/status` | per-recipient state (sender and the sender org's user only) |
| `GET /threads/{id}` | messages of a thread the caller may see |
| `POST /claim` | `{classes, min_priority?, via, limit?, inline_body_max?, include_digest?, digest_only?, mark_inlined_read?, key?, preview_chars?}`: atomically mark pending deliveries notified and return them; concurrent claims never return the same delivery. With `mark_inlined_read`, deliveries whose body is returned inline are marked read too. `key` (chosen by the caller) makes the claim idempotent: a claim with a key the seat used in the last ten minutes takes nothing new and returns what the first one took, also when both run at once |
| `PUT /focus`, `DELETE /focus` | `seat=` (user tokens; seat tokens: their own): focus `{for_s?, until?}` (default and maximum: the org's `focus_max_min`), or end it |
| `GET /changes` | `since` (seq): deliveries created or changed since |
| `GET /events` | server-sent events: `mailbox` `{"seat","seq"}` per mailbox change (supports `Last-Event-ID`), `presence`, and for node tokens `command` (`{op: "transcript"\|"sleep"\|"wake", seat, lease_ms?, backfill?, node?}`; nodes ignore unknown events, ops and seats they do not host) |
| `GET, POST /seats`; `GET, PATCH, DELETE /seats/{name}`; `POST /seats/{name}/tokens` | seats |
| `GET, POST /lists`; `GET, DELETE /lists/{name}`; `PUT, DELETE /lists/{name}/members/{address}` | lists |
| `GET, POST /links`; `POST /links/{org}/accept`; `DELETE /links/{org}` | links |
| `GET /archive/messages` | `since`, `until`, `from`, `to` (a recipient seat), `thread`, `before_id`, `limit` (user tokens) |
| `GET, PATCH /org`; `GET, POST /tokens`; `DELETE /tokens/{id}`; `GET /whoami` | org settings (including `webhook_url`; `null` clears it), tokens, caller identity |
| `GET /pending` | node and user tokens: per seat, pending (unnotified, immediate) deliveries by class, urgent count, pending digest deliveries (`digest`, `digest_since`), `focus_until`, and the seat's counter; `digest_interval_ms` of the org |
| `POST /presence` | node tokens: `{node?, seats: [{seat, state, wake_failed?, harness?, since?, attention?, attention_hint?, terminal_url?}]}`; states `offline`, `busy`, `idle`, `awaiting`, `dormant`; `attention` tags a seat that needs a person (`trust-prompt`, `login`, `permission`, `failed`, `wake-failed`). Valid for 90 s (an expired entry produces no event: treat `updated_at` older than 90 s as offline); kept in memory by a single server and in the database by several; shown as `presence` on seats (with `node_connected`) and as `presence` events on `/events` |
| `GET /seats/{name}/launch` | node and user tokens: harness, model, role, parent, permission mode and a new seat token, for starting a session; the token of the seat's previous launch is revoked |
| `POST /session`; `DELETE /session` | sign a browser in with `{token}` (user tokens only; needs `X-Amail-Csrf: 1`) / out |
| `GET /overview` | user tokens: agent seats per state, seats needing a person, messages in the last hour and day, the user's unread count, the size of the attention queue |
| `GET /counts` | user tokens: per seat (or `seat=`), `unread` (archive-mode deliveries excluded) and `pending` (not yet shown to the agent) |
| `GET /attention` | user tokens: what waits for the user (below) |
| `GET /sent` | `seat`, `before_id`, `limit`: messages a seat sent, newest first (user tokens: any seat; seat tokens: their own) |
| `POST /messages/{id}/read` | mark the caller's delivery read |
| `GET /archive/search` | `q`, `before_id`, `limit` (user tokens): full-text search (below) |
| `POST /seats/{name}/sleep`, `POST /seats/{name}/wake` | user tokens: ask the node hosting the seat; 202, or 409 `node_offline` when no node of the org is connected |
| `GET /seats/{name}/transcript` | user tokens: the seat's conversation as server-sent events: `status` (`waiting`, `streaming`, `node_offline`, `no_session`, `no_file`, `unknown`), `reset`, `events` |
| `POST /seats/{name}/transcript` | node tokens: `{node?, reset, status?, events}` for the watchers of the seat |

**What waits for the user** (`/attention`, the web interface's "needs you"
queue), urgent mail first, then seats, then other mail, each oldest first:

- `ack`: a message to the user that asks for an acknowledgement the user has
  not given (until acknowledged).
- `reply`: an unread message that reached the user through `To` (directly,
  or through a list in `To` of which the user is an immediate member; not
  `Cc`) from someone else, in a thread where the user has sent nothing since.
  It leaves the queue when the user replies in the thread or marks it read.
- `seat`: a seat whose node reports that it needs a person, or whose
  wake-ups go unanswered.

**Search** (`/archive/search`): `q` is split on whitespace and every term
must occur in the subject or the body, as a case-insensitive substring, in
any script (Chinese included). Terms of three characters or more use the
index (SQLite: FTS5 with the trigram tokenizer; Postgres: a `pg_trgm` GIN
index). If any term is shorter, the search scans only the 2,000 most recent
messages before `before_id` and says so (`"mode": "scan"`, `scan_limit`);
`next_before_id` continues with older mail. Results are the messages the org
can see (its own and those delivered to it), newest first, with a snippet.

**Webhook**: with `webhook_url` set (`amail org set` or `PATCH /org`), every
new message delivered to the org's user is announced by a `POST` of
`{"event":"message","org","seat","message":<header>}` (never the body); up
to three attempts, in the background, never delaying the send.

## How delivery works

Every delivery moves through `queued` → `notified` → `read` → `acked`:

- **notified**: the recipient's agent has been shown the message (its header,
  and its body when that is short), or the recipient claimed it with
  `amail claim`.
- **read**: the whole message reached the recipient: through `amail read`, or
  because the body was short enough (800 characters) to be injected in full.
- **acked**: the recipient confirmed it with `amail ack`.

There is one way mail gets into a running agent: a harness hook asks the node,
and the node **claims** the pending deliveries on the server. A claim is
atomic, so a delivery is shown once even when several hooks race.

- **Busy agent**: the hooks that fire during a turn inject new mail into it:
  `UserPromptSubmit` (all mail), `PostToolUse` (only `to` and urgent mail,
  between tool calls) and `Stop` (keeps the turn going to handle the mail).
- **Idle agent**: the node sends a one-line nudge that starts a new turn; that
  turn's `UserPromptSubmit` hook claims and injects the mail. A nudge carries no
  mail itself. Only `to` (also through a list in `To`) and urgent mail
  nudges; `cc` mail waits for the next turn.

Injected text starts with `[amail]`, has one header line per message, and puts
every body line behind `  > `. Subjects and `meta` are single-line, so mail
content cannot pass itself off as a header or as a line from the mail system.

## Running agents: the node

A seat's session is a Claude Code, Codex or DeepSeek Harness (`dsh`) process
on some host. Each host
runs one `amail node`, which connects those sessions to the server:

- **Busy agent**: harness hooks call `amail hook <harness>`, a tiny client that
  forwards the hook input to the node over a Unix socket and prints the answer.
  When a seat has pending mail the node claims it and injects it: on
  `UserPromptSubmit` all classes, on `PostToolUse` only `to` and urgent mail,
  and on `Stop` it keeps the turn going (at most three times in a row). A seat
  with nothing pending is answered from memory, without a server call (about
  2 ms end to end, p99, with 1,000 seats). The hook client never fails: if the
  node is down or slow (300 ms, `AMAIL_HOOK_TIMEOUT_MS`), it prints nothing.
  One hook claims at most a few messages (8 for Claude Code and dsh, 5 for Codex) and
  shows each one's header, with the body when it has at most 800 characters;
  a longer message is marked `body not shown (N lines): run amail read <id>`
  and stays unread until the agent reads it. The rest follows with the next
  hook.
- **Idle agent**: when an idle seat gets `to` or urgent mail, the node wakes the
  session with a one-line nudge (`[amail] You have new mail (wake #n).`) after a
  3 s coalescing window (urgent: at once). The nudge starts a turn whose
  `UserPromptSubmit` hook injects the mail, so mail reaches the model through
  hooks only, and the server's atomic claim means each delivery is shown once.
  `cc` mail never wakes anyone; it comes along with the next turn. Mail to a
  list in `To` wakes every member, like mail to each of them; `amail send`
  says how many (`backend=3 (list in To: wakes 3)`), and the mail
  instructions tell agents to put a list in `Cc` for information that needs
  no action.
  Claude Code is woken through its inbox socket; Codex with `codex queue`;
  dsh through its web host's API (`session/prompt`).
  An unanswered wake-up is retried once after 10 s, then the seat is reported
  as `wake_failed` and tried again after 1 minute, doubling up to 10 minutes,
  or at once when new mail arrives for it.
- **Terminal paste (last resort, off by default).** A supervised seat with
  `paste_fallback = true` in the team file (per seat, or in `[defaults]`)
  whose two native wake-ups went unanswered gets the nudge typed into its
  zellij pane, then Enter, and the turn's `UserPromptSubmit` must confirm it
  as usual. Typing into a terminal is risky (an open dialog drops the text
  and takes Enter as its default answer; a half-written draft would be sent
  with the nudge), so the node first reads the pane: it types only if the
  seat is idle, the bottom of the screen shows no dialog, menu or busy
  marker, and the input box is empty; and it presses Enter only if the box
  then holds exactly the nudge. Otherwise nothing is typed and the seat is
  marked `wake_failed`.
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
  `unattended` seats run with `-a never` under the home's `amail` permissions
  profile: the workspace sandbox (the working directory, `/tmp` and
  `$TMPDIR` writable, everything else read-only), network access, and write
  access to the git directory and the repository's common git directory of
  every Codex seat's working directory, so a seat in a git worktree can
  commit, rebase and create branches (the workspace sandbox alone keeps
  `.git` read-only, and a worktree's git directory lies outside it).

- **DeepSeek Harness** (`dsh`, `harness = "dsh"`; the launcher is found as
  `dsh`, or set `amail node --dsh-bin` / `AMAIL_DSH_BIN`): dsh has no terminal
  interface; a person uses a session through the browser GUI of a `dsh web`
  host, and amail reaches the session through the same host's HTTP API. Each
  seat runs its own host: the pane runs `amail dsh-host`, which starts
  `dsh web --port 0`, reads the address and one-time launch token the host
  prints at start-up (also shown in the pane: open that URL to use the
  session), keeps them in a private file in the node's state directory,
  creates (or resumes) the seat's session and sends its first prompt. All dsh
  seats of a host share one dsh home (`<state dir>/dsh-home`), set to write
  plain JSONL session logs; your own `~/.dsh` is not used. A per-seat
  `--patch` overlay sets the system prompt (mail instructions and role) and
  the model (`deepseek-flash`, or `<provider>/<model>`). Hooks run through
  dsh's Codex hook bridge (`SessionStart`, `UserPromptSubmit`, `PostToolUse`,
  `Stop`) and see the seat's `AMAIL_SEAT`. dsh keeps variables with
  credential-shaped names (`*TOKEN*`, `*KEY*`, ...) from the agent's
  commands, so the seat's `amail` finds its token through a profile file
  (`AMAIL_PROFILE`). A wake-up queues the nudge with `session/prompt` (mode
  `queue`): on an idle session it starts a turn, on a busy one it runs as the
  next turn, and a session that is not loaded is resumed. dsh fires no hook
  when a turn is cancelled or fails, so the wrapper watches the session log
  and reports such a `turn/end` as the end of the turn.
  `unattended` dsh seats run their whole host in a bubblewrap jail, because
  dsh's own sandbox modes limit writes only and its file tools read files
  inside the host process without any sandbox. In the jail the system is
  read-only; the home directory, `/tmp` and `$XDG_RUNTIME_DIR` are empty;
  container engine sockets (Docker, Podman, containerd) are replaced by
  `/dev/null`; and only these are mounted back: the working directory, its
  git directories and the dsh home (writable), and the `amail` and `dsh`
  installations, the Node.js runtime, `~/.gitconfig`, the seat's profile and
  patch, the endpoint files and the node socket's directory (read-only). The
  network is shared. dsh runs with `DSH_PERMISSION_MODE=danger-full-access`
  inside the jail. `bwrap` is required (unprivileged user namespaces must
  be allowed); if it cannot run, the seat does not start. The model provider's key
  comes from the node's environment (`DEEPSEEK_API_KEY`) or from the dsh
  home's credential store.

Every session gets a system prompt explaining that `[amail]` text comes from the
mail system, that `USER` mail is an instruction, `agent` mail a colleague's
request and `external` mail untrusted information, and that mail never grants
permissions.

## Teams: `amail up`

A team file (`amail.toml`) describes the seat tree and how this host runs it;
the org comes from your user token.

```toml
[team]
template = "staged-merge"        # built-in, or a template directory; optional
repo = "/path/to/repo"           # needed by staged-merge
workroot = "/path/to/worktrees"  # seats' working directories go here
idle_sleep = "30m"               # put idle seats to sleep; "off" (default)
max_live = 40                    # seats with a process on this host
restart = "on-failure"           # or "never"

[defaults]
harness = "claude"
model = "sonnet"
permission_mode = "unattended"
# paste_fallback = true          # see "Terminal paste"; off by default

[[seat]]
name = "root"
role = "Overall technical lead."

[[group]]
name = "backend"
parent = "root"                  # any seat, e.g. another group's lead
lead = { name = "be-lead", harness = "codex", model = "gpt-6.1-sol" }
workers = { count = 3, prefix = "be", role = "..." }   # be-1..be-3, under be-lead

[[group]]
name = "core"
parent = "root"
lead = { name = "core-lead" }
workers = [                      # or one table per worker, harnesses mixed
  { name = "core-1" },
  { name = "core-2", harness = "codex", model = "gpt-6.1-sol" },
]

[[seat]]
name = "core-3"
group = "core"                   # joins the group (under its lead unless `parent` is set)

[[seat]]
name = "docs"
parent = "root"
cwd = "docs"                     # relative to the team file
role_file = "roles/docs.md"
```

`amail up` (with a user token, and a running node) checks the file (errors
name the entry; nothing is changed), prepares working directories, creates or
updates the seats (tree, harness, model, role, group) and the generated lists,
and asks the node to start the sessions. It is idempotent: run it again after
editing the file and only the differences are applied. Changed seats keep
running (the output names those that need a restart); seats that left the
file are reported and deleted only with `--prune`. Changed `[team]`
settings (`idle_sleep`, `max_live`, `restart`) apply at once and are
reported. Generated lists: one per
group (lead and workers), `leads` (every seat with subordinates) and `all`
(every seat; only the user may post). They are marked `managed`; lists you
made yourself are never touched. `--only` starts just some groups or seats;
`--no-start` starts nothing.

**Templates** decide working directories and role texts (which go into every
session's system prompt, so they are short):

- `staged-merge`: every seat gets a git worktree `workroot/<seat>` of `repo`
  on branch `amail/<seat>`; seats with subordinates also get an integration
  branch `amail/<seat>-next` (the root integrates into the main branch, in
  the main checkout). Role texts say who the seat's maintainer and
  subordinates are, how to submit (a worker rebases its branch onto its
  maintainer's integration branch, tests, then mails the maintainer with
  `--meta repo= branch= commit=`), and how to handle merge requests: merge
  into the integration branch, never rebase it (catch up by merging the
  parent's branch into it), reply "merged as <commit>". Maintainers forward
  requirement and interface changes to every affected member of their team
  and wait for confirmation.
- no template: the working directory is `cwd` (or `workroot/<seat>`), the
  role is `role` or `role_file`, plus who is above and below the seat.
- a directory with `root.md`, `lead.md`, `worker.md` and optionally
  `template.toml` (`workdir = "worktree"` or `"none"`); placeholders
  `{seat}`, `{parent}`, `{subordinates}`, `{group}`, `{workdir}`, `{branch}`.
  Templates are text only.

**Sessions.** The node starts and supervises them in zellij: one session per
group, `amail-<org>-<group>` (seats without a group in `amail-<org>-main`),
one tab per seat, each running `amail run <seat>`. A pane keeps its last
output when its command ends. The node keeps a headless zellij client of
fixed size (200x60) attached to each session, so panes are 200 columns wide
while nobody is attached (a session without any client lays its panes out at
about 50 columns). `amail attach <group or seat>` attaches your terminal,
focused on the seat. Your zellij configuration and sessions are not touched.

**Lifecycle.** Seat states: `starting`, `busy`, `idle`, `awaiting` (a
permission prompt), `dormant` (asleep), `offline`, `restarting`, `failed`.

- A seat idle for `idle_sleep` with nothing pending, or the least recently
  active idle seats when more than `max_live` run, or `amail sleep`, is put
  to sleep: its process ends, its session id and pane are kept.
- `to` or urgent mail for a sleeping seat (or `amail wake`) starts it again
  in its pane, resuming its session (Claude Code `--resume`, Codex `codex
  resume`, dsh `session/create` with the session id); the mail then arrives
  as usual. At most 4 sessions start at a
  time (`amail node --start-limit`), so mail to everyone does not start
  dozens of processes at once.
- A process that ends without `down` or sleep is restarted (resuming) after
  2 s, doubling per consecutive crash; after 3 restarts the seat is `failed`.
  A clean exit (status 0, e.g. `/exit`) is not a crash: the seat goes
  offline. A seat whose session cannot be resumed starts a new session that
  is told its earlier context is gone.
- `amail ps` flags seats that need a person, with what to do: Claude Code's
  directory-trust prompt, a harness that is not logged in, a permission
  prompt, a failed or unanswered seat.

**Claude Code's directory-trust prompt** appears for a new repository even
with `--dangerously-skip-permissions`, and its default answer is "No, exit".
amail does not answer it. Trust is recorded per repository (for a git
worktree, the main repository's path) in Claude Code's own configuration, so
answering it once (`amail attach <seat>`) covers every worktree of that
repository. Seats stuck at the prompt do not hold up other starts and are
flagged in `amail ps`; the flag clears once the prompt is gone. Claude Code
checks trust only at start, so after answering once, run `amail wake <seat>`
(or `amail up`) to restart the other seats stuck at the prompt.

## Known limitations

- **Lost answers.** If a hook's answer cannot be delivered (the hook client
  timed out or went away), the node keeps the claimed mail and shows it with
  the seat's next injecting hook, or wakes the idle seat for it. A claim whose
  own answer from the server is lost (a timeout, a dropped connection) is
  retried in the background with the same idempotency key, so the node gets
  exactly the mail the first attempt took and keeps it the same way. One
  window remains: the node process dies after the server answered a claim
  and before the mail reached the hook. Such mail is marked notified without
  having been shown; it stays unread, so it appears in the next session's
  unread summary and in `amail inbox`.
- Timeouts nest: the node's server calls for one hook (150 ms in total) <
  the hook client's wait (300 ms) < the harness's hook timeout (3-5 s).
- With several server instances: mailbox events that one instance had
  committed but not yet sent to the others when it died reach nodes on other
  instances with the next 30 s resync; a killed instance's node streams count
  as connected for up to 30 s; conversation mirrors across instances need
  `--advertise-url`.
- One session per seat: if two sessions use the same seat, the most recent
  hook wins.
- After a node restart, sessions whose process and wake target still exist
  are restored as idle; anything else waits for its next hook.
- Claude Code asks once per repository whether to trust it; amail does not
  answer for you (see "Teams").
- Codex permissions are per host, not per seat: every Codex session on a
  host may write the git directories of all Codex seats' repositories on that
  host. The dedicated Codex home may lie deep in the state directory: when
  its control socket's path is longer than a Unix socket path may be (107
  bytes), the node reaches it through `/proc/self/fd` (Linux).
- State reflects hooks: Claude Code refuses a long foreground `sleep`, runs it
  in the background and ends its turn, so a seat can show `idle` while a
  background task it started still runs.
- Mail reaches a busy agent only between tool calls or at the end of a turn;
  a long-running command delays it, and `urgent` cannot interrupt.
- Codex agents need network access in their sandbox to reach the server
  (the dedicated Codex home allows it): with networking disabled, the Codex
  sandbox refuses both TCP and Unix-socket connections.
- DeepSeek Harness:
  - The pane of a dsh seat shows the host's log, not a conversation: the
    session is used in a browser, at the URL printed in the pane (it carries
    the host's launch token).
  - Only the session amail created for the seat is connected to mail; other
    sessions a person opens in the same host's GUI (and their hooks) are
    ignored.
  - The hook bridge has no permission event: with a seat that is not
    `unattended`, an approval waiting in the browser is not reported as
    `awaiting`. Its `Stop` input has no `stop_hook_active`; the node's own
    limit (three blocks in a row) prevents loops.
  - The jail of an `unattended` seat hides the home directory, but the rest
    of the file system stays readable (read-only), and the dsh home is shared
    by all dsh seats of the host (each can read the others' session logs).
    A seat that is not `unattended` has no jail: it asks for approvals in the
    browser, and its file tools can read any file the user can read.
  - A turn cut off by a crash of the host is not continued after the
    restart; mail it had already read is not delivered again.
  - Each seat runs a host process (about 200 MB idle).
- Any process of the same user can ask the node for the credentials of a seat
  it hosts. The node is a per-user daemon; its socket directory is private to
  that user.

## Development

```sh
cargo test                                   # SQLite; Postgres tests are skipped

docker run -d --name amail-pg -e POSTGRES_PASSWORD=amail -p 127.0.0.1:55432:5432 postgres:17
AMAIL_TEST_PG_URL=postgres://postgres:amail@127.0.0.1:55432/postgres cargo test
```

The web interface (`web/`, Node 20 or later):

```sh
cd web
npm ci
npm run check && npm run lint && npm test     # types, lint, unit tests
npm run build && (cd .. && cargo build -p amail-cli)
PLAYWRIGHT_BROWSERS_PATH=.e2e/browsers npx playwright install chromium
PLAYWRIGHT_BROWSERS_PATH=.e2e/browsers npm run e2e
```

The end-to-end suite starts a real `amail serve` and `amail node` (a stand-in
script replaces the harnesses; no model is called), drives the interface in
headless Chromium and writes screenshots of every page, in both themes and
languages, to `web/.e2e/screenshots` (`AMAIL_SCREENSHOTS` overrides it).
`npm run dev` serves the interface with a proxy to a server at
`AMAIL_DEV_URL` (default `http://127.0.0.1:18025`).

The store conformance tests, the HTTP tests, the end-to-end CLI test and the
node tests all run against both backends (each Postgres test creates its own
database). The node tests drive the real `amail hook` binary with minimal
hook inputs shaped like the harnesses' own (`crates/amail-cli/tests/fixtures/hooks`); no model is called. The
hook latency bound is checked with `cargo test --release --test hotpath`.
The team tests (`crates/amail-cli/tests/team.rs`) run the real `amail up`,
`down`, `ps`, `sleep` and `wake` against a node whose "panes" are plain
processes running a fake harness (`tests/common/fake_harness.sh`) that speaks
the hook protocol; they need neither zellij nor a harness (only `git`).
`tests/cluster.rs` runs two real `amail serve` processes on one Postgres
database (and checks that SQLite refuses a second one); `tests/faults.rs`
kills the server, the node or one of two instances, cuts the node off, or
makes the database unavailable while mail is on its way, and checks that
every message is injected exactly once once the fault is over.

Crates:

| crate | |
|---|---|
| `amail-proto` | wire types and address parsing |
| `amail-store` | the `Store` trait, SQLite and Postgres backends, migrations, conformance tests |
| `amail-server` | the axum HTTP API, server-sent events, policy and rate limits |
| `amail-node` | the node daemon, the hook client, the Claude Code and Codex adapters |
| `amail-cli` | the `amail` binary |
| `amail-bench` | load generator: `amail-bench --url http://127.0.0.1:8025 --db <same db> --rates 170,500`; `--mode e2e` adds one real node per org with simulated sessions (`--url a,b` for several instances) |

## License

Apache-2.0
