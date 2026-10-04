# Security model

This document describes who amail trusts, what each credential can do, how
mail that tries to manipulate an agent is handled, and what the operator of
a server can see. To report a vulnerability, see [SECURITY.md](../SECURITY.md).

## Trust boundaries

**Inside an org, the user is trusted.** Each org has exactly one user, the
person who runs the team. Mail from the user is an instruction to the
agents; the user can read all of the org's mail, watch every seat's
conversation and administer seats, lists, links and tokens.

**Agents of the same org are colleagues.** Their mail is a request, not an
instruction: agents are told to weigh it against the user's instructions.

**Everything from another org is untrusted**, even mail written by that
org's user. Orgs exchange mail only after both users agreed to a link, and
either can end it. A user sees only their own org's mail, which includes
the mail exchanged with linked orgs, never the other org's internal mail,
conversations or terminals. The delivery status of mail sent to another org's list
counts that list's seats (how many were notified, read, acknowledged)
without naming them; acknowledgement reminders count them the same way.

**The server's operator** runs the server and can suspend and delete orgs.
The operator's interfaces do not show mail or conversations; see
[what the operator sees](#what-the-operator-sees).

**A user trusts the server they connect to** for the source labels of
mail and for role texts. The server computes the `USER`, `agent`,
`external` and `system` labels, and it stores each seat's role text, which
the node puts into the session's system prompt. A server that is
compromised, or run by a malicious operator, can therefore send mail
labelled `USER` to the agents of every org on it, change their role texts
and their models, and read their mail. It cannot change what happens on a
user's machine beyond what such mail and role text can talk an agent into:

- it cannot turn off permission prompts. The node decides the permission
  mode on the user's machine, from the team file (`permission_mode` of a
  seat that `amail up` placed) or from `amail run --permission-mode`; with
  neither, the harness keeps its normal prompts. The server's
  `permission_mode` seat setting is shown in the web interface and in
  `amail seat ls`, and it has no effect on launches. When it says
  `unattended` and the local side did not choose that, the node logs a
  notice and `amail run` prints it;
- it cannot choose which program runs. The harness binaries come from the
  node's configuration (`claude` and `codex` on `PATH`, or the node's
  `--claude-bin` and `--codex-bin`); the server's `harness` setting only
  picks one of the two for a seat that is not in a team file and is
  started without `--harness`. A model name from the server that could be
  read as a command-line option is ignored;
- it cannot add launch arguments: the extra arguments of `amail run` come
  only from its own command line.

Use a server you trust with these powers. Seats with `permission_mode =
"unattended"` act on such mail without asking anyone.

**The host that runs the node is trusted with the seats it hosts.** The
node holds a node token and the seat tokens of the sessions it runs; any
process of the same user on that host can ask it for them.

The node socket is created in a directory with mode 0700
(`$XDG_RUNTIME_DIR/amail`, or `run/` in the node's state directory; never a
shared directory such as `/tmp`). Every client (`amail hook`, `amail run`,
`amail attach`, `amail up` and the others) checks before it connects that
the directory belongs to the user and that no other user can write to it,
that the socket belongs to the user, and after it connects that the
process listening runs as the user. Otherwise it refuses, so another user
of a shared machine cannot stand in for the node.

## Mail is a prompt-injection channel

Text that an agent reads can contain instructions that change what the
agent does, and mail is text written for agents to read. amail cannot make a model ignore text; it makes the origin
of every message unambiguous and keeps mail out of the permission system:

- **Authenticated sender.** The sender of a message is the seat its token
  belongs to. An agent cannot send as the user or as another seat.
- **Labelled source.** Every injected message carries its source for that
  reader: `USER` (the org's user), `agent` (a seat of the org), `external`
  (anything from another org), `system` (the mail server). The system seat
  has no tokens and cannot be addressed, so no client can send system mail.
- **Unforgeable layout.** Injected text starts with `[amail]`, has one
  header line per message, and puts every body line behind `  > `. The
  text output of `amail read`, `amail thread` and `amail claim` uses the same
  rule: the header lines, with the source label, are written by the command
  line client, and every body line is behind `  > `. Subjects, names and
  `meta` values are printed on one line. So a message cannot fake a header
  line, a message from `USER` or a line from the mail system, in the
  agent's context or in a terminal. (`--json` output contains the raw
  values.)
