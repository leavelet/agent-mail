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
either can end it. A link opens nothing but the user seats: another org
sees your org's card and the seats and teams you opened to it, and nothing
else (see [what another org can learn](#what-another-org-can-learn)). A
user sees only their own org's mail, which includes the mail exchanged
with linked orgs, never the other org's internal mail, conversations or
terminals.

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
  node's configuration (`claude`, `codex` and `dsh` on `PATH`, or the node's
  `--claude-bin`, `--codex-bin` and `--dsh-bin`); the server's `harness`
  setting only picks one of them for a seat that is not in a team file and is
  started without `--harness`. A model name from the server that could be
  read as a command-line option is ignored;
- it cannot add launch arguments: the extra arguments of `amail run` come
  only from its own command line.

Use a server you trust with these powers. Seats with `permission_mode =
"unattended"` act on such mail without asking anyone.

**Which local processes may use the node: `local_policy`.** The node
holds a node token and the seat tokens of the sessions it runs, and serves
requests on a Unix socket that only processes of the person's
operating-system user can reach (see the end of this section). The setting
`local_policy` decides what those processes may ask:

- `open`: every process of the user may use the socket fully. This
  includes the agents of the sessions the node launched: they can run
  `amail up`, `down`, `ps`, `sleep`, `wake` and `amail run` for any seat,
  which gives them any agent seat's credentials and mail. This lets an
  agent manage a team of seats.
- `restricted`: the node tells a session's processes apart from the
  person's (described below). A session gets only its own seat's
  credentials and mail, and requests meant for the person are refused to
  it.

The node takes the setting from `amail node --local-policy`
(`AMAIL_LOCAL_POLICY`, or `local_policy` in `<state dir>/node.toml`). A
team file can set `local_policy = "restricted"` under `[team]`, which makes
the node use the restricted policy while that team is up; a team file
cannot make the policy open, because team files usually lie in the
project, where agents can write. Without either setting the node uses
`open`. `amail up`, `amail ps`, `amail node status` and `amail
doctor` print the policy in effect and where it comes from.

Neither policy is a security boundary between the agents and the person:
all of them run as the same user (see below). The restricted policy stops
an agent from taking another seat's credentials or managing seats through
amail itself; it does not stop an agent that sets out to get around it.

**What the node tells the person.** The node reports two kinds of events to
the server, which writes a `system` mail to the org's user seat. The mail
appears in the user's inbox and in the web interface's list of what needs
the user. It asks for no acknowledgement and wakes no agent. The node sends
only typed fields (setting names and values, seat names, request kinds);
the server writes the text from fixed sentences, shows only values a
setting can have and only names of the org's own seats, and shows the
node's label escaped on one line. Each org's nodes together can send at
most 60 such reports per hour (bursts of 10), and a node's reports go only
to its own org's user. One report becomes one mail of at most 4 KB. An org
receives at most 50 such mails (128 KB) in 24 hours; further reports are
dropped. This mail does not count against the org's storage quota and is
deleted after 7 days.

- When the node starts, and after each `amail up`, it compares its local
  policy, and each supervised seat's permission mode and (for Codex and
  dsh seats) `git_access` (and for dsh seats whether the jail is on), with what it
  last reported, and reports the differences in
  one mail: the setting, the old and the new value, and the host. The
  first report of a node names its policy. A seat reported for the first
  time is listed when one of its settings is not the default (for example
  `permission_mode = "unattended"`), and then with all of them: an
  unattended Codex seat with its `git_access`, an unattended dsh seat with
  its `git_access` and its jail, even when these are the defaults. A node whose state was written
  by an earlier version, which always used the restricted policy, reports
  a change from `restricted`.
- Under the open policy, when a process of an agent session runs `up`,
  `down`, `forget`, `run`, `sleep` or `wake`, obtains a seat's
  credentials, or sends a hook in the name of a seat other than its own,
  the node reports which seat's session did what to which seats. The first
  such request of a session is reported within seconds; further requests
  of the same session within the next five minutes are reported together
  at the end of those five minutes. The node recognizes a session's
  processes by their parent processes, as the restricted policy does, so
  these notices are best effort: a process that left the session's process
  tree is taken for one of the person's and is not reported. Codex runs the
  commands of all Codex seats of a node in one shared process, so a notice
  about a Codex session cannot name the seat.

