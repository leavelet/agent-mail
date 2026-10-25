# Architecture

```
                 ┌──────────────────────────────┐
  browser ──────►│  amail serve                 │◄──── amail CLI (people, agents)
  (the user)     │  directory · mailboxes ·     │
                 │  lists · deliveries · archive│
                 │  presence · web interface    │
                 └──────────────▲───────────────┘
                                │ per host: one event stream (SSE) + HTTPS calls
                 ┌──────────────┴───────────────┐
                 │  amail node (one per host)   │
                 │  pending counts · claims ·   │
                 │  wake-ups · supervision ·    │
                 │  transcript relay · presence │
                 └───▲──────────▲───────────▲───┘
         Unix socket │          │           │ the harness's own interfaces
              ┌──────┴───┐ ┌────┴─────┐ ┌───┴───────┐
              │amail hook│ │Claude    │ │Codex      │
              │(run by   │ │inbox     │ │app-server │
              │ hooks)   │ │socket    │ │queue      │
              └──────────┘ └──────────┘ └───────────┘
```

## Components

**Server (`amail serve`).** The only source of truth: orgs, seats, lists,
links, messages, deliveries, tokens, settings. An axum HTTP API under `/v1`
with JSON bodies and server-sent event streams, plus the embedded web
interface, at the root or below a URL path prefix (`--base-path`, or
`X-Forwarded-Prefix` from a trusted proxy). Storage is SQLite (one process per database file) or Postgres
(any number of processes on one database). Migrations run on start.

**Node (`amail node`).** One per host and user. It connects outward to the
server (so it works behind NAT), keeps one event stream open for the whole
org, and serves the local hook clients over a Unix socket. It:

- keeps per-seat counts of pending mail in memory, refreshed from mailbox
  events (and fully every 30 seconds; a seat's count also when its session
  starts and when a turn ends with nothing to show, so that a lost event
  delays its mail only until then), so most hooks are answered without a
  server call;
- claims mail on the server when a hook can show it;
- wakes idle sessions through the harness's own interface;
- starts, supervises, puts to sleep and resumes sessions for `amail up`;
- reports each seat's state (`busy`, `idle`, `awaiting`, `dormant`,
  `offline`) and what needs a person;
- relays a seat's conversation to the server while someone watches it.

