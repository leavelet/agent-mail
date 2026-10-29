# Reference

## Configuration of the command line

Mail commands find their server and token in this order:

1. `--profile <name or file>` (or `AMAIL_PROFILE`): the name of a saved
   login, or a TOML file with `url = "..."` and `token = "..."`;
   `AMAIL_URL` / `AMAIL_TOKEN` fill in what a file leaves out;
2. `AMAIL_TOKEN` with `AMAIL_URL` (default `http://127.0.0.1:8025`): how
   agents' sessions are set up;
3. inside a Codex session (`CODEX_THREAD_ID` and `AMAIL_CODEX_PASS`), the
   seat's credentials from the node; a saved login is never used there, so
   the command fails when the node does not answer or refuses;
4. the current saved login (`amail login`).

Saved logins are stored in `$AMAIL_CONFIG_DIR/config.toml`, else
`$XDG_CONFIG_HOME/amail/config.toml`, else `~/.config/amail/config.toml`.

Output is compact and line-oriented, so it costs agents few tokens. `--json`
on any command prints JSON instead; control and invisible characters in
values are written there as JSON `\u` escapes. Errors go to stderr with a non-zero
exit code.

## Commands

### Getting started

| command | |
|---|---|
| `amail login <url> [--name n]` | read a token (from stdin, or prompted without echo), check it, save it as a login and use it from now on (default name `<org>@<host>`) |
| `amail register <url> --invite <code> --org <name> [--user-name owner] [--name n \| --no-save]` | register an org on a public server and print the user token once. It saves the token as a login (default name `<org>@<host>`) and makes that login the current one, so later commands go to the new org; it prints the name of the previous current login (switch back with `amail profile use <name>`). `--no-save` saves nothing and keeps the current login |
| `amail profile ls` / `use <name>` / `rm <name>` | saved logins |
| `amail init [-f amail.toml] [--force]` | write a commented sample team file |
| `amail doctor [-f amail.toml]` | check the platform, server, token, node, harness and zellij versions, Codex socket path and sandbox, `bwrap` (for unattended dsh seats), Claude Code trust of this directory, and the team file; exits non-zero on a failure. When the node runs, it checks the programs the node starts (`--claude-bin`, `--codex-bin`, `--dsh-bin`, `--zellij-bin`), else the ones on its own `PATH`. A missing harness is a warning when the team file or a seat on the server uses it; a dsh seat with a node started without `--dsh-env-file` is a warning too |

### Mail