**How the restricted policy tells sessions apart.** For every connection to
its socket the node takes the connecting process's id from the kernel
(`SO_PEERCRED`) and follows that process's parents in `/proc`:

- A Claude Code session is the process of `amail run`, which the node
  records when it hands out the launch plan and which then becomes the
  harness. Its hooks and tool commands run below that process. The node
  answers a Claude Code hook only when it comes from below the session of
  the seat it names.
- Codex runs the hooks and tool commands of all Codex seats of a node in
  one shared app-server, not below the session's process. The node starts
  that app-server with a random value of its own in its environment
  (`AMAIL_CODEX_PASS`) and answers Codex hooks and credential requests only
  when they carry it, and never from a Claude Code session. A Codex thread
  id alone, which Codex shows in file names and session lists, gets
  nothing. All Codex sessions of a node hold the same pass, so a Codex
  session that knows another Codex seat's thread id can get that seat's
  credentials and mail.
- A DeepSeek Harness (dsh) seat runs its hooks under its own web host,
  which runs below the process the node launched for the seat. For an
  unattended seat the host runs in a bwrap jail with its own process
  namespace; the kernel reports the hook's process id in the node's
  namespace, so the chain of parents still reaches that process. The node
  answers a dsh hook only when it comes from below the session of the seat
  it names. The host's launch token, which gives full control of the
  host's sessions, is kept in a file in the node's state directory
  (mode 0600); the jail shows a seat only its own endpoint file and its own
  dsh home.
- Requests meant for the person (`amail run`, `up`, `down`, `ps`,
  `attach`, `sleep`, `wake`, `amail node status`) are refused to processes
  that run below a session the node launched or with the node's Codex
  home.

