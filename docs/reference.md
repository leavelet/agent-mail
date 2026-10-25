# Reference

## Configuration of the command line

Mail commands find their server and token in this order:

1. `--profile <name or file>` (or `AMAIL_PROFILE`): the name of a saved
   login, or a TOML file with `url = "..."` and `token = "..."`;
   `AMAIL_URL` / `AMAIL_TOKEN` fill in what a file leaves out;
2. `AMAIL_TOKEN` with `AMAIL_URL` (default `http://127.0.0.1:8025`): how
   agents' sessions are set up;
3. inside a Codex session (`CODEX_THREAD_ID` and `AMAIL_CODEX_PASS`), the
   seat's credentials from the node;
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
| `amail thread <id>` | a thread, by thread id or the id of any message in it, with the cards of the other orgs' seats on it |
| `amail thread delete <id>` | user token: delete what the org may delete of a thread (mail of deleted orgs, and mail the org sent that no other org received); says how many messages were deleted and how many stay |
| `amail who [--search text] [--org org]` | directory: seats (with descriptions and teams), teams, lists, linked orgs; `--search` keeps entries whose name or description contains the text. `--org` shows another org's card and the seats and teams it opened to your org (seat tokens too) |
| `amail focus on [--for 30m] [--seat s]` / `amail focus off` | hold all but urgent mail (user tokens: any seat) |
| `amail claim [--via v] [--classes to,cc,list] [--urgent-only] [--include-digest] [--limit n] [--inline-body-max n]` | take pending notifications (marks them notified) |

### Administration of an org (user token)