Its local policy decides what the processes of the person's user may ask
on its socket: under `open` every such process is served, and requests of
agent sessions that manage seats or obtain other seats' credentials are
reported to the person by system mail; under `restricted` a session gets
only its own seat's mail and credentials, and requests meant for the
person are refused to sessions (see
[security](security.md#trust-boundaries)).

**Hook client (`amail hook <harness>`).** The command the harnesses run on
their hook events. It forwards the hook's input to the node over the Unix
socket and prints the node's answer. It never fails the harness: if the node
is down or slow (2 s, `AMAIL_HOOK_TIMEOUT_MS`), it prints nothing.

**Adapters.** One per harness, inside the node: how to launch or resume a
session (flags, settings, system prompt with the mail instructions and the
seat's role), how to read its hook payloads, how to wake it, and how to read
its transcript file. See [harness support](harnesses.md).

**Supervision.** `amail up` sends a team's seats to the node, which runs every seat
in zellij (one session per group, one tab per seat, each running
`amail run <seat>`), restarts crashed sessions, puts idle ones to sleep and
resumes them when mail arrives. See [team files](teams.md).

## The path of one message

1. **Send.** `amail send --to be-1 ...` (or the web interface) posts to
   `/v1/messages`. The server checks the sender's token, budget, addresses,
   links and list policies, expands lists, and in one transaction stores the
   message and one delivery per recipient seat, each with the seat's next
   sequence number. In the same transaction each recipient seat's pending
   and unread counters grow, and the seats are recorded as taking part in
   the thread (claims, reads and acknowledgements change the counters
   through triggers in the database). It answers with the recipients
   and notes on who will not see it at once.
2. **Event.** After the commit, the server publishes a mailbox event
   (org, seat, counter) to the org's event streams. With several instances
   it also sends it to the other instances (Postgres `NOTIFY`).
3. **Node.** The node of the host running `be-1` receives the event and
   refreshes the seat's pending counts (events are coalesced for 30 ms).
   The server reads them from the seat's counters, not from its
   deliveries.
4. **Busy session.** If `be-1` is in a turn, its next hook asks the node.
   On `PostToolUse` (after each tool call) only `to` and urgent mail is
   taken; on `UserPromptSubmit` all classes; on `Stop` the node keeps the
   turn going so the agent handles the mail (at most three times in a row).
5. **Idle session.** If `be-1` is idle and the mail is `to` or urgent, the
   node waits a 3-second coalescing window (urgent: no wait) and sends a
   one-line nudge, `[amail] You have new mail (wake #n).`, through the
   harness (Claude Code's session inbox socket, `codex queue` for Codex,
   `session/prompt` on the web host's API for DeepSeek Harness).
   The nudge carries no mail: it starts a turn, and that turn's
   `UserPromptSubmit` hook does step 4. A sleeping seat is resumed first.
6. **Claim.** The node calls `/v1/claim` for the seat with the classes the
   hook may show. The server marks the pending deliveries `notified` in one
   statement and returns them (at most 8 for Claude Code and dsh, 5 for Codex per
   hook; the rest follow with the next hook). Concurrent claims never return
   the same delivery.
7. **Injection.** The hook's answer adds context to the model's turn:

   ```
   [amail] 1 new message(s) for be-1:
   #3 from be-lead (agent) · to you · "Storage schema" · thread 3
     > Please add the search index.
   Read: amail read <id> · Reply: amail reply <id> -m "..."
   ```

   One header line per message; every body line behind `  > `; a body over
   800 characters is replaced by `body not shown (N lines): run amail read
   <id>`. One answer holds as many whole messages as fit in the harness's
   limit for hook context (counted as rendered, prefixes and escapes
   included); the node keeps the others and shows them with the next hook. Subjects and `meta` are single-line, so mail content cannot pass
   itself off as a header or as a line from the mail system.
   Besides new mail, a `SessionStart` hook lists the seat's unread mail
   (headers only, at most 20, nothing claimed): what an earlier session
   was shown and did not read. A `UserPromptSubmit` hook lists only
   unread mail this session was not shown before, so a message that
   stays unread (a long body the agent never ran `amail read` on) is
   listed once per session, not at every turn.
8. **Reading and answering.** The agent runs `amail read`, `amail reply`
   or `amail ack` like any shell command; those calls mark the delivery
   `read` or `acked`, which the sender can see.

Timeouts nest so that nothing blocks a harness: the node's server calls for
one hook take at most the hook budget in total, the hook client waits 2 s,
and the harnesses allow their hooks several seconds.

The hook budget follows the server's measured latency: twice the slowest
answer of the last five minutes plus 50 ms, at least 150 ms and at most
1.5 s (`hook_server_timeout` and `hook_budget_max` in the node's
configuration). A fixed short budget made every hook miss a server reached
over a slow network, and mail then arrived one hook late every time; a long
fixed budget would slow every tool call of an agent with mail waiting. Only
hooks with mail to fetch spend the budget: the node's pending counts say
when there is none, and no server call is made. A claim whose answer misses
the budget is retried in the background with the same key, and what it took
is kept for the next hook. `amail node status` shows the measured round
trips, the budget, and the hooks of the last ten minutes that asked the
server with how many missed; when at least five hooks (and 30% of them) in
ten minutes missed, the node tells the user by system mail, at most once an
hour.

## What is guaranteed

- **A delivery is shown at most once.** Mail reaches the model only through
  hooks, and only after the server's atomic claim marked it `notified`.
  The idle path sends a nudge, never the mail itself.
- **Mail is not lost when parts fail.** The test suite kills the server,
  the node, or one of two server instances while mail is being delivered, cuts
  the node off the network, and makes the database briefly unavailable; in
  each case every message is injected exactly once after the fault, without
  intervention, on both storage backends. The mechanisms:
  - if a hook's answer cannot be delivered (the hook client timed out or
    went away), the node keeps the claimed mail and shows it with the
    seat's next injecting hook, or wakes the idle seat for it;
  - a claim carries an idempotency key chosen by the node; when the
    server's answer is lost (a timeout, a dropped connection), the node
    retries in the background with the same key and gets exactly the mail
    the first attempt took;
  - after reconnecting, the node fetches all pending counts again and
    re-reports presence; every 30 seconds it resynchronizes anyway.
- **Delivery states are visible**: the sender can always see who was
  notified, who read and who acknowledged.

## What is not guaranteed

- **One window remains**: if the node process dies after the server
  answered a claim and before the mail reached the hook, those deliveries
  are `notified` without having been shown. They stay unread, so they
  appear in the next session's unread summary and in `amail inbox`, but no
  hook injects them.
- **Timing.** A busy agent sees mail only between tool calls or at the end
  of a turn; a long tool call or a long generation delays it, and `urgent`
  cannot interrupt. An idle agent is woken after the coalescing window if
  the harness accepts the nudge (see [harness support](harnesses.md) for
  when it does not).
- **That agents act on mail.** Injection puts mail into the context; what
  the model does with it is up to the model and its instructions.
- **Seat state reflects hooks.** A seat whose agent started a background
  task and ended its turn shows `idle` while the task runs.
- With several instances, a mailbox event committed by an instance that
  died before passing it on reaches nodes on other instances with the next
  30-second resynchronization.

## Several server instances

Any number of `amail serve` processes may share one Postgres database
behind any HTTP load balancer (no stickiness). The database stays the only
source of truth; what one process would keep to itself is shared like this:

| state | with several instances |
|---|---|
| mailbox events and their replay buffer | each instance sends every event (org, seat, counter) to the others with `NOTIFY` after the commit, batched. Event ids are per instance: a client that reconnects to another instance gets a snapshot of the counters instead of a replay. An instance that loses its listening connection, or hears nothing (not even its own ping) for 35 s, reconnects and ends all its event streams so clients resynchronize |
| presence | in the database; the instance that gets a report tells the others which seats changed |
| send budgets | one token bucket per seat in the database, updated in one statement on the database's clock |
| node commands (sleep, wake, mirror leases) | sent to all instances with `NOTIFY`; each hands them to the node streams it holds |
| conversation mirrors | never stored: the instance whose browsers watch a seat records the watch; the instance that receives the node's posts forwards them over HTTP to it (`--advertise-url`) |
| browser sessions, claim keys, escalation steps | in the database; an escalation step is taken by one instance only |

The protective limits of public mode (rates, stream counts) are kept per
instance.

## Conversation mirrors

A seat's page in the web interface shows its live conversation, read-only.
The browser asks the server, the server asks (through the node's event
stream) the node that hosts the seat, and the node reads the session's
transcript file (Claude Code JSONL, a Codex rollout or a dsh session log), sends the last 200
entries and then follows the file. The node takes the file's path from the
session's hooks and accepts only paths where the harness writes
transcripts: `projects/` of the Claude Code configuration directory
(`CLAUDE_CONFIG_DIR` of the session or of the node, else `~/.claude`), or
`sessions/` of the node's Codex home, or `sessions/` of the node's dsh
home (dsh hooks report no path; the node derives it from the session's
directory and id). The node stops 30 seconds after the last
browser left. Mail injected by the system is shown apart from what a person
typed. The server keeps at most the last 1,000 entries (4 MiB) of a watched
seat in memory and nothing once nobody watches; only user tokens can watch.
Each watcher has a queue of at most 8 MiB; a watcher that stops reading is
disconnected when its queue is full. A seat holds at most 16 MiB, and all
mirrors of an instance at most the `--max-mirror-mb` limit (see
[security](security.md#resource-limits)).

## Code layout

| crate | |
|---|---|
| `amail-proto` | wire types and address parsing |
| `amail-store` | the `Store` trait, SQLite and Postgres backends, migrations, conformance tests |
| `amail-server` | the HTTP API, event streams, policy and limits, public mode |
| `amail-node` | the node, the hook client, the Claude Code, Codex and DeepSeek Harness adapters (and the dsh host wrapper), team supervision |
| `amail-cli` | the `amail` binary |
| `amail-bench` | load generator used for the capacity numbers in [operations](operations.md) |
| `web/` | the web interface (Svelte, TypeScript), embedded into the binary at build time |
