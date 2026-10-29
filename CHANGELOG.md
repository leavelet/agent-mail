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
- Tasks, M8.1 (migration `0025_tasks_results.sql`):
  - Every event of a task is kept (the compaction at 1,000 events and
    `events_compacted` of `TaskDetail` are gone; the storage quota bounds
    the events). A change of the description records the whole old and
    new text. `amail task history --all` reads every page.
  - Revisions: `rev` in every task, `Rev:` in `task show`. The writes to
    one task take `if_rev` (`--if-rev N`) and are refused with 409 `stale`
    and `{rev, task, since}` when the task changed; the command prints the
    changes since then and the task.
  - `POST /tasks` takes `client_key`: the same key within 24 hours answers
    the existing task (200, `existing`). `amail task new` and the MCP
    `task_new` send a key derived from the reporter, title, body,
    assignee and parent; `task new --new` sends none.
  - Child tasks: `progress` (`{done, total}`) on parents, shown as `· 3/5`
    and `Children: 3/5 done`. Closing a parent with unfinished children
    is refused with 409 `children_open` unless forced (`--force`). The
    parent's assignee gets the reminder `children_done` when the last
    child closes. Parents nest up to 8 levels.
  - Results of done tasks: `result` (`ok`, `rejected`), `result_ref`,
    `result_note`, `result_by`, `result_at`; `task done --result`,
    `task result`, `task result withdraw` (event `result_withdrawn`);
    reopening clears the result; `task list --result`.
  - Dependencies that need an ok result (`task dep --needs-ok`,
    `--any-close`): only a `done` blocker with `ok` satisfies them, for
    `ready`, `unblocked`, `waiting` and `owed`. Reminder `blocker_rejected`
    when such a blocker is rejected or its ok result is withdrawn.
  - `amail task import <file> [--dry-run] [--map]` (`POST /tasks/import`,
    user token): tasks from a JSON Lines file, matched by their `id` on
    later imports; history entries become `imported` events; nothing is
    mailed. `task show <id>` finds an imported task.
  - `amail task report` (`GET /tasks/report`): the lead's report in
    sections with counts.
  - MCP: `task_result`, `task_report`, `if_rev`, the result fields and
    `force` on `task_set`, `needs_ok` on `task_dep`, `client_key` on
    `task_new`. The agent instructions mention results, child tasks and
    `--if-rev`.
- Work stealing: a seat asks to take over a task assigned to another seat
  that is not started (`/v1/tasks/{num}/steal`); the assignee is told by
  system mail and answers with `yield` (the requester gets the task) or
  `keep`. One pending request per task; it ends with the answer, with a
  change of the task's assignee or state, or after 24 hours, and the
  requester is told. Migration `0024_task_steal.sql`.
- Documents, M9 (server and API; the command line and MCP tools are
  below; node injection and web pages follow): an org's knowledge base under
  `/v1/docs…` (migration `0026_docs.sql`). A document has a number per org
  (`d12`), an optional slug, a kind, a title, a one-line description, a
  Markdown body kept whole per revision, a status (`draft`, `active`,
  `superseded`, `archived`), an owner, a team, labels, editors and a
  verification date with an expiry. Five built-in kinds per org
  (`decision`, `finding`, `runbook`, `index`, `reference`) with required
  sections; the user adds kinds. Bodies are parsed on write into sections
  with anchors (`d12#rollback`), read and written one section at a time,
  and into links (documents, tasks, messages, threads, seats, repository
  paths) with backlinks and dangling-link checks. A body is at most
  512 KiB, with advice to split above an estimated 40,000 tokens; `split`,
  `extract` and `merge` run in one transaction and rewrite the links to
  moved sections in other documents and in task bodies. Writes take
  `if_rev` and answer 409 `stale` with the changes since and a diff; equal
  writes create no revision; `client_key` as for tasks. An active
  `decision` is frozen. Search by section (trigram, with Chinese text),
  `find` by title, the per-turn index cut to the caller's budget, and
  similar existing documents listed on create without refusing it.
  Comments are mail in the document's thread (`[d12] <title>`); `[d12]` in
  a subject and `d12` in a task title link the thread or task. The owner
  is mailed about revisions by others, new documents that supplement or
  supersede theirs, expiry (once) and dangling links (once a day).
  `doc` events on `/v1/events`. A team can require the user to activate
  every document (`doc_activate_by` on `PUT /v1/teams/{name}`). Documents
  count against the storage quota (`doc_bytes`).