| command | |
|---|---|
| `amail send --to a,b [--cc c] -s <subject> [-m <body>] [--urgent] [--ack [--ack-timeout 30m]] [--meta k=v]... [--dry-run]` | send; the body comes from stdin when `-m` is omitted. `--ack-timeout 0` (or `off`) asks for the acknowledgement without any reminder. Mail to another org shows the contact of your org the server added in Cc, if any. `--dry-run` sends nothing and shows who would get it, the contact that would be added and what would be rejected |
| `amail inbox [--all] [--seat s]` | unread messages (all with `--all`) |
| `amail read <id> [--peek]` | show a message; marks it read unless `--peek` |
| `amail reply <id> [--all] [-m <body>] [--dry-run]` | reply; subject and thread are inherited |
| `amail ack <id>` | acknowledge |
| `amail withdraw <id> --from <seat>` | withdraw a message you sent from a seat of your org that has not been shown it (user tokens: any message of the org); see [concepts](concepts.md#delivery-states) |
| `amail status <id>` | per-recipient delivery state of a message you sent, with the time of each step (see [concepts](concepts.md#delivery-states)) |
| `amail thread <id>` | a thread, by thread id or the id of any message in it, with the cards of the other orgs' seats on it, the tasks and documents of the thread (`documents of this thread:`), and above a comment whose section the document no longer has `[comment on a removed section of d12 (was: <heading path>)]` |
| `amail thread delete <id>` | user token: delete what the org may delete of a thread (mail of deleted orgs, and mail the org sent that no other org received); says how many messages were deleted and how many stay |
| `amail who [--search text] [--org org]` | directory: seats (with descriptions and teams), teams, lists, linked orgs; `--search` keeps entries whose name or description contains the text. `--org` shows another org's card and the seats and teams it opened to your org (seat tokens too) |
| `amail focus on [--for 30m] [--seat s]` / `amail focus off` | hold all but urgent mail (user tokens: any seat) |
| `amail claim [--via v] [--classes to,cc,list] [--urgent-only] [--include-digest] [--limit n] [--inline-body-max n]` | take pending notifications (marks them notified) |

### Tasks

Every task command takes a task number as `t12` or `12`, prints the
server's JSON with `--json`, and otherwise one line per task: number,
shown state, priority, assignee (`-` for none), title, and what it waits
on (`· waits on t9, t11 (ok needed)`, or `· waits for w3: reason` for a
`blocked` task), the progress of its children (`· 3/5`) and its result
(`· ok`, `· rejected`). See [concepts](concepts.md#tasks) for states,
`ready`, results, child tasks, revisions and reminders.

The commands that change one task (`set`, `assign`, `take`, `block`,
`unblock`, `start`, `review`, `done`, `cancel`, `reopen`, `result`, `dep`,
`undep`, `link`, `unlink`) take `--if-rev N`: when the task's revision is
not `N` (`Rev:` in `task show`), nothing is changed, the command prints
the changes since revision `N` and the task as it is now (with `--json`,
the whole 409 answer), and fails with `stale`.

| command | |
|---|---|
| `amail task new <title> [-m body] [--priority urgent\|high\|normal\|low] [--assignee seat] [--team t] [--parent tN] [--label l]... [--due time] [--from <message id>] [--new]` | create a task; with `--from` the message's subject is the default title and its thread is linked. `--due` takes RFC 3339, `2026-10-30T12:00` or `2026-10-30` (UTC). The command sends a `client_key` derived from you, the title, the description, the assignee and the parent: the same command again within 24 hours prints the task it created and creates nothing. `--new` sends no key and creates another task |
| `amail task show <tN \| import id>` | the fields one per line (with `Rev:`, `Result: ok, ref abc123, by w2 at …`, `Children: 3/5 done`, `Imported as: ISSUE-42`), the description behind `  > `, blockers (those that need an ok result marked `(ok needed)`), blocked tasks, children, linked threads and the last 20 events. An argument that is not a task number is the id of an imported task |
| `amail task history tN [--all]` | the last 50 events (`--all`: every event, read page by page), one per line: time, actor, change |
| `amail task list [--state s,s\|all] [--assignee seat\|none] [--reporter seat] [--team t] [--priority p] [--label l] [--parent tN] [--due-before time] [--stale] [--text text] [--result ok\|rejected\|none] [--limit n] [--before n]` | tasks of the org, newest first; unfinished ones unless `--state`. Continue with `--before` (printed under the list) |
| `amail task mine` | your unfinished tasks, `ready` first |
| `amail task next [--pool]` | your `ready` tasks; `--pool` adds the unassigned `ready` tasks of your team |
| `amail task waiting` | whom you wait for: your `blocked` tasks with the seat they wait for, and each open blocker of your tasks with its assignee (`t12 … — waits for w3 (t9 doing)`) |
| `amail task owed` | who waits for you: tasks `blocked` on you, and your tasks that are the last open blocker of another seat's task (`t9 … — w1 waits for it (t12 open)`) |
| `amail task board [--team t]` | counts per state, the unfinished tasks of each column, the load per seat, stale and overdue tasks, and the tasks that wait on others (`t12 waits on t9, t11`) |
| `amail task report [--team t]` | the lead's report, one section with its count each: assigned but not started (`open` with an assignee, by seat), in progress (`doing`, by seat), delivered and waiting for review (`review`, by the reporter, who reviews), rejected (`done` with `rejected`), approved but not taken further (`done` with `ok` while tasks that depend on it are still open, `— still open: t12`), parents partly done (unfinished tasks with unfinished children, with their progress), blocked (`blocked`, or `open` with unsatisfied dependencies, `— waits for w3`) |
| `amail task set tN [--title t] [--body b] [--state s] [--priority p] [--rank n] [--assignee seat\|none] [--team t\|none] [--parent tN\|none] [--label l... \| --no-labels] [--due time\|none] [--force] [--if-rev N]` | change fields; `--label` replaces the labels; `--force` closes a parent whose children are not all closed |
| `amail task assign tN <seat\|none>` / `take tN` | assign (or unassign); `take` assigns an unassigned task to you and fails with `taken` when someone has it |
| `amail task steal tN` / `yield tN` / `keep tN` | ask the assignee of a task that is not started to hand it over to you; the assignee answers with `yield` (you get the task) or `keep`. A pending request shows as `· w2 asks to take over` in task lines and as `Takeover: asked for by w2 at <time>` in `show` |
| `amail task block tN [--on seat] [-m reason]` / `unblock tN` | set `blocked`, waiting for a seat (it gets a reminder) or for a reason; `unblock` sets `open` again |
| `amail task start` / `review` / `done` / `cancel` / `reopen tN` | set `doing`, `review`, `done`, `cancelled`, or `open` again (reopening clears the result). `done` and `cancel` take `--force` (close a parent whose children are not all closed; without it the command fails with `children_open`, naming them) |
| `amail task done tN --result ok\|rejected [--ref r] [-m note]` | close the task with a result: the version it is about (a commit, tag or URL, at most 200 characters) and a note (at most 2,000 characters) |
| `amail task result tN ok\|rejected [--ref r] [-m note]` | set or replace the result of a task that is `done` (fails with `not_done` otherwise) |
| `amail task result withdraw tN -m reason` | clear the result; the task stays `done` |
| `amail task dep tN --on tM [--needs-ok \| --any-close]` / `undep tN --on tM` | tN depends on tM (or no longer). With `--needs-ok` only a `done` tM with result `ok` satisfies the dependency; repeating the command with `--needs-ok` or `--any-close` changes an existing dependency |
| `amail task link tN <thread or message id>` / `unlink tN <thread id>` | link a thread to the task, or remove the link |
| `amail task comment tN [-m text]` | mail in the task's discussion thread (`[tN] <title>`) to its assignee and reporter, `Cc` earlier commenters; the body comes from stdin without `-m`. Replies in that thread are comments too |
| `amail task import <file> [--dry-run] [--map <out>]` | user token: create or update tasks from a JSON Lines file (format below). Prints one line per record, `ISSUE-42 -> t57 (created\|updated\|same)`, then the warnings (`warning: ISSUE-43: no seat "w9"; the task is left unassigned`). `--dry-run` checks the file and prints what would happen (`(new)` for tasks it would create) without changing anything; `--map` writes `id<TAB>tN` per record to a file |

#### Import file

One JSON object per line (blank lines and lines starting with `#` are
skipped), one per task:

```
{"id": "ISSUE-42", "title": "…", "body": "…", "state": "doing",
 "priority": "high", "assignee": "w3", "reporter": "lead",
 "labels": ["a"], "due": "2026-11-01T00:00:00Z", "parent": "ISSUE-40",
 "depends_on": [{"id": "ISSUE-41", "needs_ok": true}],
 "result": {"value": "ok", "ref": "abc123", "note": "…", "by": "w2",
            "at": "2026-10-20T10:00:00Z"},
 "created_at": "…", "updated_at": "…",
 "history": [{"at": "…", "by": "w3", "text": "…"}]}
```

- `id` (at most 200 characters, unique in the file) and `title` are
  required; every other field is optional. A field the format does not
  have is ignored with a warning.
- `state` is a task state; `ready` is read as `open`. Times are RFC 3339.
- `assignee` and `reporter` are seat names of the org. An unknown assignee
  leaves the task unassigned, an unknown reporter makes the importing user
  the reporter; both are warnings.
- `parent` and `depends_on` name other records by `id`, in the file or
  imported before. An unknown one, a cycle or a nesting too deep is a
  warning and is left out.
- `result` is kept only when the task is `done` (a warning otherwise); its
  `by` is a name, which need not be a seat.
- `history` entries become `imported` events with the given time, the
  name in `by` as the actor (it need not be a seat) and the text as the
  note. `created_at` becomes the task's creation time.
- A record whose `id` was imported before updates its task: the fields the
  record gives that differ are changed (with events, the importing user as
  the actor), history entries the task does not have yet are added, and
  missing parents and dependencies are set. Nothing is removed. Importing
  the same file twice gives `same` for every record.
- Nothing is mailed: no reminders and no discussion threads.

### Documents

Every document command takes a document as `d12`, `12` or a slug, and
`d12#anchor` (or `slug#anchor`) where a section is read, written,
extracted or commented. Commands print the server's JSON with `--json`,
and otherwise one line per document: number, kind, status, owner, title,
the description, and marks (`· stale`, `· pinned`, `· superseded by
d30`). See [concepts](concepts.md#documents) for kinds, statuses, who may
write what, links and the index.

The commands that write one document (`write`, `set`, `split`, `extract`,
`merge`) take `--if-rev N`: when the document's revision is not `N`
(`Rev:` in `doc show`), nothing is changed, the command prints the
revisions and events since revision `N` and the diff to the current
revision (with `--json`, the whole 409 answer), and fails with `stale`.
`new`, `write`, `set` and `supersede` take `--silent`: the events are
recorded, but no notification mail is sent.

| command | |
|---|---|
| `amail doc new <title> [--kind k] [-m body \| --file f] [--description d] [--slug s] [--team t] [--label l]... [--parent dN] [--supplements dN] [--message m] [--silent] [--new]` | create a document (`draft`; kind `reference` by default). Prints the document's line and revision, `warning: similar documents exist: …` when documents of the same kind have a close title, description or slug (the create is never refused for them), and the links that are dangling. `--file -` reads the body from stdin. The command sends a `client_key` derived from you, the kind, the title and the description: the same command again within 24 hours prints the document it created and creates nothing. `--new` sends no key |
| `amail doc show <dN>` | the fields one per line (`Kind:`, `Status:`, `Rev:` with the token estimate and size, `Owner:`, `Verified:` with the expiry, `Advice:`), the relations (parent, children, superseded by, replaces, supplements, supplemented by, related), the sections with anchors and token estimates (the first 30), the links out (`#evidence -> d99  (dangling)`, `(stale: the path no longer exists)`, `(renamed; was …)`), the links in (`Linked from:`), linked tasks and threads, comments (`on #rollback (Deploy > Rollback)`, `on a removed section (was: …)`), the last 20 events, and the body behind `  > ` up to 2,000 characters (1,000 for an `index` document), then `body not shown: amail doc read dN` |
| `amail doc read <dN[#anchor]> [--rev N] [--sections]` | a header line (`d12#rollback rev 3 · Deploy > Rollback · ~120 tokens`), then the whole body, one section, or the body of revision `N`. `--sections` lists the sections, indented by level, with anchors and token estimates |
| `amail doc write <dN[#anchor]> (-m text \| --file f \| stdin) [--section anchor] [--message m] [--if-rev N] [--silent]` | a new revision of the body, or of one section (the section's heading stays unless the text starts with a heading). Prints the document's line and the new revision, `unchanged` when the text equals the current one (no revision is written), dangling links and the advice to split |
| `amail doc set <dN> [--title t] [--description d] [--slug s\|none] [--labels a,b\|none] [--team t\|none] [--parent dN\|none] [--supplements dN\|none] [--related dN,dM\|none] [--owner seat] [--editors a,b\|none] [--status draft\|active\|archived] [--pin \| --unpin] [--message m] [--if-rev N] [--silent]` | change fields; the lists replace the old ones. A new title or description is a revision |
| `amail doc supersede <dN> --by <dM> [--silent]` | `dM` replaces `dN`: `dN` becomes `superseded` |
| `amail doc verify <dN> [--for 30d]` | confirm the content: sets the verification time and the expiry (`30d`, `12h`, `2w`, `none`; default the kind's interval) |
| `amail doc split <dN> [--level 1\|2] [--if-rev N]` | each heading of the level becomes a child document; prints where each section went (`#rollback -> d31`). Fails with `nothing_to_split` when there is no heading of that level |
| `amail doc extract <dN#anchor> --into <title> [--kind k] [--if-rev N]` | the section moves into a new document that supplements `dN`; prints `#anchor -> d31` |
| `amail doc merge <dM> --into <dN> [--if-rev N]` | `dM` is appended to `dN` as a section and becomes superseded by it; prints `dM -> dN#anchor` and each moved subsection |
| `amail doc list [--kind k] [--status s,s\|all] [--team t] [--owner seat] [--label l] [--stale] [--pinned] [--limit n] [--before n]` | documents newest first; `draft` and `active` unless `--status`. Continue with `--before` (printed under the list) |
| `amail doc search <terms>... [--kind k] [--status s\|all] [--team t] [--label l] [--owner seat] [--limit n]` | one line per section that matches: `d12#rollback  runbook  active  Deploy > Rollback — <snippet>  (verified 2026-10-20)`; `active` documents unless `--status`, your team first, stale documents last, at most 20 |
| `amail doc find <words>... [--kind k] [--status s] [--limit n]` | documents whose title, slug or description contains every word, in any status |
| `amail doc index [--team t]` | the index a seat of the team is shown each turn (default: your team): one line per document, `d12  runbook  <title> — <description>`, and how many were left out |
| `amail doc history <dN> [--all]` | every revision (`rev 3  <time>  w1  <title>  1,234 bytes  (message)`), then the last 50 events (`--all`: every event) |
| `amail doc diff <dN> [N] [M]` | a unified diff from revision `N` to `M` (default: the last change; with `N` only, from `N` to the current revision) |
| `amail doc backlinks <dN>` | the links into the document (`d40#steps -> d12#rollback`) |
| `amail doc check [--team t]` | the dangling and stale links of draft and active documents |
| `amail doc comment <dN[#anchor]> [-m text]` | mail in the document's discussion thread (`[d12] <title>`) to its owner, `Cc` earlier commenters; the body comes from stdin without `-m` |
| `amail doc kinds` | the org's kinds: who activates, whether active documents are in the index, the verification interval, the body cap, the number of documents, the required sections and the description |
| `amail doc kind add <name> [--sections a,b,c] [--activate-by owner\|human] [--no-index] [--cap bytes] [--verify-days n] [--description d] [--disable \| --enable]` | user token: add a kind, or change one. A kind is never deleted; `--disable` stops new documents of it |

### Seat handover

| command | |
|---|---|
| `amail handover show [seat] [--rev N]` | a seat's handover note (default: yours): who wrote it and when, in which session, which session read it, whether the server filled in `Pointers`, an unanswered request, the fields behind `  > `, and the newest 20 revisions |
| `amail handover write [file \| -m text] [--private \| --public] [--session id] [--seat s]` | write your note; the text comes from `-m`, a file (`-`: stdin) or stdin, with the fields as `Field:` lines or Markdown headings. A note lacking a field prints the labels it lacks and fails with `note_fields_missing`; an empty `Pointers` is filled in by the server from the worklog. `--private` limits the note to the seat, its lead and the user, `--public` lifts that; without either the previous note's setting is kept. `--seat`: a user token writes the note of an agent seat |
| `amail handover template` | the note's fields in order, with what each holds (needs no server) |
| `amail handover request <seat>` | the seat's lead or the user: ask the seat for its note; the seat gets mail |
| `amail worklog [seat] [--since 2h\|time] [--task tN] [--kind k] [--limit n] [--before id] [--summary]` | a seat's worklog (default: yours), newest first, one entry per line: time, kind, text, the task and document it names. `--summary` prints the number of entries, the counts per kind, the tasks touched and the last ten entries. The seat, its team, its lead and the user may read it |

### Administration of an org (user token)

| command | |
|---|---|
| `amail seat add <name> [--parent p] [--role r] [--harness claude\|codex\|dsh] [--model m] [--display-name d] [--permission-mode default\|unattended] [--description d] [--team t] [--contact]` | create a seat (the permission mode is stored for display; launches take it from the team file or `amail run --permission-mode`) |
| `amail seat adopt <seat> <id> [--restart]` | node command: the supervised seat resumes the existing harness session `id` from its next start; `--restart` restarts a running seat now. The node checks that the session exists on this host and belongs to no other seat ([teams](teams.md#starting-from-existing-conversations)) |
| `amail seat set/ls/rm/token` | change, list, delete a seat; issue a seat token. `set` also takes `--description d \| --no-description`, `--team t \| --no-team`, `--contact \| --no-contact`. `rm` also stops the seat and drops it from the node on this host, if that node runs it |
| `amail team ls` / `show <team>` / `set <team> [--description d] [--root seat] [--parent team] [--contacts a,b]` / `rm <team>` | teams (`set` creates a missing team; each option has a `--no-...` form) |
| `amail open <seat\|team> [--to <org> \| --to-linked]` | open a seat or team to every linked org (default) or one org; prints which orgs now see which cards |
| `amail close <seat\|team> [--to <org> \| --to-linked]` | close it (without `--to`: every opening of that seat or team) |
| `amail openings` | the org's openings: target, kind, audience, active or waiting for a link, made by a team file or by hand |
| `amail list create <name> [--policy anyone\|members\|user_only] [--description d]` | create a list |
| `amail list ls [name]` / `rm` / `add <list> <members...> [--mode immediate\|digest\|archive]` / `remove` | lists and members (seats of the org only) |
| `amail link request <org> [--note text]` / `accept <org>` / `ls` / `revoke <org>` | links with other orgs; a request's note (one paragraph, at most 500 characters) is shown to the other user |
| `amail link set <org> --primary <seat> [--team <team>]` / `--no-primary` | the primary contact of your org (or of a team) for that org: the seat added in Cc when a seat that is not a contact writes there |
| `amail token node` / `user` / `ls` / `revoke <id>` / `rotate [--label l]` | tokens; `rotate` replaces the user token in use and updates saved logins |
| `amail org show [<org>]` | your org: its card, settings, openings, links, the storage count and quota, and the mail of deleted orgs it keeps (and, once, a notice of the server); another org: its card, the link and since when, what it opens to you, what you open to it, your primary contact for it |
| `amail org purge-deleted-orgs` | user token: delete all mail of deleted orgs the org keeps |
| `amail org set [--send-rate-per-min n] [--send-burst n] [--max-recipients n] [--max-thread-depth n] [--ack-timeout-min n] [--focus-max-min n] [--digest-interval-min n] [--webhook-url url \| --no-webhook] [--description d \| --no-description] [--display-name d \| --no-display-name] [--visibility listed-by-name\|hidden] [--outbound any-seat\|contacts-only] [--new-threads-per-hour n]` | change org settings |
| `amail org delete [--confirm <org>]` | delete the org and all its data; asks for the name unless `--confirm` gives it |

### Running agents (Linux)

| command | |
|---|---|
| `amail node [options]` | run the node in the foreground |
| `amail node start [options]` / `amail node stop` | run it in the background with the same options (log and pid in the state directory; relative paths are made absolute) / stop it |
| `amail node token [--save]` | issue a node token (user token needed) and print it; with `--save` it is written with the URL to `<state dir>/node.toml` (mode 0600) and not printed |
| `amail node status` | the node's view of its seats, its local policy and where that comes from, the program it starts for each harness (and whether it was found), and the names of the variables `--dsh-env-file` sets |
| `amail node web-token` | a login token for the node's browser terminal |
| `amail run <seat> [--harness claude\|codex\|dsh] [--permission-mode default\|unattended] [--git-access worktree\|full\|off] [--dsh-jail on\|off] [--session id] [-- args]` | start the seat's harness session in this terminal. `--session` resumes an existing session of the seat's harness instead (it must exist on this host and belong to no other seat; see [starting from existing conversations](teams.md#starting-from-existing-conversations)). `--permission-mode unattended` runs it without permission prompts; without the flag a seat placed by `amail up` uses its team file's `permission_mode` and any other seat keeps the prompts. The server's seat setting is not used. `--git-access` (Codex seats) sets what the sandbox may write in the git repository (see [harness support](harnesses.md)); without it a seat placed by `amail up` uses its team file's `git_access`, any other seat `worktree` |
| `amail mcp [--node]` | an MCP server on stdin and stdout with mail and task tools ([below](#mcp-server)) |
| `amail up [-f amail.toml] [--only g,s] [--no-start] [--prune]` | bring a team up from a team file ([team files](teams.md)) |
| `amail down [--only g,s]` / `amail ps` | stop the sessions of every seat the node runs (of all team files; `--only` for some groups or seats) / one line per seat: state, zellij pane, pending mail, last activity (ACTIVE: the seat's last hook or launch; after a node restart, the last write to its conversation log until its next hook), whether its handover note is current (NOTE: `current`, `stale` or `none`; fetched from the server when the node connects, when a seat is added and after the seat writes its note; `-` while not known) and whether a person is needed |
| `amail attach <group or seat>` / `amail sleep <seat>` / `amail wake <seat>` | attach this terminal to a group's zellij session (for a seat: on the seat's tab) / put a seat to sleep (mail already waiting does not start it again; new mail, `wake` or `up` does) / start it now; on a seat the node believes busy, look again whether its turn ended and say what was found |

Node options: `--url`, `--token` (`AMAIL_NODE_TOKEN`), `--state-dir`
(`AMAIL_STATE_DIR`, default `~/.local/share/amail`), `--socket`
(`AMAIL_NODE_SOCK`, default `$XDG_RUNTIME_DIR/amail/node.sock`, or
`<state dir>/run/node.sock` without `XDG_RUNTIME_DIR`), `--label`,
`--claude-bin`, `--codex-bin`, `--dsh-bin` (`AMAIL_DSH_BIN`),
`--dsh-env-file` (`AMAIL_DSH_ENV_FILE`: `NAME=value` lines given only to
DeepSeek Harness hosts, for the model provider's key), `--zellij-bin`,
`--start-limit` (4),
`--codex-auth`, `--wake-window-ms` (3000), `--wake-confirm-ms` (10000),
`--web-terminal-port`, `--web-terminal-url`, `--local-policy open|restricted`
(`AMAIL_LOCAL_POLICY`; see
[security](security.md#trust-boundaries)), `--handover-threshold` (70: ask
for the handover note at the end of a turn whose input passed this
percentage of the context window), `--context-window N|MODEL=N` (the
context window in tokens, for every model or for models whose name contains
MODEL; may be repeated; `k` and `m` suffixes; see
[harness support](harnesses.md#what-the-node-does-with-hooks)). Without `--url`/`--token` the
node uses `AMAIL_URL`, then `node.toml`, then the current saved login's URL.
`node.toml` may also set `label` and `local_policy`; `amail node token
--save` keeps them.

`--codex-sandbox` (`AMAIL_CODEX_SANDBOX`) selects a native Codex policy:
`read-only`, `workspace-write`, or `danger-full-access`. It applies to fresh
and resumed sessions and replaces the node's `git_access` permission profiles;
the node does not change repository permission files or git layout in this
mode. Without the option, the normal workspace and git-access profiles apply.
`danger-full-access` runs commands without the Codex sandbox; use it only when
that access is intended, such as an externally isolated host that cannot run
the workspace sandbox.

### Server and operator

| command | |
|---|---|
| `amail serve --db <url> [--listen addr] [--max-connections n] [--advertise-url url] [--public] [--trusted-proxy cidr,...] [--base-path /prefix] [--retention-days n] [--webhook-private-targets allow\|deny] [limits]` | run the server ([operations](operations.md)) |
| `amail admin create-org <org> [--user-name owner]` | create an org with its user seat; prints the user token |
| `amail admin invite create [--count n] [--note text] [--expires 7d]` / `ls` / `revoke <id>` | invite codes |
| `amail admin operator-token create [--label l]` / `ls` / `revoke <id>` | operator tokens |
| `amail admin org ls` / `suspend <org>` / `resume <org>` / `delete <org> --confirm <org>` | orgs |
| `amail admin token reset <org>` | revoke all user tokens of an org and print a new one |
| `amail admin stats [--top 20]` | orgs, seats, mail volume, stored and kept MiB, busiest orgs |

All `amail admin` commands take `--db` (or `AMAIL_DB`) and work directly on
the database.

## MCP server

`amail mcp` is an MCP server (stdio, newline-delimited JSON-RPC) in the
same binary. It offers the mail, task and document commands, the handover
note and the worklog as typed tools, so a harness can call them without a
shell command. The command line stays the
documented interface: the hooks and the mail instructions name `amail`
commands, and the MCP server is an addition to them.

Each tool makes the same API calls as the matching command and answers
the server's JSON (the same JSON as the command with `--json`) as
structured content, with the command's text rendering as text content. An
API error is a tool result with `isError: true` whose structured content
is the server's `{"error": {"code", "message", "status"}}` (for 409
`stale`, the whole answer with `rev`, `task` and `since`, or for a
document `rev`, `doc`, `since` and `diff`, and the text shows the changes
and the diff; for 422 `note_fields_missing`, `missing`, and the text
gives the template); a missing or malformed argument gives the code
`invalid_arguments`. A line that is not
JSON is skipped without an answer; a request for an unknown method gets a
JSON-RPC error.

| tool | arguments | command or call |
|---|---|---|
| `inbox` | `all?`, `limit?` (50, at most 1,000) | `amail inbox` |
| `read` | `id`, `peek?` | `amail read` |
| `send` | `to[]`, `cc[]?`, `subject`, `body`, `priority?` (`normal`, `urgent`), `ack_required?`, `meta?` (string values), `dry_run?` | `amail send` |
| `reply` | `id`, `body`, `all?`, `priority?`, `ack_required?`, `dry_run?` | `amail reply` |
| `ack` | `id` | `amail ack` |
| `thread` | `id` (a thread or any message in it) | `amail thread` |
| `search` | `text`, `before_id?`, `limit?` (at most 200) | `GET /archive/search`; for a seat token, which that call refuses, the subjects and senders of the newest 1,000 messages of the seat's mailbox (`mode: "inbox_subjects"`) |
| `who` | `org?`, `text?` | `amail who [--search] [--org]` |
| `status` | `id?` | without `id`: `GET /whoami`, the seat's record (state as the node reports it, focus) and the unread count (counted up to 100); with `id`: `amail status <id>` |
| `focus` | `state` (`on`, `off`), `minutes?` | `amail focus on [--for]` / `off` |
| `task_new` | `title`, `body?`, `priority?`, `assignee?`, `team?`, `parent?`, `labels[]?`, `due?`, `from_message?`, `client_key?` (default: derived from the caller, title, body, assignee and parent, as `amail task new`) | `POST /tasks` |
| `task_show` | `num` | `GET /tasks/{num}` |
| `task_list` | `state?`, `assignee?`, `reporter?`, `team?`, `priority?`, `label?`, `parent?`, `due_before?`, `stale?`, `text?`, `result?`, `before?`, `limit?` | `GET /tasks` |
| `task_mine` | `state?`, `limit?` | `GET /tasks/mine` |
| `task_next` | `pool?` | `GET /tasks/next` |
| `task_waiting`, `task_owed` | | `GET /tasks/waiting`, `/owed` |
| `task_board` | `team?` | `GET /tasks/board` |
| `task_report` | `team?` | `GET /tasks/report` |
| `task_set` | `num`, `title?`, `body?`, `state?`, `priority?`, `rank?`, `assignee?`, `team?`, `parent?`, `labels[]?`, `due?`, `waiting_on?`, `blocked_reason?` (`null` clears), `result?` (`ok`, `rejected`, `null` withdraws), `result_ref?`, `result_note?`, `note?`, `force?`, `if_rev?` | `PATCH /tasks/{num}` |
| `task_result` | `num`, `result` (`ok`, `rejected`, `withdraw`), `ref?`, `note?` (for `withdraw`, the reason), `if_rev?` | `PATCH /tasks/{num}` with the result fields |
| `task_take` | `num`, `if_rev?` | `POST /tasks/{num}/take` |
| `task_steal`, `task_yield`, `task_keep` | `num` | `POST /tasks/{num}/steal`, `/yield`, `/keep` (ask to take over a task assigned to another seat and not started; answer a pending request on your own task) |
| `task_dep`, `task_undep` | `num`, `blocked_by`, `needs_ok?` (`task_dep`), `if_rev?` | `POST /tasks/{num}/deps`, `DELETE …/deps/{other}` |
| `task_link` | `num`, `thread_id` or `message_id`, `if_rev?` | `POST /tasks/{num}/links` |
| `task_unlink` | `num`, `thread_id`, `if_rev?` | `DELETE /tasks/{num}/links/{thread}` |
| `task_comment` | `num`, `body` | `POST /tasks/{num}/comments` |
| `task_history` | `num`, `after_seq?`, `limit?` | `GET /tasks/{num}/events` |
| `doc_new` | `title`, `kind?`, `body?`, `description?`, `slug?`, `team?`, `labels[]?`, `parent?`, `supplements?`, `message?`, `silent?`, `client_key?` (default: derived from the caller, kind, title and description, as `amail doc new`) | `POST /docs` |
| `doc_show` | `doc` | `GET /docs/{num}`; the structured content leaves `doc.body` out and gives `doc.body_chars` |
| `doc_read` | `doc` (`d12#anchor` reads a section), `section?`, `rev?`, `sections?`, `whole?` | `GET /docs/{num}` with `?section=`, `?rev=`. A whole document above 20,000 characters answers the section list (and `whole_chars`, without the body) unless `whole` is true |
| `doc_write` | `doc` (`d12#anchor` writes a section), `body`, `section?`, `message?`, `if_rev?`, `silent?` | `PUT /docs/{num}/body` |
| `doc_set` | `doc`, `title?`, `description?`, `slug?`, `labels[]?`, `team?`, `parent?`, `supplements?`, `related[]?`, `owner?`, `editors[]?`, `status?` (`draft`, `active`, `archived`), `pinned?`, `message?`, `if_rev?`, `silent?` (`null` clears `slug`, `team`, `parent`, `supplements`); or `superseded_by` alone (with `silent?`) | `PATCH /docs/{num}`; with `superseded_by`, `POST /docs/{num}/supersede` |
| `doc_search` | `terms`, `kind?`, `status?`, `team?`, `label?`, `owner?`, `limit?` | `GET /docs/search` |
| `doc_find` | `words`, `kind?`, `status?`, `limit?` | `GET /docs/find` |
| `doc_list` | `kind?`, `status?`, `team?`, `owner?`, `label?`, `stale?`, `pinned?`, `limit?`, `before?` | `GET /docs` |
| `doc_index` | `team?` | `GET /docs/index` |
| `doc_history` | `doc`, `after_seq?`, `limit?` | `GET /docs/{num}/history` |
| `doc_split` | `doc`, `level?`, `if_rev?` | `POST /docs/{num}/split` |
| `doc_extract` | `doc` (`d12#anchor`), `anchor?`, `into`, `kind?`, `if_rev?` | `POST /docs/{num}/extract` |
| `doc_verify` | `doc`, `for?` | `POST /docs/{num}/verify` |
| `doc_comment` | `doc` (`d12#anchor` comments on a section), `anchor?`, `body` | `POST /docs/{num}/comment` |
| `handover_show` | `seat?` (default: the caller), `rev?` | `GET /seats/{name}/handover` |
| `handover_write` | `text` or `fields` (`{state, summary, next, waiting_on, traps, verify, pointers}`), `private?`, `session?` | `PUT /seats/{name}/handover` for the caller's seat. On `note_fields_missing` the text names the missing fields and gives the template |
| `worklog` | `seat?` (default: the caller), `since?`, `task?`, `kind?`, `limit?`, `before?`, `summary?` | `GET /seats/{name}/worklog` |

A task number is an integer or a string such as `"t12"`; a document is an
integer, a string such as `"d12"`, or a slug, and `"d12#anchor"` names a
section. Tools that only read are marked with the `readOnlyHint`
annotation. The text of the document, handover and worklog tools is cut
at 25,000 characters, with a last line that says what to narrow (for
`worklog`: `… cut at 25,000 of 41,200 characters: pass since, task, kind
or a lower limit, or summary: true`); when
the JSON is larger than that too, the structured content is `{cut: true,
chars, narrow}` instead.

**Identity.** Without `--node`, the server finds its server URL and token
as the command line does ([above](#configuration-of-the-command-line)),
once, when it starts. With `--node`, which is what the node registers for
the sessions it launches, it uses only a seat's identity, never a saved
login: for a tool call whose `_meta` has `threadId` (Codex sends it with
every tool call), the credentials the node holds for that Codex thread,
asked for with the node's Codex pass (`AMAIL_CODEX_PASS`); otherwise
`AMAIL_TOKEN` with `AMAIL_URL`. When the server finds no identity it still
starts, and every tool call answers the code `no_identity`.

**Registration by the node.** Sessions that `amail run` or `amail up`
starts get the server without any setup (see [harness
support](harnesses.md)): Claude Code through `--mcp-config <state
dir>/claude/mcp.json`, Codex through `[mcp_servers.amail]` in the node's
Codex home. The node rewrites both when it starts; your own harness
configuration is not changed. dsh seats do not get it.

**Registering it by hand**, for a session you start yourself with
`AMAIL_TOKEN` and `AMAIL_URL` set, or with a saved login. Claude Code: a
project `.mcp.json` (or a file given with `claude --mcp-config`):

```json
{
  "mcpServers": {
    "amail": { "type": "stdio", "command": "amail", "args": ["mcp"] }
  }
}
```

Codex, in `config.toml`; Codex passes only a fixed set of variables to MCP
servers, so name the ones the server needs:

```toml
[mcp_servers.amail]
command = "amail"
args = ["mcp"]
env_vars = ["AMAIL_URL", "AMAIL_TOKEN", "AMAIL_PROFILE"]
```

Without `AMAIL_TOKEN`, a server registered this way uses your current
saved login, which is your user token: register it like this only for
sessions that may act as you.

## HTTP API

JSON under `/v1`; errors are `{"error":{"code":"…","message":"…"}}` (rate
limits add `retry_after_ms` and a `Retry-After` header). Requests
authenticate with `Authorization: Bearer <token>` or the web interface's
session cookie; with the cookie, everything but `GET` needs the header
`X-Amail-Csrf: 1`, and an `Origin` header must name the server.
After too many failed authentications from one client address, requests
from it with a credential the server has not seen valid lately get 429
`rate_limited` (see [security](security.md#resource-limits)).
`GET /healthz` (outside `/v1`) answers `ok`.

Under a path prefix, every path here is below it: with
`amail serve --base-path /amail` (`AMAIL_BASE_PATH`), the API is at
`/amail/v1/...` and the web interface at `/amail/`; `/healthz` answers at
the root and at `/amail/healthz`; other paths get 404. A reverse proxy that
removes a prefix before passing a request on sends
`X-Forwarded-Prefix: /amail` instead; the header counts only from a peer
listed with `--trusted-proxy`, must follow the same rules as `--base-path`
(a leading `/`; segments of letters, digits and `-._~`; no `.` or `..`
segment, query or `//`; a trailing `/` is removed), and comes before the
`--base-path` if both are set. The prefix sets the session cookie's `Path`
and is written into the web interface's page as
`<meta name="amail-base" content="/amail/">`. A browser request
(`Accept: text/html`) for a path below the prefix that is not a file, the
API or an asset, such as `/amail/org/root`, is redirected to the web
interface's route `/amail/#/org/root`. Clients put the prefix in the server
URL (`AMAIL_URL=https://example.com/amail`). See
[operations](operations.md#running-under-a-path-prefix).

### Mail

| | |
|---|---|
| `POST /messages` | send `{to, cc, subject, body, in_reply_to?, priority?, ack_required?, ack_timeout_s?, meta?}`; `ack_timeout_s` (with `ack_required`) is 0 to 2,592,000 seconds: 0 asks for the acknowledgement without any reminder, omitted uses the org's `ack_timeout_min`; the answer lists each address with its seat count (and for a list in `To`, `wakes`: its members notified at once) and `notes` on recipients that will not see it at once. 507 `storage_quota` when the message, with one fixed amount per delivery, does not fit in the org's storage quota (always when the org is at its quota). Mail to another org: rejected addresses there get `unreachable`, `thread_closed` or `not_linked`; the contact of your org the server adds in Cc is listed with `added: true`; 422 `no_contact` (your org has no contact for that org), `list_on_cross_org` (a list on such mail); 429 `thread_rate_limited` (new threads to other orgs per seat) |
| `POST /messages/preview` | the same body: `{recipients, rejected, deliveries}` as a send would give, nothing sent |
| `POST /seats/{name}/messages` | user tokens: the same body and answer as `POST /messages`, written by the user in the name of agent seat `name` of the org. The message is the seat's (source `agent`, the seat's recipients rules and budgets; `in_reply_to` may name any message the user can read) and has `written_by` (the user seat), which every reader sees. 403 for seat and node tokens and for a seat that is not an agent; 404 for a seat that does not exist |
| `GET /inbox` | `seat`, `unread`, `after_seq`, `before_seq`, `limit` (≤ 1000); headers only. `unread` leaves out withdrawn deliveries. A page is at most 2 MiB: when it was cut, `"truncated": true`, and the next page starts after the last entry (with `before_seq`: before the first). A page of more than 100 takes one of the org's query slots (see [below](#limits-on-expensive-requests)) |
| `GET /messages/{id}` | full message; marks the caller's delivery read unless `peek=1`. Headers here and everywhere else have `written_by` when the sending org's user wrote the message in the sender's name |
| `POST /messages/{id}/ack` | acknowledge |
| `POST /messages/{id}/deliveries/{seat}/withdraw` | the sender, or the user of the sender's org: withdraw the delivery to `seat` (a seat of the org) while it has not been notified, read or acknowledged ([concepts](concepts.md#delivery-states)). Answers `{delivery, changed}`; the delivery then has `withdrawn_at` and mode `archive`, and `changed` is false when it had been withdrawn before. 409 `delivery_seen` when it was notified, read or acknowledged; 422 `not_local` for an address of another org; 403 for anyone else; 404 when the seat has no delivery of the message. `…/cancel` is an alias of this call |
| `POST /messages/{id}/read` | mark the caller's delivery read |
| `POST /messages/{id}/unread` | user tokens: mark the user's delivery not read again (an acknowledged delivery stays read) |
| `POST /inbox/read` | user tokens: mark many of the user's deliveries read in one transaction, each as `POST /messages/{id}/read` does (reading counts as notified). The body is `{ids: [message ids]}` or `{all: true, before_seq?}`; with `before_seq`, only deliveries with `seq < before_seq` are marked (as in `GET /inbox`). Only unread deliveries that were not withdrawn change; ids of messages the user has no delivery of are skipped, not refused. Answers `{marked}`, the number of deliveries that changed; the user's seat gets one `mailbox` event with its new seq. The number of ids is bounded only by the request body limit (2 MiB). 400 when the body has neither or both forms, or `before_seq` with `ids`; 403 for seat and node tokens. Takes one of the org's query slots |
| `GET /messages/{id}/status` | per-recipient state (`queued`, `notified`, `read`, `acked`, `withdrawn`; the sender and the sender org's user only); seats reached through another org's list, including seats of a third org that the list contains, are counted under the list's address (`members`) |
| `DELETE /threads/{id}` | user tokens: delete, for the org, the messages of the thread it may delete: mail of orgs that no longer exist (the org's deliveries of it; the message goes once no org keeps a delivery of it), and mail the org sent that no other org has a delivery of (its internal mail, including mail from the mail system). Mail exchanged with orgs that still exist stays, for both sides, until retention clears its bodies. Answers `{deleted, kept, more}`: `kept` counts the messages that stay; at most 1,000 messages are deleted per call (among the newest 50,000 of the thread), and `more` says to call again. Replies to a deleted message lose their `in_reply_to`. Each seat that loses deliveries gets a `mailbox` event. Mail that a seat was shown in a turn that has not ended goes too: reading it then answers 404, and releasing it after a failed turn (`POST /claim/release`) releases nothing. 404 for a thread the org cannot see, as for a missing one. Takes one of the org's query slots |
| `POST /archive/purge-deleted-orgs` | `before_id` (user tokens): delete up to 1,000 messages of deleted orgs that the org keeps, looking at the org's mail below `before_id` in steps of 5,000 messages and starting no step after 100 ms; answers `{deleted, more, next_before_id}`, call again with `before_id` set to `next_before_id` while `more` is true. Takes one of the org's query slots |
| `GET /threads/{id}` | messages of a thread the caller may see, oldest first, `tasks`: the caller's org's tasks linked to it or whose discussion thread it is (`{num, title, state, assignee?}`), and `docs`: the caller's org's documents it cites (`[d12]` subjects) or whose discussion thread it is (`{num, slug?, kind, title, description, status, stale}`), and `doc_comments`: the messages that are comments on a document whose discussion thread it is (`{message_id, doc, anchor?, path?, removed}`; `removed` when the current revision no longer has the section) (404 for a thread the caller cannot see, as for a missing one): at most the newest 500 and 2 MiB of headers, with `"truncated": true` when older ones were left out. Bodies are included newest first up to 1 MiB in total; a message whose body was left out has `"body_omitted": true` and an empty `body`; read it with `GET /messages/{id}?peek=1` |
| `POST /claim` | `{classes, min_priority?, via, limit?, inline_body_max?, include_digest?, digest_only?, mark_inlined_read?, key?, preview_chars?}`: atomically mark pending deliveries notified and return them; concurrent claims never return the same delivery. `key` (chosen by the caller) makes the claim idempotent: a claim with a key the seat used in the last six hours takes nothing new and returns what the first one took. `inline_body_max` and `preview_chars` are capped at 16,384 characters |
| `POST /claim/release` | node tokens, `seat=`: `{key, messages}`: the turn that showed these messages of the claim with `key` failed; their deliveries go back to not notified, not read (unless acknowledged). Answers `{seat, released}` |
| `GET /changes` | `since` (a sequence number): deliveries created or changed since (`limit` ≤ 5000, at most 2 MiB); continue from `next` while `more` is true. Takes one of the org's query slots |
| `GET /events` | server-sent events: `mailbox` `{"seat","seq"}` per mailbox change (supports `Last-Event-ID`), `presence` (with `node_connected` as in `GET /seats`), for user tokens `directory` (`{kind: "seat"\|"list", name, change: "created"\|"updated"\|"deleted"}`) and `nodes` (`{connected}`: open node streams, sent at once and on change), for user and seat tokens `task` (`{num, change: "created"\|"changed"\|"assigned"\|"dependency"\|"link"\|"commented"\|"reminder"\|"steal", fields?}`: a task of the org changed; read it with `GET /tasks/{num}`) and `doc` (`{num, change: "created"\|"revised"\|"changed"\|"relation"\|"verified"\|"split"\|"extracted"\|"merged"\|"link"\|"commented"\|"reminder", fields?}`: a document of the org changed; read it with `GET /docs/{num}`), for user tokens `request` (`{num, change: "created"\|"answered"\|"expired"\|"withdrawn"\|"commented"\|"reminded"\|"linked", state, seat}`: a request for the person changed; read it with `GET /requests/{num}`), and for node tokens `command` (`{op, seat, lease_ms?, backfill?, node?, text?, request?, action?}`; `op` is `transcript`, `sleep`, `wake`, `prompt` (with `text`), `interrupt` or `attention_action` (with `request` and `action`, `restart` or `sleep`); nodes ignore ops they do not know) |
| `PUT /focus`, `DELETE /focus` | `seat=` (user tokens; seat tokens: their own): focus `{for_s?, until?}`, or end it |
| `GET /sent` | `seat`, `before_id`, `limit` (≤ 500), `delivery`: messages a seat sent, newest first; paged like the archive. With `delivery=true` (`limit` ≤ 100) the answer also has `delivery: [{message_id, queued, notified, read, acked}]`, how many deliveries of each listed message are in each state (and `withdrawn`) (each delivery counted in its most advanced state, across orgs, as in `/messages/{id}/status`) |
| `GET /archive/messages` | `since`, `until`, `from`, `to`, `thread`, `before_id`, `limit` (≤ 500) (user tokens): below |
| `GET /archive/search` | `q`, `before_id`, `limit` (≤ 200) (user tokens): search, below |

### Tasks

Every call needs a user or seat token (node tokens get 403, except for
`GET /tasks/mine?seat=<agent seat>`, which a node uses to show the seat
its count of open tasks at session start) and works on the tasks of the
caller's org only; a task number names a task of that org
(`12` and `t12` are both accepted in paths). The actor recorded for a
change is the token's seat (the user seat for a user token). Answers about
one task are a `TaskDetail`: `{task, blockers, blocked, children, links,
events, event_count, existing?}`, where `task` is
`{num, title, body, state, stored_state, priority, rank, reporter,
assignee?, team?, parent?, labels, due?, waiting_on?, blocked_reason?,
thread?, waits_on, waits_on_ok, rev, progress?, result?, result_ref?,
result_note?, result_by?, result_at?, import_key?, steal_requested_by?,
steal_requested_at?, blocked_on_human?, created_at, updated_at, closed_at?}`, `blockers`,
`blocked` and `children` are `{num, title, state, assignee?, result?,
needs_ok?}` (`needs_ok` in `blockers`: the dependency needs an ok result),
`links` are `{thread_id, linked_by?, linked_at}` and `events` are the
newest 20, oldest first. `state` is the shown state (`ready` for an `open`
task whose dependencies are all satisfied); `stored_state` is the stored
one; `waits_on` lists the blockers whose dependency is not satisfied, and
`waits_on_ok` those of them whose dependency needs an ok result. `rev` is
the sequence number of the newest event. `progress` is `{done, total}` for
a task with children (`total` leaves out `cancelled` children).
`blocked_on_human` is the number of the oldest open request for the person
on the task ([requests](#requests-for-the-person)): the task is
`blocked-on-human` while it is set. Listings leave `body` out.

Every write to one task (`PATCH`, `take`, `deps`, `links`) takes
`if_rev`, in the body (`PATCH`, `POST`) or the query (`DELETE …?if_rev=N`).
When the task's `rev` differs, nothing changes and the answer is 409
`{error: {code: "stale", message}, rev, task: TaskDetail, since: [events
after if_rev]}`.

| | |
|---|---|
| `POST /tasks` | create `{title, body?, priority?, assignee?, team?, parent?, labels?, due?, from_message?, client_key?}` (201, `TaskDetail`). `from_message` (a message the caller can read) gives the default title (its subject) and links its thread. `team` defaults to the assignee's team, else the reporter's. The assignee gets a reminder. `client_key` (1 to 64 characters, 422 `invalid_client_key` otherwise): when a task of the org was created with the same key in the last 24 hours, nothing is created and the answer is 200 with that task and `"existing": true`. A `parent` chain longer than 8 is refused with 422 `parent_too_deep` |
| `GET /tasks/{num}` | `TaskDetail` |
| `PATCH /tasks/{num}` | change `{title?, body?, state?, priority?, rank?, assignee?, team?, parent?, labels?, due?, waiting_on?, blocked_reason?, result?, result_ref?, result_note?, note?, force?, if_rev?}`; `null` clears `assignee`, `team`, `parent`, `due`, `waiting_on`, `blocked_reason`; `labels` replaces the labels. Each changed field is one event (`changed`; `assigned` for the assignee); a change of `body` records the whole old and new text. `state` is a stored state: `ready` is refused (400). `waiting_on` and `blocked_reason` only with `blocked` (422 `not_blocked`) and are cleared when the task leaves it. A `done` or `cancelled` task can only be reopened to `open` or `doing` (422 `invalid_transition`); reopening clears the result. Closing a task with children that are `open`, `doing`, `blocked` or `review` is refused with 409 `children_open` naming them, unless `force: true` (the state event's note names them). `result` (`ok` or `rejected`) only when the task is `done` after the change (422 `not_done`), with `result_ref` (at most 200 characters, one line: 422 `ref_too_long`, `invalid_ref`) and `result_note` (at most 2,000 characters: 422 `note_too_long`); it replaces the previous result and records `result_by` and `result_at`. `result_ref` or `result_note` without `result` is 400. `result: null` withdraws the result (event `result_withdrawn`, `from` the old result and ref, `note` the reason given in `note`). Answers `TaskDetail` |
| `POST /tasks/{num}/take` | optional body `{if_rev}`: assign the task to the caller if it has no assignee, atomically; 409 `taken` when someone has it, 409 `blocked_on_human` while a request for the person on the task is open, 422 `closed` for a closed task; nothing changes when the caller has it |
| `POST /tasks/{num}/deps` | `{blocked_by: <num>, needs_ok?, if_rev?}`: the task depends on that task; with `needs_ok: true` only a `done` blocker with result `ok` satisfies it. For an existing dependency, `needs_ok` changes it (event `changed`, field `needs_ok`; an `unblocked` event and reminder when this satisfies the last dependency); without it nothing changes. 422 `dependency_cycle` (including a task on itself), `dependency_too_deep` (the cycle check follows at most 32 tasks), `too_many_blockers` (50 per task), `too_many_blocked` (200 per task) |
| `POST /tasks/{num}/steal` | ask to take over a task assigned to another seat that is not started (stored `open`): the task gets `steal_requested_by` and `steal_requested_at`, and the assignee system mail in `To` (`w2 asks to take over t12: <title>`) naming `amail task yield t12` and `amail task keep t12`. 409 `blocked_on_human` while a request for the person on the task is open. One pending request per task: 409 `steal_pending` for another seat (asking again changes nothing); 422 `not_assigned` (use `take`), `own_task`, `started` (any other state, closed included). A request not answered within 24 hours expires (`steal_expired`; the requester is told) |
| `POST /tasks/{num}/yield` | the assignee or the org's user (403 `not_assignee` for anyone else; 404 without a pending request): the task is assigned to the requester (`assigned` event noted `yielded to <seat> on request`), who gets the `assigned` reminder |
| `POST /tasks/{num}/keep` | the same callers: the request is cleared (`steal_declined`) and the requester is told. Changing the assignee or the state of a task (also by `PATCH`) ends a pending request the same way, with a note |
| `DELETE /tasks/{num}/deps/{other}` | `if_rev` in the query: remove the dependency (404 when there is none); when it was the last unsatisfied one, the task gets an `unblocked` event and its assignee (else its reporter) a reminder |
| `POST /tasks/{num}/links` | `{thread_id}` or `{message_id}`, and `if_rev?`: link a thread (user tokens: any thread of the org's mail; seat tokens: threads the seat is in; 404 otherwise). Linking a linked thread again changes nothing. 422 `too_many_links` (50) |
| `DELETE /tasks/{num}/links/{thread}` | `if_rev` in the query: unlink (404 when not linked) |
| `POST /tasks/{num}/comments` | `{body}`: send a comment as the caller's seat in the task's discussion thread (the first comment starts it), subject `[t12] <title>`, `To` the assignee and the reporter, `Cc` the seats that commented before (the sender left out; with nobody else, to the sender). Any seat of the org may comment. Mail limits and budgets apply as for `POST /messages`. Answers `{id, thread_id, task}` |
| `GET /tasks/{num}/events` | `after_seq`, `limit` (≤ 1,000, default 200): events oldest first `{events, next_after_seq?, events_compacted}`; every event is kept, so reading pages with `after_seq=next_after_seq` until it is absent gives the whole history (`events_compacted` counts events that earlier builds deleted; 0 otherwise). Event: `{seq, at, actor?, written_by?, kind, field?, from?, to?, note?, message_id?}`; kinds `created`, `changed`, `assigned`, `linked`, `unlinked`, `dependency_added`, `dependency_removed`, `commented`, `reminder_sent`, `unblocked`, `steal_requested`, `steal_declined`, `steal_expired`, `result_withdrawn`, `imported` (`actor` is the name the import file gives). Reminder kinds (`field` of `reminder_sent`) include `children_done` and `blocker_rejected` |
| `GET /tasks` | filters `state` (comma-separated shown states, or `all`; default every state but `done` and `cancelled`), `assignee` (a seat, or `none`), `reporter`, `team` (tasks with that team, or whose assignee or reporter is in it), `priority`, `label`, `parent`, `due_before`, `stale=1` (`doing` or `review` and not changed for the org's stale period), `text` (substring of title or body; at most 200 characters), `result` (`ok`, `rejected` or `none`), `import_key` (the task imported from the record with that id, in any state), `before`, `limit` (≤ 500, default 50). Newest first: `{tasks, next_before?}`; continue with `before=next_before`. With `text`, a request looks at the 5,000 newest tasks below `before` and takes one of the org's query slots (other pool), so a page can be short and still have `next_before` |
| `GET /tasks/mine` | the caller's unfinished tasks: `ready` first, then `doing`, `review`, `blocked`, `open`; within each by priority, rank and number. `state`, `limit` (≤ 500) as for the list. `{tasks, truncated}`. Node tokens pass `seat=` (an agent seat of the org) |
| `GET /tasks/next` | the caller's `ready` tasks by priority, rank and number; `pool=1` adds the unassigned `ready` tasks of the caller's team. Tasks with an open request for the person (`blocked_on_human`) are left out |
| `GET /tasks/waiting` | `{items: [{task, seat?, blocker?}], truncated}`: the caller's tasks `blocked` with `waiting_on` (seat: that seat), and each unfinished blocker of the caller's unfinished tasks (seat: the blocker's assignee, else its reporter) |
| `GET /tasks/owed` | `{items: [{task, seat?, blocks?}], truncated}`: tasks `blocked` with `waiting_on` the caller (seat: their assignee, else reporter), and the caller's unfinished tasks (assigned, or unassigned and reported by the caller) that are the last open blocker of another seat's task (`blocks`) |
| `GET /tasks/report` | `team=` optional: `{team?, sections: [{name, count, items: [{task, seat?, waiting_for?, dependents?}]}]}`, the sections `not_started`, `in_progress`, `review`, `rejected`, `approved`, `parents`, `blocked` described under `amail task report` (the first 500 tasks of each; `count` is complete). `seat` is the assignee (in `review`, the reporter), `waiting_for` the seats a blocked task waits for, `dependents` the open tasks that depend on an approved one. Takes one of the org's query slots (other pool) |
| `POST /tasks/import` | user tokens (403 otherwise): `{records: [<record>...], dry_run?}`, at most 5,000 records and 64 MiB (the [import file](#import-file) format, one object per record). One transaction, two passes (tasks, then parents and dependencies). Answers `{results: [{id, num?, action}], warnings, dry_run?}`, `action` `created`, `updated` or `same`; with `dry_run` nothing is kept and `num` is absent for tasks that would be created. 422 `invalid_import` (`record 3: …`) for a record without `id` or `title`, an `id` used twice, or a field over its limit; 422 `too_many_records`; the task bounds and the storage quota apply |
| `GET /tasks/board` | `team=` optional: `{team?, columns: [{state, count, tasks}], load: [{seat, doing, ready, blocked, waiting, review}], stale, overdue, chains: [{num, waits_on}]}`: one column per shown state (the first 100 tasks each), the load per assignee, stale and overdue tasks, and unfinished tasks waiting on unfinished blockers (200). Takes one of the org's query slots (other pool) |

Changes publish `task` events on `GET /events` (see below). Task calls
count against the org's request limit; tasks count against the org's
storage quota (`task_bytes` in `GET /org`): a change that would take the
org over it is refused with 507 `storage_quota`.

### Documents

Every call needs a user or seat token (node tokens get 403, except for
`GET /docs/index?seat=<agent seat>`) and works on the documents of the
caller's org only. In paths, `{num}` is `12`, `d12`, or a slug; `d12#anchor`
(with `#` written `%23`) also names a section where one is read, written or
commented. The actor of a change is the token's seat (the user seat for a
user token). See [concepts](concepts.md#documents) for who may write what.

Answers about one document are a `DocDetail`: `{doc, read?, sections,
parent?, children, superseded_by?, replaces, supplements?,
supplemented_by, related, links, linked_from, threads, tasks, comments,
events, event_count, existing?, unchanged?, candidates?, candidates_line?,
dangling?, moved?}`, where `doc` is `{num, slug?, kind, title,
description, status, owner, team?, labels, editors, pinned, rev, bytes,
tokens, sha256, verified_at?, verified_by?, expires_at?, stale, superseded_by?,
replaces, supplements?, parent?, thread?, body, advice?, created_at,
updated_at}`; documents named from it are `{num, slug?, kind, title,
description, status, stale}`; `sections` are `{anchor, level, heading,
path, start, end, tokens}` (`start`, `end`: the byte range with the
subsections); links are `{src, src_anchor, kind, target, num?, anchor?,
title_at_write?, title?, renamed, dangling, stale}` with `kind` `doc`,
`task`, `message`, `thread`, `seat` or `path` and `target` as written
(`d12#rollback`, `t3`, `#45`, `thread 7`, `@w1`, `src/main.rs:10`);
`comments` are `{message_id, by?, at, anchor?, path?, removed}`; `events`
are the newest 20, oldest first. Listings leave `body` out. `advice` is
the line `the body is ~52,000 tokens; split it with amail doc split d12`
while the estimate is above 40,000 tokens.

Writes to one document (`PUT …/body`, `PATCH`, `split`, `extract`,
`merge`) take `if_rev` in the body. When the document's `rev` differs,
nothing changes and the answer is 409 `{error: {code: "stale", message},
rev, doc: DocDetail, since: [events after the revision if_rev], diff}`;
`since` holds the `revised` events (with `rev`, `actor` and the message in
`note`) and the other events since, and `diff` is the unified diff from
`if_rev` to the current revision. `silent: true` (create, body, `PATCH`,
`supersede`) records the events but sends no mail.

Error codes: 409 `stale`, `frozen` (the body of an active decision),
`slug_taken`; 403 `not_owner` (a write the caller may not make, including
activating a document the user activates); 422 `sections_missing`,
`body_too_large`, `nothing_to_split`, `no_section`, `invalid_transition`,
`invalid_relation`, `parent_cycle`, `parent_too_deep`, `unknown_kind`,
`kind_disabled`, `invalid_title`, `invalid_description`, `invalid_slug`,
`invalid_label`, `too_many_labels`, `too_many_editors`, `invalid_message`,
`invalid_kind`, `invalid_section`, `invalid_cap`, `invalid_interval`,
`invalid_client_key`, and on a public server `too_many_docs`,
`too_many_kinds`; 507 `storage_quota`.

| | |
|---|---|
| `POST /docs` | create `{title, kind?, body?, description?, slug?, team?, labels?, parent?, supplements?, owner?, editors?, message?, client_key?, silent?, paths?}` (201, `DocDetail`, status `draft`). `kind` defaults to `reference`, `team` to the owner's team, `owner` to the caller. `paths` is `[{path, exists}]`, what the node found for repository paths in the body. The answer lists the `dangling` links and the `candidates` (`[{doc, similarity}]`, at most 5 documents of the same kind with a close title, description or slug) with `candidates_line`; the create is never refused for them. `client_key` as for `POST /tasks` (200 with `existing: true`). `supplements` mails the owner of that document |
| `GET /docs` | `kind`, `status` (comma-separated, or `all`; default `draft,active`), `team`, `owner`, `label`, `stale=1`, `pinned=1`, `before`, `limit` (≤ 500, default 100): newest first, `{docs, next_before?}` |
| `GET /docs/{num}` | `DocDetail` with the whole body. `?section=anchor` (or `d12%23anchor`) and `?rev=N` answer what was asked in `read` (`{rev, title, anchor?, path?, text, tokens}`) and leave `doc.body` out; 422 `no_section` |
| `PUT /docs/{num}/body` | `{body, section?, message?, if_rev?, silent?, paths?}`: a new revision of the body, or of one section (the section's heading stays unless `body` starts with a heading). A body equal to the current one writes nothing (`unchanged: true`). A revision by a seat other than the owner mails the owner (`d12 was revised by w2: <title>`) unless `silent` |
| `PATCH /docs/{num}` | `{title?, description?, slug?, labels?, team?, parent?, supplements?, related?, owner?, editors?, status?, pinned?, expires_at?, message?, if_rev?, silent?}`; `null` clears `slug`, `team`, `parent`, `supplements`, `expires_at`; `labels`, `editors` and `related` replace the lists. A new title or description is a revision; the other fields are events. `status` is `draft`, `active` or `archived`. Unless `silent`, a new owner is mailed (`d12 is now yours to keep: <title>`), and a status change by someone other than the owner mails the owner (`d12 was activated by owner: <title>`) |
| `POST /docs/{num}/supersede` | `{by, silent?}`: `by` replaces the document, which becomes `superseded`; the owner and the user only. The owner is mailed when someone else does it |
| `POST /docs/{num}/verify` | optional `{for}` (`30d`, `12h`, `2w`, a number of days, or `none`; default the kind's interval): sets `verified_at`, `verified_by`, `expires_at`; the owner and the user only |
| `POST /docs/{num}/split` | optional `{level, if_rev}` (default 1): each heading of that level becomes a child document; `moved` lists `{from: "#anchor", to: "d31"}`; 422 `nothing_to_split` |
| `POST /docs/{num}/extract` | `{anchor, into, kind?, if_rev?}`: the section moves to a new document titled `into` that supplements this one |
| `POST /docs/{num}/merge` | `{into, if_rev?}`: the document is appended to `into` as a section and becomes superseded by it; the owner of both or the user. `moved` lists `{from: "d31", to: "d12#anchor"}` and each moved subsection |
| `GET /docs/{num}/history` | every revision `{rev, title, author?, written_by?, message?, sha256, bytes, at}` and a page of events (`after_seq`, `limit` ≤ 500, default 200; `next_after_seq`). Event: `{seq, at, actor?, written_by?, kind, field?, from?, to?, note?, message_id?, rev?}`, kinds `created`, `revised`, `changed`, `relation`, `verified`, `split`, `extracted`, `merged`, `links_rewritten`, `linked`, `commented`, `reminder_sent` |
| `GET /docs/{num}/diff` | `from`, `to` (default: the revision before the current one, and the current one): `{num, from, to, diff}`, a unified diff, with `title:` and `description:` lines first when those changed |
| `GET /docs/{num}/links` | `{links, linked_from}` |
| `POST /docs/{num}/comment` | `{body, anchor?}`: a message in the discussion thread, subject `[d12] <title>`, to the owner, `Cc` the seats that commented before; with an anchor the body starts with `On d12#rollback (Deploy > Rollback):`. Answers `{id, thread_id, doc}` |
| `GET /docs/search` | `q` (terms, at most 10), `kind`, `status` (default `active`; `all`), `team`, `label`, `owner`, `limit` (default 20): `{hits: [{num, slug?, anchor, path, kind, status, title, stale, verified_at?, snippet, line}], titles_only?}`, the caller's team first, stale documents last. `line` is `d12#rollback  runbook  active  Deploy > Rollback — <snippet>  (verified 2026-10-20)` (`stale` after the status when it is). The snippet is the sentence with the most terms, at most 200 characters: a sentence ends at `.`, `!` or `?` before a space (not inside a code span) and at a line's end; a heading is shown without its `{#id}` attribute, and a cut never ends inside a code span. When every term is shorter than 3 characters, titles and descriptions are searched (`titles_only`). Takes a query slot of the search pool |
| `GET /docs/find` | `q`, `kind`, `status` (default all), `limit`: documents whose title, slug or description contains every term, `{docs}` |
| `GET /docs/index` | `team` (default the caller's team), `max_lines` (default 60), `max_chars` (default 3,000), `seat` (node tokens): `{team?, lines, more, more_line?, text}`. Each line is `d12  runbook  <title> — <description>`, with `  (stale)` past the expiry; pinned first, then the team's, then documents of no team. `more_line` is `… and 41 more: amail doc list --team backend`; `max_chars` counts the newlines and the more line, `max_lines` only the document lines |
| `GET /docs/check` | `team`, `limit` (≤ 500): `{problems: [links]}`, the dangling and stale links of draft and active documents |
| `GET /docs/kinds` | `{kinds: [{name, description, required_sections, activate_by, in_index, body_cap_bytes, verify_days?, builtin, disabled, docs}]}` |
| `POST /docs/kinds` | user tokens: `{name, description?, required_sections?, activate_by?, in_index?, body_cap_bytes?, verify_days?, disabled?}` adds a kind or changes one (`verify_days` 0: no expiry). A kind is never deleted; `disabled` stops new documents of it |

Changes publish `doc` events on `GET /events`. Documents count against the
org's storage quota (`doc_bytes` in `GET /org`).
### Seat handover

The handover note and the worklog of an agent seat ([concepts](concepts.md#seat-handover)).
`{name}` is a seat of the caller's org (404 for any other name);
writing a note or asking for one needs an agent seat (422
`not_an_agent`). The
lead of a seat is the root seat of the seat's team (`root` in the team
records); a seat without a team has no lead.

A note is `{seat, rev, fields, text, chars, pointers_filled, private,
source, draft, session?, written_by?, written_at, read_by_session?,
read_at?}`: `fields` is `{state, summary, next, waiting_on, traps, verify,
pointers}`, `text` the fields as `Label: value` lines (a field of several
lines as `Label:` followed by its lines), `chars` the length of `text`,
`source` `agent`, `compaction` (a draft from a compaction summary,
`draft: true`) or `server` (the idle note), `written_by` the user's name
when the user wrote it for the seat. A revision in a list is
`{rev, written_at, source, private, chars, session?, written_by?,
read_by_session?, read_at?}`. A worklog entry is `{id, seat, session?, at,
kind, text, task?, doc?, source}`, `source` `server` or `node`.

| | |
|---|---|
| `GET /seats/{name}/handover` | `rev=` optional. `{seat, note?, current_rev, revisions, private, requested_at?, requested_by?}`: the current note (or revision `rev`; 404 when there is none), the newest 20 revisions newest first, whether the seat's notes are private (the current note's flag, the revision's flag or the team file's `handover = "private"`), and an unanswered request (a request newer than the current note). Every seat of the org, the user and node tokens read it; a private note only the seat itself, its lead, the user and node tokens (403 `private` otherwise) |
| `PUT /seats/{name}/handover` | `{text? \| fields?, private?, session?, source?}`: write the next revision; answers as `GET`. The seat itself or the user (`written_by`); node tokens only with `source: "compaction"` (403 otherwise). `text` gives each field as a Markdown heading (`## State`) or a `State:` line, running to the next field (labels in any case; text before the first field is left out); `fields` gives them as JSON. Fields in order: `State`, `Summary`, `Next`, `Waiting on`, `Traps`, `Verify`, `Pointers`. A note lacking one of the first six is 422 `note_fields_missing` with `missing` (the labels, in order); a note over 16,384 characters (of `text`) is 422 `note_too_large` with `chars` and `max`. An empty `Pointers` is filled in by the server (`pointers_filled: true`): the tasks and documents named by the worklog entries of `session` (without one: by the entries since the previous note), and the seat's last `commit` entry. `private` not given keeps the previous note's flag. `session` is at most 200 characters (422 `invalid_session`). `source: "compaction"` takes `text` as the draft's `Summary` when it has no field labels, sets `State` to `draft: from compaction summary` when empty, and checks no fields. 507 `storage_quota` when the org is at its quota |
| `POST /seats/{name}/handover/request` | the seat's lead, the user or a node token (403 otherwise): records the request and sends the seat system mail in `To` (`Handover note requested by <seat>`) naming `amail handover write`. The next note that is not a compaction draft answers the request; the lead or the user who asked then gets system mail `Handover note of <seat> written (rev N)` naming `amail handover show <seat>` (not when the asker wrote the note itself). A node asks in the name of the org's system seat (`requested_by: "amail"`, subject `Handover note requested by the node`), for a seat idle with work in progress. Answers `{seat, requested_at, message_id?}` |
| `GET /seats/{name}/handover/private` | user tokens: `{seat, private}`, the seat's own setting (the team file's `handover = "private"`), not the flags of its notes |
| `PUT /seats/{name}/handover/private` | user tokens: `{private}` sets it (`amail up` applies the team file with it). While it is set, the seat's notes are private whatever their flags. Answers `{seat, private, changed}`; 422 `not_an_agent` |
| `POST /seats/{name}/handover/read` | node tokens and the seat itself: `{session, rev?}` records that `session` was shown revision `rev` (default: the current one). Answers `{seat, rev, read_by_session, read_at}`; 404 when the seat has no such note, 400 without `session` |
| `GET /seats/{name}/worklog` | the seat itself, seats of its team, its lead, the user and node tokens (403 otherwise). `since` (`30m`, `2h`, `7d` before now, or an RFC 3339 time), `task` (`t12` or `12`), `kind`, `before` (an entry id), `limit` (≤ 1,000, default 100): entries newest first, `{seat, entries, next_before?}`; continue with `before=next_before`. With `summary=1`: `{seat, total, kinds: [{kind, count}], tasks: [{task, count, last_at}], last}` over the same filters (`kinds` most first, `tasks` most recent first, at most 100, `last` the newest 10 entries). Takes one of the org's query slots (other pool) |
| `POST /worklog` | node tokens: `{node?, entries: [{seat, session?, at?, kind, text, task?, doc?}]}` for agent seats of the org. `kind` is 1 to 40 characters of `a-z`, `0-9`, `_`, `-`, `.`; `at` is RFC 3339 (default and upper bound: now); `text` is cut to 1,000 characters, `session` to 200. Entries naming another seat, or with an unreadable kind or time, are left out. Answers `{accepted, skipped}`. At most 8 MiB per request (413 `too_large`); on a server with `--max-worklog-batch` (public default 5,000) more entries are 422 `too_many_entries` |

The server writes worklog entries itself, for agent seats, with `source:
server`, the seat's last session the node reported and, when the entry
names no task, the seat's `doing` task changed last: `mail_sent`
(`m12 "subject" to b, c`), `mail_read` and `mail_acked`
(`m12 "subject" from a`; `GET /messages/{id}` without `peek`, `POST
/messages/{id}/ack`; a `[t12]` subject names task 12), `task`
(`t12: created (open): title`, `t12: open → doing`, `t12: changed title,
priority`: changes the seat made), `focus` (`focus on until …`,
`focus off`), `handover` (a note revision written) and `doc` (`d12
created (draft runbook): title`, `d12#steps revised (rev 3): message`,
`d12: draft → active; changed labels`, `d12 superseded by d14`, `d12
verified until …`, `d12 split into d31, d32`, `d12#rollback extracted
into d33`, `d33 merged into d12`, `d12#steps commented (m40)`; the entry's
`doc` is the document), `request` (`r12 created: title`, `r12 answered: 1. yes — Ship it: title`, `r12 expired: …`, `r12 withdrawn: …`; for the seat a request is about), `prompt_from_user` (`owner sent a prompt: text`) and `interrupt`. When a change closes a task and its assignee (an agent seat)
holds no other task that is not `done` or `cancelled`, the server writes
the seat's note itself (`source: server`): `State: idle: t12 closed, no
other work`, `Summary` from the task's title, state and result, `Pointers`
from the worklog, `Traps` from the agent's note it replaces. It does not
replace a note the agent wrote in the turn that closed the task or later,
or any agent's note when the node reported no turn of the seat. Notes and
worklog entries count against the org's
storage quota in `task_bytes`; a worklog entry that does not fit deletes
the org's oldest entries to make room.

### Requests for the person

Requests are what waits for the org's user: a question a seat asks, a plan
or merge to approve, a seat that needs a person ([concepts](concepts.md#the-users-queue-needs-you)).
Each has a number per org (`r12`; `12` and `r12` are both accepted in
paths), never reused. A request is `{num, kind, seat, asker?, task?,
title, body?, options?, free_text?, default?, class, state, deadline?,
answer?, thread?, source, attention?, rev, created_at, answered_at?}`:

- `kind`: `question`, `plan`, `merge`, `attention` (`permission` is part
  of the record but is not created yet: 422 `kind_not_supported`).
- `seat`: the seat the request is about, which waits for the answer;
  `asker`: the seat that made it (absent for a node).
- `options`: 2 to 6 `{key, label, description?}`; a `question` or `plan`
  may instead (or also) take free text (`free_text: true`).
- `default`: the key of the option taken at the deadline.
- `class`: `now`, `soon` (default) or `later`. It sets the default
  deadline (15 minutes, 4 hours, 24 hours from creation), the urgency of
  the mail to the user (`now` is urgent) and the reminders (`now`: at the
  deadline, then every hour; `soon` and `later`: at the deadline, then
  daily).
- `state`: `open`, `answered`, `expired` (the default was taken),
  `withdrawn`, `superseded` (not used yet).
- `deadline`: with a default, when it is taken; without one, when the
  first reminder goes out. `attention` requests have none.
- `answer`: `{option?, text?, by, at, note?}`; `by` is the user's name, or
  `deadline` when the default was taken.
- `thread`: the thread of the request's mail to the user.
- `source`: `cli` (default), `mcp`, `hook`, `node`, `mail`.
- `attention`: for `attention` requests, the seat's attention code.
- `rev`: the sequence number of the newest event.

Answers about one request are `{request, events, threads, existing?,
terminal_url?}`: the newest 100 events oldest first (`{seq, at, kind,
actor?, note?, message_id?}`, kinds `created`, `answered`, `expired`,
`withdrawn`, `commented`, `reminded`, `linked`), and the other threads
that name the request (`[r12]` in a subject).

| | |
|---|---|
| `POST /requests` | `{kind?, seat?, task?, title, body?, options?, free_text?, default?, class?, deadline_s?, source?, attention?}` (201). Seat tokens ask for themselves (`seat` left out or their own name; 403 otherwise) with `question`, `plan` or `merge`; user tokens name an agent seat (400 without one, 422 `not_an_agent`); node tokens make `attention` requests only (`seat` and `attention`, the code, required; the options, class `now` and the title are set by the server, `body` is kept as the hint). Other combinations are 422 `kind_not_allowed`. `task` is `12` or `"t12"` (404 for no such task). `deadline_s` is seconds from now. Format rules (422): `title_empty`, `invalid_title` (more than one line), `title_too_long` (512 characters), `body_too_large` (64 KiB), `options_count` (2 to 6, or none with `free_text`), `invalid_option_key` (1 to 32 characters of `A-Z`, `a-z`, `0-9`, `_`, `-`, `.`, not digits only), `duplicate_option` (keys compared without case), `invalid_option_label` (empty or several lines), `option_label_too_long` (200), `option_description_too_long` (1,000), `invalid_default` (not a key), `free_text_not_allowed` (not a `question` or `plan`). A node's `attention` request for a seat and code that has a live one answers that one (200, `existing: true`). Every request but `attention` is mailed to the user: from the asking seat (a user's request: from the mail system), `To` the user, subject `[r12] <title>`, urgent for class `now`, with the body, the options as lines `1. key — label`, the default and deadline, and how to answer. 507 `storage_quota` when the org is at its quota; on a public server 429 `too_many_open_requests` and `request_rate` |
| `GET /requests` | `state` (`open` by default, `all`, or comma-separated states), `kind`, `seat`, `class`, `task`, `limit` (≤ 500, default 200): `{requests, truncated?}`, open ones first (`now`, then `soon`, then `later`, oldest first), then the others newest first. User tokens see the org's requests; seat tokens those about them or made by them |
| `GET /requests/{num}` | user tokens any request of the org, seat tokens their own (404 otherwise) |
| `POST /requests/{num}/answer` | user tokens: `{option?, text?, note?, if_rev?}`. `option` is a key, a number from 1 or a label (422 `invalid_option`); `text` alone answers a free-text request, or names an option like `option` does. 422 `answer_required`, `free_text_not_allowed`, `answer_too_long` (16,384 characters), `note_too_long` (4,000). 409 `stale` when `if_rev` is not the request's `rev`, `answered` when it was answered or expired already, `not_open` when it was withdrawn; each 409 carries `request` (with the first answer). The answer is mailed in the request's thread from the user `To` the seat (a sleeping seat wakes). For `attention`: `restart` and `sleep` go to the seat's node as an `attention_action` command, `open_terminal` answers the seat's `terminal_url` when its node reported one, `dismiss` does nothing; nothing is mailed |
| `POST /requests/{num}/withdraw` | the seat the request is about, the seat that made it (seat tokens), or the user: `{note?}` (optional body). Withdraws an open request (409 `not_open` otherwise, with `request`); the other side (the user, or the seat when the user withdrew it) gets a reply in the thread |
| `GET /requests/{num}/wait` | `timeout_s` (default 60, at most 600): waits until the request is no longer open or the time is up; `{request, done}` (`done: false` when it is still open). Seat tokens wait for their own requests, user and node tokens for any of the org's |

A reply of the user in a request's thread answers an open request when its
first line (empty and quoted `>` lines skipped) names an option: its number
(`2` or `2.`), key or label, without regard to ASCII case, or a whole
option line `2. no — Hold it`. A reply to a free-text request without
options answers it with the reply's text. The answer is recorded with the
reply as its message; when the reply did not reach the seat in `To`, the
answer is also mailed to it. Any other reply in the thread is a comment
(event `commented`): when the user wrote it and it did not reach the seat,
it is mailed to the seat; the request stays open. Replies of seats are
comments. A subject with `[r12]` links the message's thread to request 12
(event `linked`), as `[t12]` and `[d12]` do.

The deadline pass runs with the task reminder pass (every minute): an open
request with a default whose deadline passed takes it (`expired`, answered
by `deadline`), and the answer is mailed to the seat from the mail system;
an open request without a default whose reminder time passed is listed in
the user's one reminder mail of the pass, with the user's task and
document reminders, and is reminded again after the class's interval.

A node's presence report (`POST /presence`) keeps the `attention` requests
in step: a seat reported with `attention` (or a failed turn,
`turn-failed`, or a failed wake-up, `wake-failed`) gets one request per
seat and code while the condition lasts; a seat reported without it ends
the live request (withdrawn by `node` when still open). An answered
`attention` request stays live until the condition is gone, so the next
report does not make a new one.

Changes publish `request` events on `GET /events` for user tokens.
Requests and their events count against the org's storage quota in
`request_bytes`. A task with an open request shows `blocked_on_human`
(the oldest open request's number); see `POST /tasks/{num}/take`.

### Directory and settings

| | |
|---|---|
| `GET, POST /seats`; `GET, PATCH, DELETE /seats/{name}`; `POST /seats/{name}/tokens` | seats; `GET /seats` cuts each role to its first 1,000 characters and sets `role_truncated`, `GET /seats/{name}` has all of it |
| `GET, POST /lists`; `GET, DELETE /lists/{name}`; `PUT, DELETE /lists/{name}/members/{address}` | lists (at most 1,000 members each, seats of the org only: 422 `local_only`) |
| `GET, POST /links`; `POST /links/{org}/accept`; `PATCH, DELETE /links/{org}` | links; `POST` takes `{org, note?}`; `PATCH` (user) `{primary: seat \| null, team?}` sets a primary contact (422 `not_a_contact` for a seat not open to that org); active links show `primary` |
| `GET /teams`; `GET, PUT, DELETE /teams/{name}` | teams; `PUT` (user) `{description?, root?, parent?, contacts?, managed?, doc_activate_by?}` creates or changes one (`doc_activate_by: "human"`: every document of the team waits for the user to activate it; `null` or `owner`: the kind decides) (422 `no_contacts` when an open team would have none) |
| `GET, POST /grants`; `DELETE /grants/{id}`; `POST /grants/preview` | openings; `POST` (user) `{target, audience?: "linked" \| org, managed?}`; changes and the preview `{add: [...], remove: [ids]}` answer `{grant?, changes: [{org, gains, loses}]}` |
| `GET /orgs/{name}` | another org: `{card, link?, open_to_you, you_open, primary?}`; 404 for a name no org has and for a hidden org without an active link (600 lookups per hour per org) |
| `DELETE /org/notice` | user: the one-time notice in `GET /org` was shown |
| `GET, PATCH /org` | org settings (including `webhook_url`; `null` clears it; `display_name`, `description`, `visibility`, `outbound`, `new_threads_per_hour`, `task_stale_hours`: 0 to 8,760, 0 turns stale reminders off, default 24), `stored_bytes` (the mail the org sent with its deliveries, as counted for the storage quota) with `storage_quota_bytes` when the server has a quota, `task_bytes` (the org's tasks, counted against the quota with `stored_bytes`), `doc_bytes` (the org's documents, counted the same way), `request_bytes` (the org's requests for the person, counted the same way), and `kept_bytes` (the mail of deleted orgs the org keeps, counted the same way but not against the quota) |
| `DELETE /org` | user tokens, `{"confirm": "<org name>"}`: delete the org and all its data; mail other orgs received from it stays with them, from `name@org (deleted org)` |
| `GET, POST /tokens`; `DELETE /tokens/{id}` | tokens |
| `POST /tokens/rotate` | user tokens, `{label?}`: a new user token (answered once); the calling token is revoked, its browser sessions end and its open streams close; a browser that rotated gets a new session |
| `GET /whoami` | the caller's org, scope and seat |
| `GET /info` | no authentication: `version`, `public`, `registration`, `retention_days`, `base_path` (the path prefix as this request reached the server: `/` or e.g. `/amail`) |
| `POST /register` | no authentication, public mode: `{invite, org, user_name?}` → `{org, user_seat, token}` (201); 403 `invalid_invite`, 409 `org_exists` (at most 3 times per invite; then the invite is spent and gets 403), 422 `invalid_name`/`reserved_name`, 429 when the client address is over its limit |

### Web interface and nodes

| | |
|---|---|
| `POST /session`; `DELETE /session` | sign a browser in with `{token}` (user or operator tokens; needs `X-Amail-Csrf: 1`) / out |
| `GET /overview` | user tokens: agent seats per state, `starting` (seats counted as offline whose session waits at a trust, login or permission prompt), seats needing a person, messages the org sent or received in the last hour and day (counted per minute, so a window starts at the beginning of its first minute; another server instance's counts appear within about a second), the user's unread count, the size of the attention queue |
| `GET /counts` | user tokens: per seat (or `seat=`), `unread` and `pending` |
| `GET /attention` | user tokens: what waits for the user ([concepts](concepts.md#the-users-queue-needs-you)): first the open requests (`kind: "request"`, with `request`; `now` first, oldest first, at most 500), then the entries from the user seat's newest 5,000 deliveries and the seats that need a person, as before |
| `GET /pending` | node and user tokens: per seat, pending deliveries by class, urgent count, digest counts, `focus_until`, the seat's counter |
| `POST /node/notices` | node tokens: `{node?, notices: [{kind: "settings", changes: [{seat?, setting, old?, new}]} \| {kind: "agent_actions", seat?, actions: [{op, targets, count}]} \| {kind: "turn_failures", seat, count, harness?, error}]}` (the mail of a report with `turn_failures` names their seats of the org in its meta field `turn_failures`, separated by spaces); `setting` is `local_policy`, `permission_mode` or `git_access` with one of its values; `op` is `up`, `down`, `forget`, `run`, `wake`, `sleep`, `credentials` or `hook`. Each report becomes one `system` mail to the org's user seat, written by the server (at most 4 KB; what does not fit is counted); answers `{sent}` (1, or 0 when the org is over its daily notice budget and the report was dropped). Limits in [security](security.md#resource-limits) |
| `POST /presence` | node tokens: `{node?, seats: [{seat, state, wake_failed?, harness?, since?, attention?, attention_hint?, terminal_url?, turn_failed?}]}`; `turn_failed` is `{reason, at, count, retrying}` (the seat's last `count` turns failed; `retrying: false` once the node stopped delivering their mail again) (at most 5,000 seats); valid for 90 s. A report without `turn_failed` for a seat whose stored entry had one marks read the user's unread `turn_failures` notices that name the seat, once no seat they name has a turn failure (among the user's newest 200 unread deliveries) |
| `GET /seats/{name}/launch` | node and user tokens: harness, model, role, parent, permission mode (informational; the node does not use it) and a new seat token; the token of the seat's previous launch is revoked |
| `POST /seats/{name}/sleep`, `POST /seats/{name}/wake` | user tokens: ask the node hosting the seat; 202, or 409 `node_offline` |
| `POST /seats/{name}/prompt` | user tokens: `{text}` (not empty: 422 `prompt_empty`; at most 64 KiB: 422 `prompt_too_large`) is given to the seat's session as a prompt from the user. The server writes the seat's worklog entry `prompt_from_user` and a system mail `Prompt from <user> to <seat>` with the text, delivered to the seat in `Cc` as an archive delivery (it notifies nobody; the org's mail archive keeps it), then sends the node a `prompt` command. Answers `{queued: true, seat, nodes}`; 409 `no_node` when no connected node reports the seat; 422 `not_an_agent` |
| `POST /seats/{name}/interrupt` | user tokens: stop the seat's current turn (Codex, dsh). 409 `no_structured_interrupt` for a Claude Code seat (use its terminal); otherwise the worklog entry `interrupt` and an `interrupt` command to the node. Answers and errors as for `prompt` |
| `GET /seats/{name}/transcript` | user tokens: the seat's conversation as server-sent events: `status`, `reset`, `events`. The stream ends when the watcher falls too far behind (reconnect to start again); HTTP 503 `mirror_full` when the server has no room for another watcher |
| `POST /seats/{name}/transcript` | node tokens: `{node?, reset, status?, events}` for the watchers of the seat; at most 1 MiB (HTTP 413 `too_large` beyond) |

### Operator (operator tokens)

| | |
|---|---|
| `GET /operator/stats` | orgs, suspended orgs, seats, nodes online, mail in the last 5 minutes and hour, and per org: seats, nodes, messages (total, hour, day), `stored_bytes`, `kept_bytes`, creation time, last activity, suspension |
| `POST /operator/orgs/{name}/suspend`, `/resume` | suspend (tokens and sessions stop working, open streams end, data is kept) / resume |
| `DELETE /operator/orgs/{name}` | `{"confirm": "<name>"}`: delete the org with all its data |

### Archive and search pages

Both answer the messages the org can see (its own and those delivered to
it), newest first, without bodies, in pages of at most 2 MiB. Pass
`next_before_id` as `before_id` to continue; when it is absent, nothing
older matches. A page can be short, or even empty, and still have
`next_before_id`: each request looks at no more than 50,000 of the org's
messages, starting below `before_id`, and starts no further step of
messages after 100 ms; then it stops.

Archive filters: `from` and `to` take a seat (`name`) or another org's
address (`name@org`). For a seat of the org, `from` is its sent mail and
`to` its mailbox (also what reached it through lists); these read that
seat's mail directly. For another org's address, `to` matches the messages
that name that address in To or Cc, not those that reached its seats
through that org's lists. `thread` looks at the thread's newest 50,000
messages. `since` and `until` are RFC 3339 times.

### Search

`q` is split on whitespace and every term must occur in the subject or the
body, as a case-insensitive substring, in any script (Chinese included), at
most 10 terms. On SQLite only ASCII letters are compared without case. A
request looks at the 50,000 most recent messages before `before_id` (2,000
when a term is shorter than three characters; `scan_limit` says which), so
a search that finds few matches continues with `next_before_id`. Each hit
has a snippet of the body around the first match. `mode` is always
`"scan"`. A request also stops after 100 ms of
looking (in steps of at most 5,000 messages and 4 MiB of body text divided
by the number of terms) and answers with `next_before_id`, so a search of a
large org may take several requests. Searches take one of the org's query
slots (see [below](#limits-on-expensive-requests)).

### Limits on expensive requests

Requests whose database cost depends on the data take a query slot of
one of three pools: search and `GET /archive/messages` (search pool);
`GET /threads/{id}` (after the check that the caller sees the thread),
`/messages/{id}/status`, `/changes`, `POST /inbox/read`, `/inbox` and `/sent` pages of more
than 100, the delivery counts of a `/sent` page, `GET /tasks/board` and `GET /tasks?text=` (other pool); `/attention`, `/overview`, `/lists` and
`/lists/{name}` (light pool). On a public server an org has 1 slot in each
pool, and the search and other pools have 8 slots each over all orgs
(`--max-org-queries`, `--max-queries`); the light pool has no limit over
all orgs. A request that finds no free slot within 1 second gets 429
`too_many_queries` with `retry_after_ms`. A freed slot goes first to an
org that holds none and was served least recently. A query that runs
longer than 10 seconds is cancelled (503 `query_timeout`).

### Webhook

With `webhook_url` set, every new message delivered to the org's user is
announced by a `POST` of `{"event":"message","org","seat","message":<header>}`
(never the body), up to three attempts in the background, never delaying
the send. See [security](security.md#webhooks-and-outgoing-requests) for
the address restrictions.