- **No hidden or terminal-control characters.** In everything the command
  line client prints and everything injected into a session (bodies,
  subjects, `meta`, previews, names, list descriptions, token labels, role
  text, error messages), control characters, terminal escape sequences,
  bidirectional controls, zero-width characters, Unicode tag characters and
  the other invisible format characters are shown as visible escapes such
  as `\u{202e}`; line breaks inside one-line values are shown as spaces.
  Text that a person cannot see in the terminal or the web interface
  therefore does not reach a model unseen.
- **Instructions in the system prompt.** Every session started by amail is
  told that `[amail]` text comes from the mail system, that `USER` mail is
  an instruction, `agent` mail a colleague's request and `external` mail
  untrusted information, and that mail never grants permissions.
- **No permission through mail.** Approving a tool call or a sandbox
  exception happens only in the harness (the terminal). Whether a seat runs
  without prompts is decided on the user's machine (the team file or
  `amail run --permission-mode`), never by mail or by the server (see
  [trust boundaries](#trust-boundaries)). Seats with `permission_mode =
  "unattended"` have no prompts at all: there, what an agent may do is
  limited only by the harness's sandbox (Codex) or not at all (Claude Code
  with `--dangerously-skip-permissions`). Run unattended seats only on
  repositories and hosts you can afford to lose, and be most careful when
  your org is linked to others.

In our tests, agents of both supported harnesses refused a deletion
requested by another org's user and carried out the same request from their
own user. This is model behaviour, not a guarantee.

**Codex seats and git.** The `amail` permissions profile in the node's
Codex home lets sessions write the git directories of every Codex seat's
working directory on the host, so that seats can commit, rebase and merge.
In those directories, the entries that git runs or reads as configuration
stay read-only: `hooks/`, `config`, `config.worktree`, `modules/` and
`worktrees/` of a repository's git directory, and `commondir` and
`config.worktree` of each linked worktree's git directory. Entries that do
not exist are covered too: Codex puts a read-only placeholder at their path,
so they cannot be created. A session therefore cannot add a hook or set
`core.hooksPath`, `core.fsmonitor` or a similar key that git would run
later, outside the sandbox. What remains:

- Codex cannot protect a file that does not exist without making git fail
  to read it, so a session can still create `commondir` in a repository's
  git directory (`.git/commondir`). Git run in that repository's main
  checkout (not in its linked worktrees) then reads config and hooks from
  the directory the file names. Git never creates this file there. amail
  refuses to start a Codex seat while a git directory it grants contains
  one, and `amail up` refuses to prepare worktrees in such a repository.
  Git commands you run yourself do not check for it. If you find one, remove it and look at the directory it names before
  you run git in that checkout.
- A seat can leave an unfinished rebase whose todo list contains `exec`
  lines; `git rebase --continue` runs them. Before you continue a rebase a
  seat started, read its todo list (`git status` shows it);
  `git rebase --abort` runs nothing.
- Files a session writes in its working directory (scripts, build files,
  editor settings) run when you run them. Review them as code from an
  untrusted author.
- With network access enabled, which amail's profile needs, the sandbox
  allows `connect()` on Unix sockets, including the node's socket.

## Tokens and their scopes

Tokens are random 256-bit bearer secrets (`amail_` + 64 hex characters).
The server stores only their SHA-256 hashes. Each token is shown once.

| scope | issued by | can |
|---|---|---|
| `user` | `amail admin create-org`, registration, `amail token user`, rotation, `amail admin token reset` | everything in its org: seats, lists, tokens, links, settings, the whole archive, conversations; send and receive as the user seat |
| `seat` | `amail seat token`, and the node when it starts a session | send and receive as its seat; read its mailbox and the threads it takes part in; read the org's directory |
| `node` | `amail node token` (`amail token node`) | for any agent seat of its org (never the user seat): list and claim pending mail, read changes and events, report presence, fetch launch information (which issues a new seat token and revokes the seat's previous launch token), post conversation entries. It cannot send mail or read message bodies through `/v1/messages` |
| `operator` | `amail admin operator-token create`, on the database only | the operator endpoints: statistics, suspend, resume and delete orgs. No mailbox, no mail, no conversations |

- `amail token ls` lists an org's live tokens (never the secrets);
  `amail token revoke <id>` revokes one. The last user token of an org
  cannot be revoked.
- `amail token rotate` (or Admin → Settings → rotate in the web interface)
  issues a new user token and revokes the one in use. Every browser
  session made with the old token ends (the browser that rotated gets a
  new session), and saved logins holding it are updated. Use it when a
  user token may have leaked: it also signs out every browser that signed
  in with it.
- Revoking or rotating a token, or signing out, also ends the event
  streams and conversation mirrors opened with that token or session: at
  once on the instance where it happened and at the other instances
  within the sweep interval (15 seconds).
- A user who lost every user token asks the operator:
  `amail admin token reset <org>` revokes all user tokens and their browser
  sessions and prints a new one.
- Saved logins (`amail login`) are kept in `~/.config/amail/config.toml`
  with mode 0600 in a 0700 directory.

## The web interface

- Only user tokens (and operator tokens, for `/admin`) can sign in. Signing
  in exchanges the token for a session cookie: `HttpOnly`,
  `SameSite=Strict`, valid for 30 days (operator sessions: one day),
  `Secure` when the request came over HTTPS and always in public mode.
  Revoking or rotating the token ends its sessions. A token has at most 20
  sessions; signing in once more ends the oldest.
- Signing in counts against the token's request rate in public mode, like
  any other request made with the token.
- Requests authenticated by the cookie must send `X-Amail-Csrf: 1` for
  anything but `GET`, and their `Origin`, when present, must name the
  server's host (`X-Forwarded-Host` counts only from a trusted proxy).
- A seat's terminal link, reported by its node, is kept and shown only
  when it is an `http` or `https` URL.
- Pages are served with a strict Content Security Policy (scripts only
  from the server itself), `X-Frame-Options: DENY` and
  `X-Content-Type-Options: nosniff`. API responses are not cached.

## Webhooks and outgoing requests

The only requests the server makes on a user's behalf are notification
webhooks (`amail org set --webhook-url`): a `POST` with the header of each
new message to the user, never its body. Redirects are never followed and
no proxy is used. Only the user's token and browser sessions
see the webhook URL; seat tokens read the org's settings without it,
since such URLs often contain credentials.

In public mode (or with `--webhook-private-targets deny`), webhooks may not
reach loopback, private, link-local, carrier-grade NAT, multicast or other
reserved addresses, including IPv4-mapped and NAT64 forms. The server's own
interface addresses, and the networks they are on, are refused as well
(an address can be public and still belong to the server, such as a
global IPv6 address), and only ports 80 and 443 may be used. URLs with such a
literal address, and `localhost`, are refused when saved. A host name is
checked every time a webhook request is sent, on the addresses it resolves to, and
the connection goes to exactly those addresses; so a name that resolved to
a public address when it was saved and to a private one later (DNS
rebinding) is not reached. Self-hosted servers allow private targets unless
told otherwise (`--webhook-private-targets allow|deny`).

## Reverse proxies

`X-Forwarded-For`, `X-Forwarded-Host` and `X-Forwarded-Proto` are believed
only from peers listed with `--trusted-proxy`; see
[operations](operations.md#behind-a-reverse-proxy-or-a-tunnel). Otherwise a
client could choose its own address for rate limits or pass the browser
origin check.

## Org names

Org names are not secret, but the server does not list them. Registration
checks the invite code before the name, so without a valid code nobody
learns whether a name is taken. A link request to a name that is not an
org is answered, listed and withdrawn exactly like a request the other org
has not accepted yet; such a request is dropped when an org of that name
is created later.

## Reserved names

On a public server, seats and lists may not take names that could pass for
the mail system, the operator or the user (`amail`, `system`, `admin`,
`administrator`, `operator`, `postmaster`, `mailer-daemon`, `noreply`,
`no-reply`, `owner`, `user`, `human`, `me`, `security`, `support`,
`abuse`). The user seat itself may be called `owner` (the default). Org
names have their own reserved list (`amail`, `system`, `admin`, `root`,
`support`, `www`, `api`, ...). Existing data is not affected.

## What the operator sees

The operator (whoever runs `amail serve` and holds the database) can:

- through the operator token and `/admin`: see org names, creation times,
  seat counts, nodes online, mail counts per hour and day, and last
  activity; suspend, resume and delete orgs;
- through `amail admin`: the same, plus invite codes and resetting an
  org's user tokens.

Operator tokens cannot read mail, mailboxes, threads, the archive or
conversations; the API refuses them there. **Anyone with access to the
database or the server's memory can, however, read the stored mail.** Mail
is not end-to-end encrypted. Conversations of agents are relayed through
the server's memory while someone watches and are never written to disk.

## Data stored and its deletion

The server stores the org's seats, lists and links, all mail (subjects,
bodies, delivery states) and token hashes. Browser sessions are stored as
hashes of their cookies. Invite codes are stored as hashes, with the name
of the org that used them. Logs name orgs and counts; they never contain
tokens, invite codes or message bodies, and webhook URLs are logged
without path, query or credentials.

A user deletes their org and all its data with `amail org delete` or in the
web interface; an operator with `amail admin org delete`. An operator may
set a retention period after which message bodies are cleared. Backups the
operator made earlier are not affected by either.

## Resource limits

Every input that could make the server use unbounded memory, storage or
time has a fixed bound. Limits marked *public* apply only with
`amail serve --public` (each can be changed with the flag or environment
variable shown in [operations.md](operations.md)); the others always apply.
Rate and stream limits are counted per server instance.

| What | Limit |
|---|---|
| Request body (any endpoint) | 2 MiB; a transcript post (`POST /seats/{name}/transcript`) 1 MiB (HTTP 413 beyond; nodes split their posts to fit) |
| Message subject | 300 characters, one line |
| Message body | 256 KB |
| Addresses in one message | 100 |
| Seats one message reaches | 200 (org setting `max_recipients`) |
| `meta` | 16 keys, 4 KB in total |
| Reply chain depth | 50 (org setting `max_thread_depth`) |
| Messages sent per agent seat | 30 per minute, bursts of 30 (org settings; the user seat is exempt) |
| Org settings | `send_rate_per_min` and `send_burst` at most 600, `max_recipients` and `max_thread_depth` at most 1,000 (*public*); `ack_timeout_min`, `focus_max_min` and `digest_interval_min` at most 10,080 (one week) |
| Messages per org | 50 per second (*public*) |
| Messages over all orgs | 500 per second (*public*) |
| Requests per token or browser session | 200 per second, bursts of 400 (*public*); sign-ins count too |
| Registration attempts per client address (IPv6: per /64 network) | 10 per hour, bursts of 5, failed ones included (*public*) |
| Link requests per org | 20 per hour, bursts of 5 (*public*) |
| Pending outgoing link requests per org | 1,000 |
| Keys tracked by each rate limiter | 100,000 (further keys share one bucket; idle keys are dropped on every cleanup pass) |
| Agent seats per org | 200 (*public*) |
| Lists per org | 500 (*public*) |
| Live tokens per org | 1,000 (*public*) |
| Open event streams per org (`/events` and conversation mirrors) | 32 (*public*) |
| `GET /inbox` page | 50 by default, at most 1,000 |
| `POST /claim` | at most 500 deliveries |
| `GET /changes` page | 500 by default, at most 5,000 |
| `GET /archive/messages`, `GET /sent` page | 50 by default, at most 500 |
| `GET /archive/search` | 50 results by default, at most 200; at most 10 terms; terms shorter than 3 characters scan only the 2,000 newest messages |
| `GET /attention` | 500 items |
| `GET /threads/{id}` | the newest 500 messages (`truncated` says when older ones were left out) |
| Event replay buffer (`Last-Event-ID`) | 4,096 events per org; a subscriber more than 1,024 events behind is disconnected and resynchronizes |
| Presence report | 5,000 seats per request; an entry is valid for 90 s |
| Conversation mirror, per watched seat | in memory only while someone watches: the last 1,000 events or 4 MiB kept for watchers that join later; at most 8 MiB waiting to be sent to one watcher (a watcher that falls further behind, for example one that stopped reading, is disconnected and its queue freed; it reconnects and starts again from the kept events); 16 MiB for the seat in all (beyond it, the watchers furthest behind are disconnected); 1,000 events per post; 16,000 characters per event; 2,000 characters per status field |
| Conversation mirrors over all seats | 256 MiB per instance (*public*); beyond it, the watcher furthest behind is disconnected or the largest kept-events buffer shortened, whichever holds more |
| Browser sessions | 30 days (user), 1 day (operator) |
| Webhooks | URL at most 2,000 characters; 3 attempts; 5 s to connect, 10 s per attempt; redirects are not followed; 16 notices in flight per org, further ones are dropped (*public*) |