- Reminders: the cap of 200 reminder mails per org and hour, and the
  notice to the user when it dropped some, are removed. The background
  pass sends each seat one mail listing everything due for it (tasks and
  documents together); changes still send one mail per recipient.
- Seat handover, M9 (server side; migration `0027_handover.sql`):
  - The handover note: one live note per agent seat with every revision
    kept (`GET /seats/{name}/handover[?rev=N]`, `PUT
    /seats/{name}/handover`). Seven fields in order (`State`, `Summary`,
    `Next`, `Waiting on`, `Traps`, `Verify`, `Pointers`), written as text
    (Markdown headings or `Field:` lines) or JSON; a note lacking one of
    the first six is refused with 422 `note_fields_missing` (with
    `missing`), one over 16,384 characters with 422 `note_too_large` (with
    `chars`). An empty `Pointers` is filled in from the worklog. Notes are
    readable by every seat of the org; a `private` note only by the seat,
    its lead (the team's root seat) and the user. The user may write a
    seat's note; node tokens file compaction summaries as drafts
    (`source: compaction`).
  - `POST /seats/{name}/handover/request` (the lead, the user, or a node
    in the name of the system seat) records a request and mails the seat;
    `POST /seats/{name}/handover/read` (node tokens) records which session
    was shown the note. `GET`/`PUT /seats/{name}/handover/private` (the
    user) is the seat's setting from the team file's `handover =
    "private"`.
  - When a task closes and its assignee holds no other unfinished task,
    the server writes the seat's note itself (`State: idle`).
  - The worklog: `POST /worklog` (node tokens) appends entries for the
    org's agent seats; the server adds entries for mail sent, read and
    acknowledged, task changes, focus and note revisions.
    `GET /seats/{name}/worklog` (the seat, its team, its lead, the user,
    node tokens)
    filters by `since`, `task`, `kind` and pages with `before`;
    `summary=1` gives counts per kind, the tasks touched and the last ten
    entries. Entries hold at most 1,000 characters of text.
  - Notes and worklog entries count against the storage quota in
    `task_bytes`; a worklog entry that does not fit deletes the org's
    oldest entries. Worklog retention (`--worklog-days`) and the entries
    per request (`--max-worklog-batch`) are limits of public servers
    (defaults 90 days and 5,000).
- Documents and seat handover on the command line, M9: `amail doc new`,
  `show`, `read`, `write`, `set`, `supersede`, `verify`, `split`,
  `extract`, `merge`, `list`, `search`, `find`, `index`, `history`,
  `diff`, `backlinks`, `check`, `comment`, `kinds` and `kind add` (user
  token); `amail handover show`, `write`, `template` and `request`; and
  `amail worklog [seat] [--summary]`. A document is `d12`, `12` or a slug;
  `d12#anchor` names a section to read, write, extract or comment on.
  `doc show` prints the record, relations, links in and out (dangling and
  stale marked), linked tasks and threads, comments, the last 20 events
  and the first 2,000 characters of the body. `doc new` sends a
  `client_key` derived from the caller, kind, title and description
  (`--new` sends none) and prints similar existing documents as a
  warning. Writes take `--if-rev`; a 409 `stale` prints the revisions and
  events since and the diff. `split`, `extract` and `merge` print where
  each section went (`#rollback -> d31`). `handover write` reads the note
  from `-m`, a file or stdin; a note lacking fields prints the missing
  labels and points to `amail handover template`.
- Requests for the person, M10 phase A (server side; migration
  `0028_requests.sql`):
  - One record per thing that waits for the org's user, numbered per org
    (`r12`): kind (`question`, `plan`, `merge`, `attention`; `permission`
    is in the record but not created yet), the seat it is about, an
    optional task, title, Markdown body, 2 to 6 options or free text, a
    default, a class (`now`, `soon`, `later`) that sets the deadline
    (15 minutes, 4 hours, 24 hours), the state, the answer, the mail
    thread, the source, a revision and an append-only history.
  - `POST /requests` (seats ask for themselves; the user names a seat;
    nodes make `attention` requests), `GET /requests` (open ones first,
    `now` first, oldest first; filters by state, kind, seat, class and
    task), `GET /requests/{num}`, `POST /requests/{num}/answer` (user
    tokens; by key, number, label or text; `if_rev`; 409 `answered` with
    the first answer), `POST /requests/{num}/withdraw` and
    `GET /requests/{num}/wait` (a long poll of at most 600 s).
  - A new request is mailed to the user, `To`, subject `[r12] <title>`,
    with the options as numbered lines; class `now` is urgent. The answer
    is mailed in the request's thread `To` the seat, which wakes it. A
    reply of the user in the thread whose first line names an option is
    the answer; any other reply is a comment that reaches the seat.
    `[r12]` in a subject links the thread to the request.
  - The deadline pass joins the reminder pass: a request with a default
    takes it at the deadline (`expired`, answered by `deadline`, mailed to
    the seat); one without a default is reminded to the user by class, in
    the user's one reminder mail of the pass with task and document
    reminders.
  - Presence reports keep `attention` requests in step: one per seat and
    condition (`attention`, a failed turn, a failed wake-up) while it
    lasts, withdrawn by `node` when the node reports it gone. Answering
    one sends the node an `attention_action` command (`restart`, `sleep`)
    or gives the seat's terminal link (`open_terminal`).
  - `GET /attention` lists the open requests first (`kind: "request"`),
    then the entries as before; `request` events on `/v1/events` for user
    tokens.
  - A task with an open request shows `blocked_on_human`; `take` and
    `steal` refuse it with 409 `blocked_on_human`, and `GET /tasks/next`
    leaves it out.
  - Requests count against the storage quota in `request_bytes` (shown in
    `GET /org`).
- Web control of seats, M10 phase A (server side):
  `POST /seats/{name}/prompt {text}` and `POST /seats/{name}/interrupt`
  (user tokens) record a worklog entry (`prompt_from_user`, `interrupt`;
  a prompt also as system mail kept in the archive, notifying nobody) and
  send the seat's node a `prompt` or `interrupt` command; they answer
  `{queued: true, seat, nodes}`, 409 `no_node` when no connected node
  reports the seat, and `interrupt` 409 `no_structured_interrupt` for a
  Claude Code seat. Node commands gain the fields `text`, `request` and
  `action`.
- Requests and seat controls, M10 phase B (command line and MCP):
  `amail ask` (options as `--option key:label[:description]`, `--default`,
  `--class`, `--deadline 2h`, `--task`, `--kind`; `--wait` long-polls
  until the answer or the deadline and prints it), `amail answer r12
  <option|text> [-m note] [--if-rev N]`, `amail withdraw r12 [-m note]`
  (`amail withdraw <id> --from <seat>` still withdraws a message),
  `amail requests` (open `now` first, with age, default and deadline;
  filters by state, kind, seat, class and task), `amail request show r12`
  (the options numbered as in the mail, the answer, the events, the
  threads), `amail prompt <seat>` and `amail interrupt <seat>` (the 409s
  in plain words). Task lines and `task show` print `blocked-on-human
  (r12)`; `task take` and `task steal` of such a task print the server's
  refusal. MCP tools `ask`, `requests`, `answer`, `prompt_seat` and
  `interrupt_seat` (the last three for the user's seat); the MCP
  instructions tell seats to use `ask` for the person's decisions and to
  stop until the answer comes as mail.
- Requests for the person and web control of seats, M10 phase C (node
  side; migration `0029_presence_waiting.sql`):
  - The node carries out `prompt` (Claude Code: a line on the inbox
    socket; Codex: `codex queue` after the daemon check, also while a turn
    runs; dsh: `session/prompt`; a supervised seat that is asleep is
    started first), `interrupt` (Codex: `turn/interrupt` on the turn in
    progress; dsh: `session/cancel`; Claude Code: ignored) and
    `attention_action` (`restart` stops and starts the seat's session,
    `sleep` as `amail sleep`). A prompt is not a wake-up. Each is recorded
    in the seat's worklog (`prompt`, `interrupt`, `attention_action`).
  - A seat whose turn ran `amail ask` (or the `ask` tool) and ended with
    its `question`, `plan` or `merge` request still open and no mail
    waiting is marked as waiting for it: presence `waiting_for`, `waiting
    for r12` in `amail ps`, worklog `waiting`. It sleeps by the team's
    idle rule; the answer's mail wakes it, and its next turn clears the
    mark.
  - `GET /requests` takes node tokens for one seat (`seat` required).
  - The mail instructions tell seats to use `amail ask` for the person's
    decisions, then stop.

#### Running agents

- `amail mcp`: an MCP server on stdio in the same binary, with the mail
  commands (`inbox`, `read`, `send`, `reply`, `ack`, `thread`, `search`,
  `who`, `status`, `focus`) and the task operations (`task_new`,
  `task_show`, `task_list`, `task_mine`, `task_next`, `task_waiting`,
  `task_owed`, `task_board`, `task_set`, `task_take`, `task_dep`,
  `task_undep`, `task_link`, `task_unlink`, `task_comment`,
  `task_history`, `task_steal`, `task_yield`, `task_keep`), the document
  tools (`doc_new`, `doc_show`, `doc_read`, `doc_write`, `doc_set`,
  `doc_search`, `doc_find`, `doc_list`, `doc_index`, `doc_history`,
  `doc_split`, `doc_extract`, `doc_verify`, `doc_comment`),
  `handover_show`, `handover_write` and `worklog`, and the request tools
  (`ask`, `requests`, `answer`, `prompt_seat`, `interrupt_seat`) as typed
  tools. Each answers the server's JSON with the
  command's text rendering; API errors are tool errors with the server's
  code and message. The node registers it for the sessions it launches
  (Claude Code: `--mcp-config` and an allow rule for its tools; Codex:
  `[mcp_servers.amail]` in the node's Codex home, the seat found from the
  thread id of each call). The command line stays the documented
  interface. The text of the document, handover and worklog tools is cut
  at 25,000 characters with a line that says what to narrow, and
  `doc_read` of a whole document above 20,000 characters answers its
  section list unless `whole` is true. The server's instructions tell
  the agent to search documents before deciding how something is done
  and to record findings as documents rather than mail.
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
  checks before it changes anything. A seat without `cwd` runs in the
  directory the session was started in. The adopted session gets the seat
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
  finds the node through them: it takes the pass from the node's state
  file and the socket from the command, after checking that it runs in that
  node's Codex home and that the state file and the socket's peer belong to
  this user. A daemon whose environment has the pass uses it as before. A
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
- A wake-up whose turn finds nothing pending any more (its mail was shown
  by an earlier turn, or read in the meantime) is answered with one line
  that says so, and the agent ends the turn. Before `codex queue`, the
  node asks the Codex daemon about the thread and adds no nudge while a
  turn runs or a nudge is already queued (a nudge queued behind a running
  turn would start a turn of its own after it).
- Failed turns: mail shown in a harness turn that failed (Claude Code
  `StopFailure`, a dsh turn that ended with an error, a Codex turn the
  app-server reports as failed, or a session process that ended during the
  turn) goes back to "not read" and is delivered again, up to three failed
  turns in a row; the seat is flagged as `turn failed` in `amail ps`, the
  web interface and the user's queue, and the user gets one system mail.

- Documents and seat handover on the node, M9:
  - Claude Code and Codex seats register `PreCompact` and `PostCompact`
    hooks.
  - Session start shows the seat's handover note before the mail (up to
    two thirds of the injection budget; `State`, `Summary`, `Next`,
    `Waiting on` and `Verify` whole, then as much of `Traps` and
    `Pointers` as fits; a warning line past 7 days) and records the read,
    and the team's document index under `Documents:` after the task line
    (up to a third of the budget). A turn's start shows the index again
    when it changed. Mail always comes first: the node keeps the index in
    a cache it refreshes in the background, a hook waits for the index or
    the note only at session start and only for a part of its time, and
    what misses comes with the next hook.
  - The end of a turn asks once for the handover note when the note is
    older than the turn and the context passed `--handover-threshold`
    (70%) of the model's window (read from the transcript; windows from
    `--context-window`, the harness or a table), the seat closed a task
    and still holds work, or a request is open. Seats idle with work in
    progress for the team's `handover_idle` (30 minutes) are asked by a
    request mail. `PostCompact` files the harness's summary as a draft
    note.
  - Worklog entries from hooks: session start and end, compaction, turn
    start and end (with the failure), tool calls (name and 200 characters
    of input), and commits in the seat's working directory.
  - The mail instructions describe documents and the index; the seat
    introduction says what to keep when the context is compacted.
  - Team files: `docs.activate_by` (`[team]` and `[[group]]`),
    `handover_idle`, and `handover = "private"` per seat; `amail up`
    applies them. `amail ps` has a NOTE column.
