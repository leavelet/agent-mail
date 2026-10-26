# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
uses [Semantic Versioning](https://semver.org/). Before 1.0, minor versions
may change the HTTP API and the team file format.

## [0.1.0] - Unreleased

The first public version.

### Added

#### Mail server and command line

- One binary, `amail`: the server (`amail serve`), administration
  (`amail admin`) and the command-line client for agents and people.
- Storage in SQLite (default, zero setup) or Postgres; both backends share
  one schema and pass one test suite. Migrations run on start.
- Seats (positions with durable mailboxes), lists with post policies and
  per-member delivery modes (`immediate`, `digest`, `archive`), threads,
  and visible per-recipient delivery states (queued, notified, read,
  acked).
- One org per user; orgs are isolated until both users agree to a link.
  Mail from another org is always marked as external, never as an
  instruction.
- Other orgs see an org's card and only what it opens to them. Every org,
  seat, team and list has a description (500 characters); an org has a
  display name and a visibility (`listed-by-name`, or `hidden`: to orgs
  without an active link it answers as a name no org has). Openings
  (`amail open`, `amail close`, `amail openings`, the web interface's
  Openings, or `open = ...` in a team file) show a seat or team to every
  linked org or to one org and let it start threads there; a new org opens
  only its user seat. Only the user changes openings; each change is
  reported by system mail with the orgs that gain or lose sight of which
  cards, and the web interface shows it before it is applied.
- Every message between two orgs reaches a contact of each: recipients in
  the other org must be its contacts or participants of the thread
  (otherwise `unreachable`, answered as for a name that does not exist);
  when an org has no contact on a message, the server adds in Cc its
  contacts already in the thread, or, for the sender's org on a first
  message, its primary contact (`amail link set <org> --primary <seat>`,
  per org or per team). Every address on such a message is visible to
  everyone on it. Lists cannot be addressed on mail to other orgs and hold
  seats of their own org only. A thread that lost every contact of one
  org takes no new message. The user is told by system mail when a seat
  that is not a contact joins a thread with another org. `amail send
  --dry-run` and `POST /v1/messages/preview` show the added contact before
  sending.
- What an org knows of a thread is only the messages its own seats sent or
  received (participants, the reply check and the contacts added come from
  them). A delivery status shows another org's recipients' state and read
  and acknowledgement times only. The link and openings a cross-org send
  depends on are checked again in its transaction. New threads to other
  orgs are capped by the server (per seat, for the user seat, and per
  receiving seat). Link-request notices quote the other org's text; the
  "Openings changed" notice says what made the change.
- Teams: several team trees per org (`amail team`, the web interface's
  Teams view), created by team files (`[team] name`, `[[group]]`, nested
  as written) with descriptions, contacts and openings; several team files
  can be up in one org.
- Another org's page (`amail org show <org>`, `amail who --org <org>`, the
  web interface's Other orgs): its card, the link and since when, the cards
  it opens to you, what you open to it and your primary contact for it.
  Link requests carry a note for the other user. A budget of new threads to
  other orgs per seat (`new_threads_per_hour`, 10 by default) and an
  `outbound` setting (`any-seat` or `contacts-only`).
- Attention protection: per-seat send budgets, a recipient cap, list post
  policies, a reply-depth limit, focus mode, digests, and acknowledgement
  deadlines that escalate to the sender (and then to the laggards' leads)
  with system mail.
- Search in any script (Chinese included), an archive with
  filters, and an optional notification webhook (headers only).
- Several server instances can share one Postgres database.

- TLS to Postgres: the binary is built with TLS support (rustls), so a
  database URL with `sslmode=require`, `verify-ca` or `verify-full`
  connects (it could not before), and the default `prefer` uses TLS when
  the database offers it. `sslrootcert` names a private certificate
  authority; see [operations](docs/operations.md#tls-to-the-database).

- The subject limit is 4,096 characters (was 300), counted as Unicode
  scalar values. Injected header lines, one-line listings (`amail inbox`,
  `claim`), system mail and list rows of the web interface show the first
  200 characters and `…` (digests 120); `amail read`, `amail thread`,
  `--json`, the API and the web message view keep the whole subject.
  `amail reply` and the web reply keep `Re: ` subjects within the limit.

- `amail send --ack --ack-timeout 0` (or `off`; `ack_timeout_s: 0` in the
  API) asks for an acknowledgement without any reminder by system mail.

- The org's user can write a message in the name of one of the org's
  agent seats (`POST /v1/seats/{name}/messages`). It is the seat's mail
  (source `agent`, the seat's budgets and reachability) and records that
  the user wrote it: injected mail, digests, `amail inbox`, `read`,
  `thread` and `status`, `--json` (`written_by`) and the web interface
  show `written by USER` (or the user's address in other orgs). Seat and
  node tokens cannot do this (migration 0020).

- The sender of a message (or the org's user) can withdraw a delivery to
  a seat of the org that has not been shown, read or acknowledged:
  `amail withdraw <id> --from <seat>`, Withdraw in the web delivery table,
  `POST /v1/messages/{id}/deliveries/{seat}/withdraw`. The recipient no
  longer has it as unread or pending mail and is not woken or reminded;
  it can still read it by id and in its threads. Status shows
  `withdrawn` (migration 0021).
- Reading or acknowledging a message counts as having been notified of
  it. Before, a message the agent read on its own (`amail read` after
  `amail inbox`, before a hook or wake had shown its header) kept
  `notified_at` empty, so it stayed in the seat's pending mail, was
  claimed and injected again at the next hook, and the web interface
  showed its delivery as queued. `notified_via` is `read` or `ack` for
  such deliveries; migration 0022 repairs existing rows.
- Tasks: one shared task board per org (`/v1/tasks…`). A task has a
  number per org (`t12`), a title, description, state (`open`, `doing`,
  `blocked`, `review`, `done`, `cancelled`; an `open` task whose blockers
  are all closed is shown as `ready`), priority and rank, reporter, one
  assignee, team, parent, labels and due time. Dependencies between tasks
  (cycles refused), `take` for unassigned tasks, linked threads, a
  discussion thread made of mail (`[t12] <title>`), and a history with one
  event per change. The personal queries `mine`, `next`, `waiting` and
  `owed`, a filtered list and the lead's board. Reminders are system mail
  to the seats concerned (assigned, no longer blocked, waited for, done or
  cancelled, now urgent, stale, past due), never to the seat that made the
  change. A `[t12]` in a mail subject links the thread to the task. Thread
  answers list the linked tasks; `task` events on `/v1/events` follow
  changes. Tasks are bounded per org and per task and count against the
  storage quota. Migration `0023_tasks.sql`.
- `amail task`: `new` (also `--from <message>`), `show`, `history`,
  `list` with the API's filters, `mine`, `next [--pool]`, `waiting`,
  `owed`, `board`, `set`, `assign`, `take`, `steal`/`yield`/`keep`,
  `block [--on seat]`,
  `unblock`, `start`, `review`, `done`, `cancel`, `reopen`, `dep`/`undep`,
  `link`/`unlink` and `comment`; one line per task in text output and the
  server's JSON with `--json`. `amail thread` lists the tasks linked to
  the thread. The seat introduction in an agent's system prompt names the
  task commands, and at session start the node adds
  `[amail] you have N open task(s), M ready: amail task mine` when the
  seat has unfinished tasks (`GET /tasks/mine?seat=` with the node
  token; the only task call node tokens may make).
- Work stealing: a seat asks to take over a task assigned to another seat
  that is not started (`/v1/tasks/{num}/steal`); the assignee is told by
  system mail and answers with `yield` (the requester gets the task) or
  `keep`. One pending request per task; it ends with the answer, with a
  change of the task's assignee or state, or after 24 hours, and the
  requester is told. Migration `0024_task_steal.sql`.

#### Running agents

- `amail mcp`: an MCP server on stdio in the same binary, with the mail
  commands (`inbox`, `read`, `send`, `reply`, `ack`, `thread`, `search`,
  `who`, `status`, `focus`) and the task operations (`task_new`,
  `task_show`, `task_list`, `task_mine`, `task_next`, `task_waiting`,
  `task_owed`, `task_board`, `task_set`, `task_take`, `task_dep`,
  `task_undep`, `task_link`, `task_unlink`, `task_comment`,
  `task_history`, `task_steal`, `task_yield`, `task_keep`) as typed tools. Each answers the server's JSON with the
  command's text rendering; API errors are tool errors with the server's
  code and message. The node registers it for the sessions it launches
  (Claude Code: `--mcp-config` and an allow rule for its tools; Codex:
  `[mcp_servers.amail]` in the node's Codex home, the seat found from the
  thread id of each call). The command line stays the documented
  interface.
- The node keeps Codex's app-server daemon at the `codex` command's
  version: it turns the daemon package's self-update off
  (`app-server-daemon/settings.json`) and removes a package of another
  version before it starts the daemon. A session that Codex refuses to
  attach to its daemon ("Background server has incompatible feature
  settings") is reported by `amail ps` as `codex-daemon`.
- A Codex session's environment carries no seat token. Tool commands and
  the MCP server get the seat's credentials from the node by thread id; a
  daemon that a session starts or restarts no longer hands its seat's token
  to every thread.
- Seats can continue existing conversations: `session = "<id>"` on a seat
  in the team file, `amail run <seat> --session <id>` and `amail seat adopt
  <seat> <id> [--restart]` make the seat resume that Claude Code, Codex or
  dsh session. The node checks that the session exists on this host (the
  Claude Code transcript, the Codex rollout file in the node's Codex home,
  the dsh session directory) and that no other seat has it; `amail up`
  checks before it changes anything. The adopted session gets the seat
  intro on its first start (Claude Code: in the system prompt; Codex and
  dsh: in a first prompt). A failed resume falls back to a new session and
  shows `adopt-failed` in `amail ps`.
- The unread summary a hook adds at the start of a turn leaves out mail
  the session was shown before. Before, every `UserPromptSubmit` listed up
  to 20 unread headers, so a message that stayed unread (a body over 800
  characters the agent never ran `amail read` on) was listed again at every
  turn and taken for new mail. A new or resumed session still gets the
  whole list once at `SessionStart`.
- `amail node --codex-sandbox read-only|workspace-write|danger-full-access`
  (`AMAIL_CODEX_SANDBOX`) runs Codex seats under a native Codex sandbox
  policy instead of the node's `git_access` permission profiles; the node
  then changes no repository permission files.
- The Codex hook command carries the node's state directory and socket
  (`amail hook codex --node-state-dir … --node-socket …`). A hook started by
  a Codex daemon that does not have the node's environment (one started
  before the node, or by a person's own session in the same `CODEX_HOME`)
  finds the node through them, after checking that its parent is that
  home's app-server and that the socket's peer is this node binary. A
  failed check of the managed hooks' trust is now a launch error with its
  reason, instead of a warning.
- Codex seats whose turn history cannot be read (the app-server's thread
  store answered `database disk image is malformed`) no longer stay busy
  for good: the node proves a normal completion from the session's rollout
  file (a complete `task_started`/`task_complete` pair, no error, no later
  input, the thread idle with an empty queue) and only then marks the seat
  idle. After a node restart, a Codex session that is still running is held
  (`recovery-unverified` in `ps`, no automatic wake-ups) until a hook or
  that proof shows its last turn ended normally; before, it was assumed
  idle.
- A resumed session that exits with status 1 before its first hook is
  treated as a failed resume (and replaced by a new session) only when the
  pane shows Claude Code's `No conversation found with session ID`; a
  launcher or configuration error keeps the saved session.
- Presence hints are cut to their limit before they are sent, so a long
  turn-failure reason no longer makes the server refuse the whole presence
  update.
- A Codex seat's `effort` saved with its resume id (`codex_effort` in the
  node state) is applied again when the session is resumed or woken.
- `POST /messages/{id}/deliveries/{seat}/cancel` is accepted as an alias of
  `…/withdraw`.
- The time a hook waits for the mail server follows the measured latency
  (twice the slowest answer of the last five minutes, between 150 ms and
  1.5 s; `hook_budget_max` in the node configuration) instead of a fixed
  150 ms, and the hook client waits up to 2 s for the node instead of
  300 ms. With a server reached over a slow network every hook used to miss
  its budget, so mail arrived one hook late every time. `amail node status`
  shows the round trips, the budget and the hooks of the last ten minutes
  that missed it; when five or more (and 30%) missed, the user gets a
  system mail once an hour.


- `amail node`: a per-host daemon that injects mail into running Claude
  Code and Codex sessions through their hooks (between tool calls, or at
  the end of a turn) and wakes idle sessions; every delivery is shown
  exactly once, also across node and server restarts.
- `amail up`: turns a team file into seats, generated lists, git worktrees
  and supervised sessions in zellij; idle seats can sleep and are started
  again (resuming their session) by new mail. A built-in `staged-merge`
  workflow template: workers send merge requests to their lead, leads to
  the root.
- `amail ps`, `attach`, `sleep`, `wake`, `down`; a guarded terminal-paste
  wake-up as a last resort (off by default).
- Failed turns: mail shown in a harness turn that failed (Claude Code
  `StopFailure`, a dsh turn that ended with an error, a Codex turn the
  app-server reports as failed, or a session process that ended during the
  turn) goes back to "not read" and is delivered again, up to three failed
  turns in a row; the seat is flagged as `turn failed` in `amail ps`, the
  web interface and the user's queue, and the user gets one system mail.

#### Web interface

- A single-page app embedded in the binary: the org tree with live seat
  states, a "needs you" queue, inbox, threads with delivery states, the
  archive and search, composing mail, a read-only mirror of each seat's
  conversation, and administration of seats, lists, links, tokens and org
  settings. English and Chinese, light and dark.
- Browser notifications for new items in the "needs you" queue (opt-in in
  the settings).
- Tasks: a board per org with one column per shown state (`ready` among
  them) and a table with the list filters and paging. Each card has a
  priority control, and cards are dragged within their column (or moved
  with Alt+Up/Down) to set their rank; a card cannot be dropped in another
  column. A task page shows the record, dependencies, child tasks, linked
  threads with their latest message, the discussion thread and the
  history, and lets the user change the state (with the seat and reason
  for `blocked`), assignee, priority, due time and labels, add and remove
  dependencies, link and unlink threads, comment, and answer a request to
  take the task over. A seat's page lists its tasks and what others wait
  for from it; a thread lists its tasks under the subject; "needs you"
  lists tasks waiting for the user and the user's overdue tasks. All of it
  follows `task` events live.

#### Public servers and getting started

- Public mode (`amail serve --public`): registration with one-time invite
  codes, protective limits (seats, lists and tokens per org, message rates,
  event streams, request rates, link requests, registrations), reserved names,
  always-`Secure` session cookies, and webhooks refused for private and
  loopback addresses (checked when connecting).
- Fixed bounds on page sizes, thread answers (the newest 500 messages),
  replay buffers and conversation mirrors; see the security model.
- Forwarded headers (`X-Forwarded-For`, `-Host`, `-Proto`, `-Prefix`) are
  believed only from configured trusted proxies.
- Running under a URL path prefix such as `https://example.com/amail/`:
  `amail serve --base-path /amail` (`AMAIL_BASE_PATH`), or a reverse proxy
  that removes the prefix and sends `X-Forwarded-Prefix`. The web interface
  works under any prefix without a rebuild (the server writes the prefix
  into its page); the session cookie's `Path`, redirects and `/v1/info`
  (`base_path`) follow it; `/healthz` also answers at the root. The CLI and
  nodes take the URL with the prefix; `amail login` and `amail doctor` say
  when a URL seems to lack it. Tested configurations for nginx and Caddy
  are in the operations guide.
- A browser link to a web interface route given as a path (`/org/root`)
  is redirected to the route (`/#/org/root`).
- An operator role: statistics without mail content, suspending, resuming
  and deleting orgs, invite codes and resetting an org's lost user token
  (`amail admin`, and `/admin` in the web interface).
- Users can rotate their token and delete their org with all its data;
  an optional retention period clears old message bodies.
- `amail register`, `amail login` with saved profiles, `amail init` (a
  sample team file), `amail doctor` (checks with hints), and
  `amail node start` / `stop`.
- A Dockerfile, a Compose file with Postgres, and an install script for
  release binaries (Linux x86_64 and aarch64) that verifies checksums.
- `THIRD_PARTY_LICENSES.md` with the licenses of the bundled Rust crates
  and npm packages.

#### Platforms

- The node, `amail run` and `amail up` support Linux only for now. The
  server and the mail commands also build on macOS.

### Security

Changes made after a security review, before the first public server.

#### Mail content shown to people and agents

- `amail read`, `amail thread` and `amail claim` print header lines,
  including the source label, themselves and put every body line behind
  `  > `, as hook injection does, so a message body cannot pass for a
  header, a message from the user or a line of the mail system. The mail
  instructions given to agents describe this.
- Control characters, terminal escape sequences, bidirectional controls,
  zero-width and other invisible characters are shown as visible escapes
  in everything the command line prints and everything injected into a
  session (bodies, subjects, `meta`, names, descriptions, labels, role
  text). In `--json` output they are written as JSON `\u` escapes.
- Injected mail is packed by its rendered size; messages that do not fit
  wait for the next hook instead of being cut.

#### Credentials

- Revoking or rotating a token, or signing out, ends the event streams and
  conversation mirrors opened with it. Rotation ends every browser session
  of the old token (the browser that rotated gets a new one). A token has
  at most 20 browser sessions. Revoked tokens are deleted.
- The node gives a seat's credentials and mail only to that seat's own
  session, identified by the calling process and its parents (Claude Code)
  or by a value only the node's Codex sessions receive (Codex; checked
  under both local policies, so a thread id alone gets nothing). Requests
  meant for the person (`amail run`, `up`, `down`, `ps`, `attach`, `sleep`,
  `wake`, `node status`) are refused to agent sessions. The remaining
  limits, when agents run as the same operating-system user as the person,
  are described in the security model.
- Clients check the owner and mode of the node socket and its directory,
  and the user of the process serving it, before using it. Without
  `XDG_RUNTIME_DIR` the socket is in the node's state directory, not in
  `/tmp`.
- Node tokens cannot claim or read the user seat's mail. Seat tokens do
  not see the org's webhook URL.

#### Decisions on the user's machine

- The permission mode of a session is decided by the node's machine: the
  team file or `amail run --permission-mode`. The server's seat setting is
  shown but has no effect on launches; with nothing set locally, the
  harness keeps its permission prompts. A model name from the server that
  could be read as an option is ignored.
- The Codex permission profile keeps git hooks, git config and related
  files read-only while allowing commits, rebases and merges. Codex seats
  and `amail up` refuse a repository whose git directory was redirected.
- The node follows conversation transcripts only where the harness writes
  them, and bounds what it reads from the server and from its socket.

#### Bounds on a shared server

- Search and archive requests look only at the requesting org's own mail,
  at most 50,000 messages per request (continued with a cursor), with a
  query time limit and at most 4 such requests per org in progress
  (`--max-org-queries`). The trigram and full-text indexes are dropped.
- Thread answers carry at most 1 MiB of bodies (older bodies are fetched
  one at a time; the command line and the web interface do this). Search
  snippets are cut in the database. Listings, inbox, changes and attention
  pages are cut at 2 MiB and continue with their cursor. `GET /seats`
  shortens long roles. Claims load at most 16,384 characters of a body.
  Lists have at most 1,000 members.
- A per-org storage quota (`--max-org-storage-mb`, 1 GiB by default in
  public mode) counts the mail each org sent; a send over it is refused
  with 507 `storage_quota`.
- Conversation mirrors are bounded per post (1 MiB), per watcher (8 MiB),
  per seat (16 MiB) and per server instance (`--max-mirror-mb`, 256 MiB by
  default in public mode); a watcher that falls behind is disconnected and
  starts again from the replay buffer.
- Length limits on token labels, seat display names, roles, models and
  harnesses, list descriptions and presence fields. Org settings have
  upper bounds in public mode.
- Rate-limiter state is bounded; IPv6 clients are limited per /64.
- Webhooks: the server's own addresses and networks are refused, only
  ports 80 and 443 are allowed in public mode, and at most 16 notices per
  org are in flight.

#### Orgs

- The delivery status of mail sent to another org's list counts that
  list's seats instead of naming them; the archive's `to=` filter matches
  another org's seats only where the message named them.
- Registration checks the invite code before the org name, and link
  requests are answered the same way whether or not the org exists.
- Deleting an org no longer removes the mail other orgs received from it;
  that mail stays with them, shown as from a deleted org.
- A seat's terminal link is kept only when it is an `http` or `https` URL.

#### Releases

- GitHub Actions are pinned by commit, release archives carry build
  provenance attestations, and `install.sh` downloads over https only and
  accepts a separately obtained checksum (`AMAIL_SHA256`).

#### Second review

- Pending and unread counts come from per-seat counters kept with each
  delivery change, not from counting deliveries. A thread the caller cannot
  see is refused before anything is loaded. Requests whose cost depends on
  the data (search, archive, threads, attention, overview, delivery status,
  lists, changes, large inbox and sent pages) take one of the org's query
  slots (1 per org and 8 per server in public mode, `--max-org-queries`,
  `--max-queries`), and every org has a request limit over all its tokens
  (500 per second in public mode, `--max-org-requests-per-sec`); a
  request over it is refused before the database is asked, and the 429
  answer is sent after one second. On SQLite the writer's checkpoint waits
  at most 100 ms for readers. `amail-bench tenants` measures one org's
  latency while another org loads the server.
- The storage quota also counts a fixed amount per message and per
  delivery and the address lists; an org at its quota cannot send at all.
  Mail kept after the sending org was deleted is charged to the orgs that
  keep it. Existing counts are recomputed on upgrade.
- Codex seats have a per-seat `git_access` setting (`worktree` by default,
  `full`, `off`). With `worktree` the root of the repository's git
  directory stays read-only; `packed-refs` is moved into `.git/amail/` so
  that commits, rebases and merges in a linked worktree still work; branch
  and tag deletion need the person or a `full` seat. The staged-merge
  template gives a Codex root seat `full`, and `amail up` says so. Codex
  seats and `amail up` refuse git directories that contain symbolic links
  in `refs/` or `logs/`.
- Every default-ignorable code point (variation selectors included) is
  shown as an escape in the command line and in injected mail, and the web
  interface shows invisible and control characters as visible escapes.
- A full rate-limiter key table no longer refuses new clients;
  registrations are also limited per IPv6 /48. An invite code is spent
  after three answers that a name is taken. Link requests that are not
  accepted within 30 days expire, to unknown names and to existing orgs
  alike.
- Revocation ends only the streams of the revoked credential. Delivery
  status counts the seats of a third org behind another org's list.
  Webhook notices from other orgs share a separate in-flight limit.
  An empty `AMAIL_MAX_*` or `AMAIL_RETENTION_DAYS` value counts as not set.
  `install.sh` requires curl.
- The node has a local policy, `local_policy`: `open` (every process of
  the person's OS user, including agent sessions, may manage seats and
  obtain any seat's credentials through the node) or `restricted` (the
  earlier checks by parent process: a session gets only its own seat's
  credentials and cannot manage seats). It is set with `amail node
  --local-policy`, `AMAIL_LOCAL_POLICY` or `node.toml`; a team file can
  only choose `restricted`. The default is `open`. `amail up`, `amail ps`,
  `amail node status` and `amail doctor` print the policy in effect.
- The node tells the person by system mail, written by the server from
  fixed texts and sent only to the org's own user seat: when its local
  policy, or a supervised seat's permission mode or `git_access`, differs
  from what it reported last (at start and after `amail up`), and, under
  the open policy, when a process of an agent session manages seats,
  obtains a seat's credentials or sends a hook in another seat's name (at
  most one mail per session every five minutes). Recognizing a session's
  processes is best effort. Reports are limited per org.
- The supervisor waits for the multiplexer to mark a pane exited before
  running it again after a stop it asked for, so restarting a seat stuck
  at a prompt is no longer counted as a failed start; a screen check of an
  earlier launch no longer marks the new launch as stuck.

Changes users will notice in this review:

- An org at its storage quota cannot send at all, and deliveries count
  toward the quota (existing counts are recomputed on upgrade).
- Search, archive, thread, attention, overview, status, list, changes and
  large inbox and sent pages of one org run one at a time on a public
  server (429 `too_many_queries` beyond that); an org that sends more than 500
  requests per second gets 429 answers after one second.
- Codex seats cannot write the root of the repository's git directory
  unless their `git_access` is `full`; branch and tag deletion from a
  `worktree` seat fails; seats refuse to start while `refs/` or `logs/`
  contain a symbolic link.
- Invisible characters show as escapes in the command line, in injected
  mail and in the web interface.
- A registration with a valid invite and a taken name spends one of the
  invite's three tries; link requests expire after 30 days.
- The user's inbox gets a system mail when a node first starts or changes
  its local policy or a seat's permission mode or `git_access`, and,
  under the open policy, when agent sessions manage seats.

#### Third review

- Requests whose cost depends on the data take slots of three pools: one
  for search and archive, one for threads, delivery status, changes and
  large inbox and sent pages, and one for overview, attention and lists,
  which is limited per org only. A search or archive request starts no
  further step after 100 ms and answers with a cursor; a search step reads
  at most 4 MiB of message bodies, divided by the number of terms. A freed
  slot goes to the org holding the fewest slots of the pool, then to the
  org that held slots of the pool for the least time lately. So other
  orgs cannot keep an org's overview, attention or lists waiting, and its
  searches wait at most for the next freed slot. The web archive page
  repeats a search up to ten times to fill a page. `amail-bench tenants
  --phases hostile` measures twelve orgs searching their full mailboxes
  against another org's use of the web interface.
- Mail an org keeps from a deleted org is counted as `kept_bytes`, not
  against the org's storage quota, and retention deletes it once it is
  older than the retention period. The org's user can delete it and the
  org's internal mail: `DELETE /v1/threads/{id}` (`amail thread delete`,
  Delete on the web thread page) and `POST /v1/archive/purge-deleted-orgs`
  (`amail org purge-deleted-orgs`). Mail exchanged with orgs that still
  exist cannot be deleted by either side. Existing counts are moved on
  upgrade (migration 0016).
- A node notice report becomes at most one system mail of at most 4 KB.
  Notice mail is charged to nobody's storage quota; an org receives at
  most 50 notice mails and 128 KB of them in 24 hours, and notice mail is
  deleted after 7 days (migration 0015).
- After 10 failed authentications per minute (bursts of 30) from one
  client address (IPv6 per /64), requests from that address with a
  credential the server has not seen valid lately get 429 before the
  database is asked (`--max-auth-failures-per-min`).
- The dsh jail mounts only `~/.gitconfig` and `config`, `ignore` and
  `attributes` of git's configuration directory, one file at a time, so
  git's credential stores are not visible; it runs the host in a new
  session with `/dev/null` as input, so jailed processes have no
  controlling terminal.
- With the failed-turn fixes: a node's report of failing turns is one
  section of the report's single notice mail and counts against the
  org's daily notice budget like any other report; deleting a thread or
  purging deleted orgs' mail gives each seat that lost deliveries a
  `mailbox` event, also when the mail was shown in a turn that has not
  ended (releasing it after a failed turn then releases nothing); the
  failed-turn flag (migration 0017) comes after this review's migrations.
- A request body declared larger than 2 MiB is read and dropped (up to
  16 MiB, for at most 5 s) before the 413 answer, so that the client gets
  the answer instead of a reset connection.
- The node opens its event stream with an HTTP client of its own. With
  one client for everything, a request made just after the stream opened
  could, rarely, wait behind the stream until its timeout: the HTTP
  client can hand out a connection whose answer is still being read.

Changes users will notice in this review:

- A search or archive request of a large org answers sooner and more
  often with a cursor and few or no results; follow `next_before_id`.
- `GET /org` shows `kept_bytes`; mail of deleted orgs no longer blocks
  sending; users can delete mail of deleted orgs and their own internal
  threads.
- Several notices of one node report arrive as one mail, and notice mail
  is removed after 7 days.
- Many failed sign-ins or invalid tokens from one address lead to 429 for
  a while from that address.
- A jailed dsh seat no longer sees `~/.config/git` except `config`,
  `ignore` and `attributes`.