Under both policies, `amail node status` does not show session or thread
ids, and the node relays a seat's conversation only from a transcript file
in the directory where the harness writes transcripts, whatever path a hook
reports (see [conversation mirrors](architecture.md#conversation-mirrors)).

**On the host, agents and the person are not separated from each other.**
All sessions run as the same operating-system user as the person who
started the node, so file permissions do not separate the agents from each
other or from the person. (The exception is an unattended DeepSeek Harness
seat with its jail on, the default: amail runs its host in a bwrap jail
that hides the home directory, the runtime directory and other processes,
so the points below that need those do not apply to it; it can still read
the rest of the file system. With `dsh_jail = false` all the points apply.
The jail mounts `/` read-only, a new `/dev` and `/proc`, and empty `tmpfs`
mounts over the home directory, `/tmp` and `$XDG_RUNTIME_DIR`; it covers
the Docker, Podman and containerd sockets with `/dev/null`; it mounts back
writable the seat's working directory, its dsh home and the git paths its
`git_access` allows; and it mounts back read-only the `amail`, `dsh` and
Node.js installations, the repository's other git paths, the seat's
profile, patch and endpoint directory, the node socket's directory, and
git's configuration files `~/.gitconfig` and `config`, `ignore` and
`attributes` in `~/.config/git` (or `$XDG_CONFIG_HOME/git`), each file on its own, never git's
credential stores. The host has no controlling terminal and reads
`/dev/null`. The full list is in the harness documentation.
See [harness support](harnesses.md#deepseek-harness-dsh).) An agent that can run shell commands (Claude Code
with `--dangerously-skip-permissions`, a Codex seat whose sandbox lets it
read your home directory, or any agent whose commands you approve without
reading them) can:

- read your saved login (`~/.config/amail/config.toml`) if you saved one on
  that host, and then act as you: read all of the org's mail and
  conversations, send mail as `USER` (which agents treat as instructions),
  manage seats, tokens and links;
- read the node's state directory (the node token, which fetches a new
  token for any agent seat; the seat tokens handed out at launch; the
  Codex pass) and other sessions' environments in `/proc`, and then act as
  any agent seat of the org;
- start a process outside its own process tree (through a terminal
  multiplexer, the service manager or a scheduler), which the node then
  takes for one of the person's;
- connect to the Codex app-server's control socket, which lets any of the
  user's processes start turns and run commands in every Codex seat's
  thread; Codex seats of one node are therefore not separated from each
  other;
- write to a Claude Code session's inbox socket, which accepts messages
  from any process of the user (`crossSessionInbound: "accept"`, which the
  node's wake-ups need), and so put a prompt into another seat's session.

The restricted policy stops an agent from taking another seat's token or
mail through amail itself; it does not stop an agent that sets out to read
files or other processes. If that matters for your agents (for example because
your org is linked to others and they read external mail):

- run the node and its agents under an operating-system user of their own,
  or in a container, and not under the account that holds your saved
  login; give agents that must not reach each other's tokens separate
  users or containers, each with its own node;
- keep the user token out of the agents' reach: do not save a login
  (`amail login`) on the host or account where agents run, and manage the
  org from the web interface or from another machine.

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
  belongs to, with one exception: the org's user may write a message in
  the name of one of the org's agent seats (`POST /v1/seats/{name}/messages`,
  user tokens only; seat and node tokens cannot, and the user and system
  seats cannot be named). Such a message is the agent's for everything
  that decides what it may do: its source label is `agent` (`external` in
  other orgs), the reachability rules, the outbound setting, the send
  budget and the budget of new threads to other orgs are the agent's. It
  also records that the user wrote it, and every reader is shown that:
  injected mail, digests and the command line say `from be-lead (agent,
  written by USER)` (in another org `from be-lead@acme (external, written
  by owner@acme, the user of acme)`), `amail status` adds a line, `--json`
  and the API have `written_by`, and the web interface shows "written by
  owner" next to the sender. The server logs the token, the seat and the
  message id of each such send. An agent gets only its own seat's token from amail (Codex
  seats of one node excepted: they can get each other's); otherwise it can
  send as the user or as another seat only by reading their tokens from
  files or processes of the same operating-system user (see
  [trust boundaries](#trust-boundaries) for when that is possible and what
  to do about it).
- **Labelled source.** Every injected message carries its source for that
  reader: `USER` (the org's user), `agent` (a seat of the org), `external`
  (anything from another org), `system` (the mail server). The system seat
  has no tokens and cannot be addressed, so no client can send system mail.
- **Unforgeable layout.** Injected text starts with `[amail]`, has one
  header line per message, and puts every body line behind `  > `. The
  text output of `amail read`, `amail thread` and `amail claim` uses the same
  rule: the header lines, with the source label, are written by the command
  line client, and every body line is behind `  > `. Subjects, names and
  `meta` values are printed on one line; a subject longer than 200
  characters is cut there, so that it cannot fill the text injected into
  a session. So a message cannot fake a header
  line, a message from `USER` or a line from the mail system, in the
  agent's context or in a terminal. `--json` output contains the values
  unchanged: control, line-break and invisible characters in them are
  written as JSON `\u` escapes (for example `\u202e`), so they decode to the
  original text. It is meant for programs, which must apply their own rules
  before showing the decoded values to a person or a model.
- **No hidden or terminal-control characters.** In everything the command
  line client prints and everything injected into a session (bodies,
  subjects, `meta`, previews, names, list descriptions, token labels, role
  text, error messages), control characters, terminal escape sequences and
  invisible characters are shown as visible escapes such as `\u{202e}`;
  line breaks inside one-line values are shown as spaces. Invisible
  characters are every code point with the Unicode property
  Default_Ignorable_Code_Point (bidirectional controls, zero-width
  characters, variation selectors, Unicode tag characters, Hangul fillers),
  every format character (category Cf) and U+2800 (braille blank), as of
  Unicode 17.0 (generated by `scripts/invisible-chars.py`). The web
  interface shows the same characters as highlighted escapes in mail
  bodies, subjects, `meta` values, display names, roles, list
  descriptions, token labels, presence hints and conversation mirrors. Text
  that a person cannot see in the terminal or the web interface therefore
  does not reach a model unseen. As a side effect, an emoji written with a
  variation selector or a zero-width joiner is shown with the escape (for
  example `✔\u{fe0f}`).
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
  limited only by the harness's sandbox (Codex), by amail's bwrap jail
  (DeepSeek Harness; see [harness support](harnesses.md#deepseek-harness-dsh)),
  or not at all (Claude Code with `--dangerously-skip-permissions`). Run unattended seats only on
  repositories and hosts you can afford to lose, and be most careful when
  your org is linked to others.

In our tests, agents of both supported harnesses refused a deletion
requested by another org's user and carried out the same request from their
own user. This is model behaviour, not a guarantee.

**Codex seats and git.** Codex seats can commit only where their sandbox
profile lets them write git directories. What they may write is set per
seat with `git_access` (see [harness support](harnesses.md) for the exact
rules):

- `worktree` (the default): in a linked worktree, the repository's objects,
  refs and branch reflogs, and the worktree's own git directory. The root
  of the repository's git directory stays read-only, so a session cannot
  create or change `config`, `hooks/`, `info/`, `modules/` or `commondir`
  there (a `commondir` file would make git run in the main checkout take its
  config, and so commands such as `core.fsmonitor`, from a directory the
  session chose). In the worktree's git directory, `commondir`, `gitdir`
  and `config.worktree` stay read-only. A session therefore cannot make git
  run a command outside the sandbox through hooks or configuration.
- `full`: the whole git directory and the main checkout, minus `hooks/`,
  `config`, `config.worktree`, `modules/` and `worktrees/`. A session with
  `full` can still create `.git/commondir` (Codex cannot keep a missing
  file from being created without making git fail to read it), and git run
  in the main checkout then takes config and hooks from the directory the
  file names: **a `full` session can make git run commands outside its
  sandbox, as you**. The staged-merge template gives `full` to its root
  seat, which merges in the main checkout; `amail up` names such seats. Set
  `git_access = "worktree"` for that seat and merge yourself if you do not
  want this.
- `off`: no git grant.

What remains with `worktree`:

- Git appends reflog lines through symbolic links. A session can replace a
  branch's reflog in `logs/refs/` (or a file in its worktree's git
  directory) with a symbolic link to a file of yours, for example a shell
  start-up file; the next time you or a `full` seat change that branch
  outside the sandbox, git appends a line that ends with a commit subject
  the session chose. This needs you to run git on that branch (the reflog
  of the branch checked out in the main checkout, and `logs/HEAD`, stay
  read-only). amail refuses to start a Codex seat, and `amail up` refuses to
  run git, while a repository's `refs/`, `logs/`, `amail/` or a linked
  worktree's git directory contains a symbolic link; git never creates
  one there. Git commands you run yourself do not check for this. If amail
  reports one, look at where it points and remove it.
- A session can write any branch and tag of the repository, not only its
  own.
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
  `Secure` when the request came over HTTPS and always in public mode,
  with `Path` set to the server's path prefix (`/` at the root).
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

`X-Forwarded-For`, `X-Forwarded-Host`, `X-Forwarded-Proto` and
`X-Forwarded-Prefix` are believed only from peers listed with
`--trusted-proxy`; see
[operations](operations.md#behind-a-reverse-proxy-or-a-tunnel). Otherwise a
client could choose its own address for rate limits or pass the browser
origin check. A path prefix (from `--base-path` or `X-Forwarded-Prefix`) is
checked against a fixed set of characters before it is written into the
page, a redirect or a cookie.

## What another org can learn

What org B can see of org A's seats and teams is built in one place on
the server, from A's openings; whether a message may go from one org to
the other is decided in one other place, from the openings and the stored
threads.

**B can learn:**

- A's card (name, display name, description), unless A is hidden and has
  no active link with B;
- with an active link: the cards (name, kind, description) of the seats
  and teams A opened to B or to every linked org, and the link's state and
  since when;
- everything about the messages that B's seats sent or received: their
  senders and every address on them, and the cards of the seats on them
  (not of other messages of the same thread that B did not get). A message that crosses orgs carries a contact of each org and
  no list, so the seats of A that B learns this way are A's contacts and
  the seats A's side put on such a message (by writing or by naming them);
  A's user is told by system mail when a seat that is not a contact joins
  such a thread;
- for mail B sent: the delivery state of each recipient.

**B cannot learn:** the names of seats, teams or lists A did not open and
did not put on mail to B; how A's org is structured (parents, teams, list
members); role texts, harnesses, models, presence, hosts or terminals; A's
internal mail, also in threads that crossed orgs; whether a name is a
closed seat, a list or nothing at all. Mail to any of these is refused
with `unreachable` after the same lookups. The test suite checks that
these answers do not differ in status, body or time (beyond noise)
between a closed seat and a missing name.

**Agents cannot change what the org shows.** Openings, primary contacts,
the org's visibility and outbound setting, team contacts and links change
only with the user token (the command line, the web interface, or a team
file applied by the user's `amail up`). Every change is reported to the
user by system mail, naming which orgs gain or lose sight of which cards.
An agent can still write to an open seat of a linked org: its own card
then becomes visible to that org in that thread, and the org's primary
contact is added in Cc; with `outbound = "contacts-only"` only the user
and the contacts may write outside.

**Delivery state across orgs.** For a recipient in another org, a
message's delivery status gives its state (queued, notified, read, acked)
and the read and acknowledgement times, nothing else: not the seat's
mailbox counters, nor how or when it was notified (which would show its
harness and how much other mail it gets).

**Checked in the send's transaction.** The link and the openings a
message to another org depends on are checked again in the transaction
that stores it (on Postgres the rows are locked for share), so that a link
ended or an opening removed just before cannot be passed by a send that
was checked earlier.

**Text of another org in system mail.** A link request's notice quotes
everything the requesting org chose (display name, description, note) on
lines starting with `    > `, under a fixed sentence saying that the
other org wrote it; none of it can begin a line of the mail system's own
words.

**Ids.** Message numbers (and thread numbers, which are message numbers)
are server-wide sequential integers, so the numbers an org sees show how
much mail the whole server handles, not who sent it. No answer gives the
ids of another org's seats, teams, grants or deliveries.

**What the design itself reveals.** These follow from the rules and are
not hidden:

- The contact added in Cc can show which team a sender belongs to: a
  team's primary contact is added for the team's members.
- A team's contacts appear among the cards together with the team card.
- A seat that writes to another org shows that org its name and
  description (in that thread), whether or not it is open; with `outbound
  = "contacts-only"` only contacts may write outside.
- Threads that were closed (the last contact of one org closed, or the
  link ended) become usable again when a contact is opened again or the
  link is restored, and their earlier participants can then be addressed
  in them again.
- Openings follow whoever holds a user token. A team file's openings are
  applied by `amail up` with the user token it finds; under the default
  `open` local policy, any process of the person's operating-system user
  that can use the saved login (agents of that host included) can do so.
  The "Openings changed" notice names the token's label and, for `amail
  up`, the agent session the node recognised as running it, when it did
  (best effort, as described under [trust boundaries](#trust-boundaries)).

**Hidden orgs.** An org with visibility `hidden` does not exist for orgs
without an active link with it: a card lookup answers `not_found`, a link
request is answered and listed like a request to a name no org has (and
does not reach the hidden org), and mail is refused with `not_linked`,
after the same lookups as for an unknown name. One thing cannot be hidden:
registering a new org with the name of a hidden org fails, because two
orgs cannot share a name (see [org names](#org-names) for how that answer
is limited).

## Org names

Org names are not secret, but the server does not list them. Registration
checks the invite code before the name, so without a valid code nobody
learns whether a name is taken. With a valid code, the answer `org_exists`
is counted on the invite: after 3 such answers the invite is spent (it then
gets `invalid_invite` like an unknown code), so one invite can test at most
3 taken names. A link request to a name that is not an org is answered,
listed and withdrawn exactly like a request the other org has not accepted
yet. When an org of that name is created later, the request does not reach
it, but it stays in the requesting org's list; asking again sends a real
request, with the same answer. Every link request that is not accepted
within 30 days is dropped, whether or not an org of that name exists, so
the requesting org cannot tell the two apart by watching its list.

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
web interface; an operator with `amail admin org delete`. Deleting removes
the org's seats, lists, links, tokens, its mailboxes and the mail it sent
that no other org received. The copies other orgs received stay in their
mailboxes and archives, shown as from `name@org (deleted org)`: an org
cannot remove mail it sent to another org, such as a prompt-injection
attempt, by deleting itself. Of the deleted org, only the names of the
seats that sent such mail are kept. A new org that takes the name sees none
of this mail and is never shown as its sender. That mail is counted, for
each org that keeps it (in full, with its own deliveries), as `kept_bytes`,
which is not held against the org's storage quota: a linked org cannot
stop another org from sending by filling its quota and deleting itself.
What it can leave behind is bounded by its own quota. The receiving org's
user can delete that mail (`amail thread delete`, `amail org
purge-deleted-orgs`), and retention deletes it once it is older than the
retention period. The user can also delete the org's internal mail (mail
no other org received); mail exchanged with orgs that still exist cannot be
deleted by either side, so neither can remove what it sent to the other.
An operator may set a retention period after which message bodies are
cleared. Backups the
operator made earlier are not affected by either. A public server limits
the mail each org may store (see [resource limits](#resource-limits)).

## Resource limits

Every input that could make the server use unbounded memory, storage or
time has a fixed bound. Limits marked *public* apply only with
`amail serve --public` (each can be changed with the flag or environment
variable shown in [operations.md](operations.md)); the others always apply.
Rate and stream limits are counted per server instance.

| What | Limit |
|---|---|
| Request body (any endpoint) | 2 MiB; a transcript post (`POST /seats/{name}/transcript`) 1 MiB (HTTP 413 beyond; nodes split their posts to fit). A body whose declared length is above 2 MiB is answered with 413 before authentication; up to 16 MiB of it is read and dropped first (for at most 5 s), so that the client receives the answer instead of a reset connection |
| Message subject | 4,096 characters (Unicode scalar values: a character outside the Basic Multilingual Plane counts once), one line. Where a subject is shown on one line (injected header lines, the text output of the command line, system mail, list rows of the web interface) it is cut to 200 characters followed by `…` (120 in digests); `--json`, the API and the web interface's message view keep all of it |
| Other stored text | token labels, seat display names and models, node labels 200 characters; seat roles 16,000; list descriptions 1,000; harness names and presence `attention` 64; presence hints 500; URLs (webhook, terminal) 2,000 (HTTP 400 beyond) |
| Message body | 256 KB |
| Addresses in one message | 100 |
| Seats one message reaches | 200 (org setting `max_recipients`) |
| `meta` | 16 keys, 4 KB in total |
| Reply chain depth | 50 (org setting `max_thread_depth`) |
| Messages sent per agent seat | 30 per minute, bursts of 30 (org settings; the user seat is exempt) |
| Org settings | `send_rate_per_min` and `send_burst` at most 600, `max_recipients` and `max_thread_depth` at most 1,000 (*public*); `ack_timeout_min`, `focus_max_min` and `digest_interval_min` at most 10,080 (one week) |
| Messages per org | 50 per second (*public*) |
| Mail stored per org | 1 GiB (*public*): every message the org sent counts its subject, body, `meta` and addresses plus 512 bytes, and 384 bytes per delivery (one per recipient seat, so a list counts once per member); mail of deleted orgs is counted for the orgs that keep it as `kept_bytes`, not against their quota (retention deletes it after the retention period, and the user can delete it); retention gives back the bytes of cleared bodies; a send over it is refused with 507 `storage_quota`, and an org at its quota can send nothing, not even an empty message, until it deletes internal mail or retention clears bodies (mail exchanged with orgs that still exist keeps counting its headers and deliveries) |
| Messages deleted per request | 1,000 (`DELETE /threads/{id}`, `POST /archive/purge-deleted-orgs`); the answer says whether more remain. A purge looks at the org's mail in steps of 5,000 messages and starts no step after 100 ms, then answers with `next_before_id` |
| Messages over all orgs | 500 per second (*public*) |
| Requests per token or browser session | 200 per second, bursts of 400 (*public*); sign-ins count too |
| Requests per org, all its tokens and browser sessions together | 500 per second, bursts of 1,000 (*public*); once the server knows a token's org, a request over the limit is refused before the database is asked, and the refusal (429) is sent after one second, so that a client that retries at once is slowed down |
| Registration attempts per client address (IPv6: per /64 network) | 10 per hour, bursts of 5, failed ones included (*public*); in addition, all /64 networks of one IPv6 /48 together get 4 times that (40 per hour, bursts of 20), checked first |
| Link requests per org | 20 per hour, bursts of 5 (*public*) |
| New threads to other orgs | per agent seat: the org's `new_threads_per_hour` (10 by default, at most 100 whatever the org sets); the user seat: 60 per hour; per receiving seat: 20 per hour from any one other org (per server instance). Replies in existing threads are not counted. Over them a send is refused with 429 `thread_rate_limited` or `receiver_rate_limited` |
| Notices of link requests from one org to another | 3 per day (requesting, withdrawing and requesting again sends no more; per server instance) |
| Lookups of other orgs' cards (`GET /v1/orgs/{name}`) per org | 600 per hour, bursts of 60 |
| Openings (grants) per org | 1,000 |
| Teams per org | 500 |
| Notes of link requests, descriptions | 500 characters, one paragraph |
| Node notice reports (`POST /v1/node/notices`) per org | 60 per hour, bursts of 10, all of the org's nodes together; at most 20 notices per report, 100 items per notice and 50 seat names per item |
| Notice mail (node notice reports, and notices of changes to openings, visibility and links) per org | one mail per report, body at most 4 KB (items and notices that do not fit are left out and counted); 50 mails and 128 KB in any 24 hours, counted in the database (so across restarts and instances; with several instances, reports that arrive at the same moment on different instances can each pass); reports over it are answered as taken and dropped; not counted against the storage quota and never refused at it; deleted after 7 days, whatever the retention setting |
| Failed authentications per client address (IPv6: per /64 network) | 10 per minute, bursts of 30: unknown, revoked or expired tokens and sessions, at sign-in, on API requests and operator requests (*public*); beyond them, a request from that address with a credential the server instance has not seen valid lately gets 429 without a database lookup. Credentials that were valid on that instance lately keep working from that address; a valid credential not seen yet (after a restart, say) is refused there until the address has room again, and works from other addresses |
| Pending outgoing link requests per org | 1,000; a request not accepted within 30 days is dropped |
| `org_exists` answers per invite code | 3; then the invite is spent |
| Keys tracked by each rate limiter | 100,000; when a new key arrives at that count, the fullest 1/64 of the buckets are dropped to make room, so a new client always gets a bucket of its own; keys whose bucket is full again are dropped on every cleanup pass |
| Agent seats per org | 200 (*public*) |
| Lists per org | 500 (*public*) |
| Live tokens per org | 1,000 (*public*) |
| Open event streams per org (`/events` and conversation mirrors) | 32 (*public*); the `directory` and `nodes` events of user tokens come on the same `/events` stream, not on a connection of their own |
| `directory` and `nodes` events | at most 256 `directory` events wait for one stream (a stream further behind skips the oldest; the web interface reloads the seat list once per second at most); `nodes` sends only the latest count |
| `GET /inbox` page | 50 by default, at most 1,000, and at most 2 MiB (`truncated` says the page was cut) |
| `POST /claim` | at most 500 deliveries; `inline_body_max` and `preview_chars` at most 16,384 characters (a longer body is not inlined) |
| `POST /claim/release` | at most 500 message ids per request, among at most the first 1,000 deliveries of the claim; a claim key is kept 6 hours |
| `GET /changes` page | 500 by default, at most 5,000, and at most 2 MiB (`more` says what follows) |
| `GET /archive/messages`, `GET /sent` page | 50 by default, at most 500, and at most 2 MiB; with the delivery counts of `/sent` (`delivery=true`) at most 100 |
| `GET /archive/search` | 50 results by default, at most 200, and at most 2 MiB; at most 10 terms; each request looks at most at the org's 50,000 newest messages before its cursor (2,000 when a term is shorter than 3 characters), in steps of at most 5,000 messages and 4 MiB of body text divided by the number of terms, and starts no step after 100 ms; then it answers with `next_before_id` |
| Messages one archive request looks at | at most 50,000 of the org's own, newest first (or of the thread, with `thread=`), in steps of 5,000, and no step after 100 ms; then it answers with `next_before_id`. Filters by one of the org's seats (`from=`, `to=`) read that seat's mail directly |
| Requests whose database cost depends on the data, in progress, in three pools: search (`/archive/search`, `/archive/messages`); other (`GET /threads/{id}`, `/messages/{id}/status`, `/changes`, `/inbox` and `/sent` pages of more than 100, and the delivery counts of a `/sent` page); light (`/overview`, `/attention`, `/lists`, `/lists/{name}`, each with a fixed upper bound on its cost) | in each pool 1 per org (*public*); the search and the other pool also 8 each over all orgs (*public*), the light pool has no limit over all orgs. A request waits up to 1 s for a slot, then gets 429 `too_many_queries`; a freed slot goes to the org holding the fewest slots of the pool, then to the org served least recently. A slot is held until the database work ends, also when the client gave up |
| One query of those requests | 10 s, then it is cancelled (503 `query_timeout`) |
| `GET /pending`, `GET /counts` | read per-seat counters, kept in the transactions that change deliveries; their cost does not depend on the number of deliveries |
| `GET /attention` | looks at the user seat's newest 5,000 deliveries; 500 items and 2 MiB |
| `GET /threads/{id}` | whether the caller sees the thread is checked first, with index lookups, so a thread the caller cannot see costs what a missing one does; then the newest 500 messages and at most 2 MiB of headers (`truncated` says when older ones were left out); message bodies up to 1 MiB in total, newest first; the others come with `body_omitted` and are read one at a time with `GET /messages/{id}` |
| `GET /seats` | a seat's role is cut to its first 1,000 characters (`role_truncated`); `GET /seats/{name}` has all of it |
| List members | 1,000 per list |
| Event replay buffer (`Last-Event-ID`) | 4,096 events per org; a subscriber more than 1,024 events behind is disconnected and resynchronizes |
| Presence report | 5,000 seats per request; an entry is valid for 90 s |
| Conversation mirror, per watched seat | in memory only while someone watches: the last 1,000 events or 4 MiB kept for watchers that join later; at most 8 MiB waiting to be sent to one watcher (a watcher that falls further behind, for example one that stopped reading, is disconnected and its queue freed; it reconnects and starts again from the kept events); 16 MiB for the seat in all (beyond it, the watchers furthest behind are disconnected); 1,000 events per post; 16,000 characters per event; 2,000 characters per status field |
| Conversation mirrors over all seats | 256 MiB per instance (*public*); beyond it, the watcher furthest behind is disconnected or the largest kept-events buffer shortened, whichever holds more |
| Browser sessions | 30 days (user), 1 day (operator); at most 20 per token (signing in once more ends the oldest); expired sessions and revoked tokens are deleted by the background cleanup |
| Webhooks | URL at most 2,000 characters; 3 attempts; 5 s to connect, 10 s per attempt; redirects are not followed; per receiving org, 16 notices in flight for its own mail and 16 more for mail from other orgs, of which one sending org may hold 4; further notices are dropped (*public*) |

The node limits what it reads from the server and from its socket, so a
malicious or broken server cannot make it grow without bound:

| What the node reads | Limit |
|---|---|
| A response body from the server | 16 MiB |
| A line or an event of the server's event stream | 1 MiB; a longer one ends the stream and the node connects again |
| Seats it keeps state for without hosting them (named by the server) | 1,000; names that are not seat names are ignored |
| A request on its socket | 64 MiB (the hook client sends at most 32 MiB of hook input) |