- Documents and seat handover, fixes from the M9 acceptance run:
  - The node finds `amail task` (and the amail task tools) in the whole
    tool input, also after `cd`, `&&` or `;` and beyond the 200
    characters kept for the worklog, so a closing command such as `cd ...;
    git commit ...; amail send ...; amail task done t3` asks for the note.
  - `amail ps` NOTE is right from the start: the node fetches the note's
    state when it connects, when a seat is added and after the seat runs
    `amail handover write`; after a restart it reads the last turn's start
    from the worklog.
  - After `/compact` in Claude Code (`SessionStart` before `PostCompact`)
    the session is shown the compaction draft; the note before it is not
    shown or recorded as read.
  - A new Codex session's start is recorded in the worklog (and its note
    shown) at the prompt that registers it.
  - The node stops refreshing pending counts the server refuses (the user
    seat, 403) until it reconnects.
  - The worklog records an agent seat's document events (`doc`, with the
    document's number): create, revision, status and field changes,
    supersession, verification, split, extract, merge and comment.
  - `doc set --owner X --silent` no longer mails the new owner; an owner's
    comment on a document nobody else commented on mails nobody.
  - A status change by someone other than the owner (activating a draft)
    is mailed to the owner unless `silent`; the lead or user who asked for
    a handover note is mailed when the seat writes it.
  - The server's idle note replaces only a note older than the task's
    closing and never an agent's note from the turn that closed the task
    or later; replacing an agent's earlier note, it keeps its `Traps`.
  - A section write keeps the document's spacing (no blank line added
    after the heading).
  - Search snippets show headings without `{#id}` and do not end inside a
    code span.
  - `amail thread`, the MCP `thread` tool and the web thread page list the
    documents a thread cites and mark comments on removed sections
    (`doc_comments` in `GET /v1/threads/{id}`).
  - The web Needs you page at phone width no longer draws the reader over
    the drafts and their Activate button.
  - The agent instructions say concretely that a verified fact about the
    code, a tool or the environment is a `finding` document with its
    evidence and that a comment corrects or discusses an existing
    document.

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
- Inside a Codex session, `amail` commands never fall back to a saved
  login (the person's token) when the node does not answer the thread's
  credentials request: the command fails and says why. An imported task is
  not reminded as stale before one stale period after the import. The
  `blocker_rejected` reminder reads "is done with result rejected". Board
  chains and reminder mail mark dependencies that need an ok result; the
  `unblocked` history line shows the event's note; the web history shows
  the direction of a dependency; the agent instructions say to reopen and
  re-result a rejected task instead of creating a new one.
- Tasks, M8.1: cards and table rows show the progress of child tasks and
  a `rejected` badge; the table filters by result; the task page has a
  result block that sets or withdraws the result, marks dependencies that
  need an ok result, closes a parent with open children only when asked
  to, and sends the revision with title and description edits. The Tasks
  page has a Report view.
- Documents and seat handover, M9: a **Documents** section with a page per
  team (pinned documents, kinds with counts, stale and draft counts), lists
  per kind with filters, section search, and a viewer that renders the
  Markdown body (marked and DOMPurify; no script, event handler or
  `javascript:` link survives, images are not shown) with the server's
  section anchors, `d12`/`t3` references as links, the fields, relations,
  links in and out with dangling and stale marks, linked tasks and
  threads, the discussion with section anchors, revisions with a diff, and
  the history. The user verifies, activates, archives, supersedes, pins,
  edits fields and the body (with `if_rev`; a stale write shows the diff
  and can be sent again), comments on a section, and splits, extracts and
  merges. The create form shows similar titles while typing; a Kinds page
  lists and adds kinds. Seat pages have **Handover** and **Worklog** tabs;
  "needs you" lists `decision` drafts to activate and the user's stale
  documents. All of it follows `doc` events live.
- Inbox selection: each Inbox row has a check box (`x` ticks the row the
  keyboard is on), a header check box selects every loaded row, and a bar
  above the list marks the selected messages read or unread or clears the
  selection. **Mark all read** marks every unread message of the Inbox
  read and asks first when there are more than 20. Both use the new
  `POST /v1/inbox/read` (user tokens), which marks the user's deliveries
  of a list of message ids, or all of them (below a `before_seq`), read in
  one transaction and answers how many changed.
- Requests for the person and seat control in the web interface (M10
  phase D):
  - "Needs you" shows the open requests as cards grouped by class (`now`,
    `soon`, `later`), oldest first, above the other entries: seat, task,
    title, the body (rendered Markdown, collapsed), age, deadline and
    default, the options as buttons with the default marked, a free-text
    field, an optional note, **Snooze 1 h** (this browser only), and for
    `attention` requests the system's actions (`open_terminal` opens the
    terminal link the answer returns). Answers send `if_rev`; a card whose
    request was answered first shows that answer. A class whose open
    requests all have a default can be answered with the defaults after
    one confirmation. A request's creation mail is not listed again below
    its card and is marked read when the request is answered here; entry
    kinds the page does not know are left out.
  - A request page (`#/requests/r12`) with the fields, options, answer,
    history, threads, the answer form and **Withdraw**, and a requests
    list (`#/requests`) with filters for state, kind, class and seat.
  - Mail whose subject names a request (`[r12]`) shows it above the
    message in the Inbox and on the thread page, with its options as
    buttons that answer through the API, or its answer.
  - Tasks with an open request show `blocked-on-human (r12)` on the task
    page, the board and the table.
  - Seat pages: a prompt box under the conversation mirror, **Interrupt**
    for Codex and dsh seats, the terminal link with an explanation for
    Claude Code seats, the seat's open requests, and "waiting for r12"
    when the node reports it.
  - `request` events refresh the cards, the pages, the lists and the
    navigation count live.

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
- Documents have two classes of limits: format and model-protection rules
  (body 512 KiB, field lengths, the index budget) everywhere, and resource
  rules (documents, indexed sections and links, search hits, kinds per
  org, how many documents the index and the candidates look at) in public
  mode only; on a self-hosted server the storage quota is the only bound.
- Requests for the person have the same two classes: format rules (title
  512 characters, body 64 KiB, 2 to 6 options with keys of 32 characters
  and labels of 200, answers 16,384 characters, waits of at most 600 s,
  prompts of 64 KiB) everywhere, and resource rules (10,000 open requests
  per org, 600 requests per seat per hour) in public mode only.
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