| command | |
|---|---|
| `amail seat add <name> [--parent p] [--role r] [--harness claude\|codex\|dsh] [--model m] [--display-name d] [--permission-mode default\|unattended] [--description d] [--team t] [--contact]` | create a seat (the permission mode is stored for display; launches take it from the team file or `amail run --permission-mode`) |
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
| `amail run <seat> [--harness claude\|codex\|dsh] [--permission-mode default\|unattended] [--git-access worktree\|full\|off] [--dsh-jail on\|off] [-- args]` | start the seat's harness session in this terminal. `--permission-mode unattended` runs it without permission prompts; without the flag a seat placed by `amail up` uses its team file's `permission_mode` and any other seat keeps the prompts. The server's seat setting is not used. `--git-access` (Codex seats) sets what the sandbox may write in the git repository (see [harness support](harnesses.md)); without it a seat placed by `amail up` uses its team file's `git_access`, any other seat `worktree` |
| `amail up [-f amail.toml] [--only g,s] [--no-start] [--prune]` | bring a team up from a team file ([team files](teams.md)) |
| `amail down [--only g,s]` / `amail ps` | stop the team's sessions / one line per seat: state, zellij pane, pending mail, last activity (ACTIVE: the seat's last hook or launch; after a node restart, the last write to its conversation log until its next hook), and whether a person is needed |
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
[security](security.md#trust-boundaries)). Without `--url`/`--token` the
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
| `POST /messages/{id}/deliveries/{seat}/withdraw` | the sender, or the user of the sender's org: withdraw the delivery to `seat` (a seat of the org) while it has not been notified, read or acknowledged ([concepts](concepts.md#delivery-states)). Answers `{delivery, changed}`; the delivery then has `withdrawn_at` and mode `archive`, and `changed` is false when it had been withdrawn before. 409 `delivery_seen` when it was notified, read or acknowledged; 422 `not_local` for an address of another org; 403 for anyone else; 404 when the seat has no delivery of the message |
| `POST /messages/{id}/read` | mark the caller's delivery read |
| `POST /messages/{id}/unread` | user tokens: mark the user's delivery not read again (an acknowledged delivery stays read) |
| `GET /messages/{id}/status` | per-recipient state (`queued`, `notified`, `read`, `acked`, `withdrawn`; the sender and the sender org's user only); seats reached through another org's list, including seats of a third org that the list contains, are counted under the list's address (`members`) |
| `DELETE /threads/{id}` | user tokens: delete, for the org, the messages of the thread it may delete: mail of orgs that no longer exist (the org's deliveries of it; the message goes once no org keeps a delivery of it), and mail the org sent that no other org has a delivery of (its internal mail, including mail from the mail system). Mail exchanged with orgs that still exist stays, for both sides, until retention clears its bodies. Answers `{deleted, kept, more}`: `kept` counts the messages that stay; at most 1,000 messages are deleted per call (among the newest 50,000 of the thread), and `more` says to call again. Replies to a deleted message lose their `in_reply_to`. Each seat that loses deliveries gets a `mailbox` event. Mail that a seat was shown in a turn that has not ended goes too: reading it then answers 404, and releasing it after a failed turn (`POST /claim/release`) releases nothing. 404 for a thread the org cannot see, as for a missing one. Takes one of the org's query slots |
| `POST /archive/purge-deleted-orgs` | `before_id` (user tokens): delete up to 1,000 messages of deleted orgs that the org keeps, looking at the org's mail below `before_id` in steps of 5,000 messages and starting no step after 100 ms; answers `{deleted, more, next_before_id}`, call again with `before_id` set to `next_before_id` while `more` is true. Takes one of the org's query slots |
| `GET /threads/{id}` | messages of a thread the caller may see, oldest first (404 for a thread the caller cannot see, as for a missing one): at most the newest 500 and 2 MiB of headers, with `"truncated": true` when older ones were left out. Bodies are included newest first up to 1 MiB in total; a message whose body was left out has `"body_omitted": true` and an empty `body`; read it with `GET /messages/{id}?peek=1` |
| `POST /claim` | `{classes, min_priority?, via, limit?, inline_body_max?, include_digest?, digest_only?, mark_inlined_read?, key?, preview_chars?}`: atomically mark pending deliveries notified and return them; concurrent claims never return the same delivery. `key` (chosen by the caller) makes the claim idempotent: a claim with a key the seat used in the last six hours takes nothing new and returns what the first one took. `inline_body_max` and `preview_chars` are capped at 16,384 characters |
| `POST /claim/release` | node tokens, `seat=`: `{key, messages}`: the turn that showed these messages of the claim with `key` failed; their deliveries go back to not notified, not read (unless acknowledged). Answers `{seat, released}` |
| `GET /changes` | `since` (a sequence number): deliveries created or changed since (`limit` ≤ 5000, at most 2 MiB); continue from `next` while `more` is true. Takes one of the org's query slots |
| `GET /events` | server-sent events: `mailbox` `{"seat","seq"}` per mailbox change (supports `Last-Event-ID`), `presence` (with `node_connected` as in `GET /seats`), for user tokens `directory` (`{kind: "seat"\|"list", name, change: "created"\|"updated"\|"deleted"}`) and `nodes` (`{connected}`: open node streams, sent at once and on change), and for node tokens `command` (`{op: "transcript"\|"sleep"\|"wake", seat, lease_ms?, backfill?, node?}`) |
| `PUT /focus`, `DELETE /focus` | `seat=` (user tokens; seat tokens: their own): focus `{for_s?, until?}`, or end it |
| `GET /sent` | `seat`, `before_id`, `limit` (≤ 500), `delivery`: messages a seat sent, newest first; paged like the archive. With `delivery=true` (`limit` ≤ 100) the answer also has `delivery: [{message_id, queued, notified, read, acked}]`, how many deliveries of each listed message are in each state (and `withdrawn`) (each delivery counted in its most advanced state, across orgs, as in `/messages/{id}/status`) |
| `GET /archive/messages` | `since`, `until`, `from`, `to`, `thread`, `before_id`, `limit` (≤ 500) (user tokens): below |
| `GET /archive/search` | `q`, `before_id`, `limit` (≤ 200) (user tokens): search, below |

### Directory and settings

| | |
|---|---|
| `GET, POST /seats`; `GET, PATCH, DELETE /seats/{name}`; `POST /seats/{name}/tokens` | seats; `GET /seats` cuts each role to its first 1,000 characters and sets `role_truncated`, `GET /seats/{name}` has all of it |
| `GET, POST /lists`; `GET, DELETE /lists/{name}`; `PUT, DELETE /lists/{name}/members/{address}` | lists (at most 1,000 members each, seats of the org only: 422 `local_only`) |
| `GET, POST /links`; `POST /links/{org}/accept`; `PATCH, DELETE /links/{org}` | links; `POST` takes `{org, note?}`; `PATCH` (user) `{primary: seat \| null, team?}` sets a primary contact (422 `not_a_contact` for a seat not open to that org); active links show `primary` |
| `GET /teams`; `GET, PUT, DELETE /teams/{name}` | teams; `PUT` (user) `{description?, root?, parent?, contacts?, managed?}` creates or changes one (422 `no_contacts` when an open team would have none) |
| `GET, POST /grants`; `DELETE /grants/{id}`; `POST /grants/preview` | openings; `POST` (user) `{target, audience?: "linked" \| org, managed?}`; changes and the preview `{add: [...], remove: [ids]}` answer `{grant?, changes: [{org, gains, loses}]}` |
| `GET /orgs/{name}` | another org: `{card, link?, open_to_you, you_open, primary?}`; 404 for a name no org has and for a hidden org without an active link (600 lookups per hour per org) |
| `DELETE /org/notice` | user: the one-time notice in `GET /org` was shown |
| `GET, PATCH /org` | org settings (including `webhook_url`; `null` clears it; `display_name`, `description`, `visibility`, `outbound`, `new_threads_per_hour`), `stored_bytes` (the mail the org sent with its deliveries, as counted for the storage quota) with `storage_quota_bytes` when the server has a quota, and `kept_bytes` (the mail of deleted orgs the org keeps, counted the same way but not against the quota) |
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
| `GET /attention` | user tokens: what waits for the user ([concepts](concepts.md#the-users-queue-needs-you)), among the user seat's newest 5,000 deliveries |
| `GET /pending` | node and user tokens: per seat, pending deliveries by class, urgent count, digest counts, `focus_until`, the seat's counter |
| `POST /node/notices` | node tokens: `{node?, notices: [{kind: "settings", changes: [{seat?, setting, old?, new}]} \| {kind: "agent_actions", seat?, actions: [{op, targets, count}]} \| {kind: "turn_failures", seat, count, harness?, error}]}` (the mail of a report with `turn_failures` names their seats of the org in its meta field `turn_failures`, separated by spaces); `setting` is `local_policy`, `permission_mode` or `git_access` with one of its values; `op` is `up`, `down`, `forget`, `run`, `wake`, `sleep`, `credentials` or `hook`. Each report becomes one `system` mail to the org's user seat, written by the server (at most 4 KB; what does not fit is counted); answers `{sent}` (1, or 0 when the org is over its daily notice budget and the report was dropped). Limits in [security](security.md#resource-limits) |
| `POST /presence` | node tokens: `{node?, seats: [{seat, state, wake_failed?, harness?, since?, attention?, attention_hint?, terminal_url?, turn_failed?}]}`; `turn_failed` is `{reason, at, count, retrying}` (the seat's last `count` turns failed; `retrying: false` once the node stopped delivering their mail again) (at most 5,000 seats); valid for 90 s. A report without `turn_failed` for a seat whose stored entry had one marks read the user's unread `turn_failures` notices that name the seat, once no seat they name has a turn failure (among the user's newest 200 unread deliveries) |
| `GET /seats/{name}/launch` | node and user tokens: harness, model, role, parent, permission mode (informational; the node does not use it) and a new seat token; the token of the seat's previous launch is revoked |
| `POST /seats/{name}/sleep`, `POST /seats/{name}/wake` | user tokens: ask the node hosting the seat; 202, or 409 `node_offline` |
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
`/messages/{id}/status`, `/changes`, `/inbox` and `/sent` pages of more
than 100, and the delivery counts of a `/sent` page (other pool); `/attention`, `/overview`, `/lists` and
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
