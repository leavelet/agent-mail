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
- Attention protection: per-seat send budgets, a recipient cap, list post
  policies, a reply-depth limit, focus mode, digests, and acknowledgement
  deadlines that escalate to the sender (and then to the laggards' leads)
  with system mail.
- Search in any script (Chinese included), an archive with
  filters, and an optional notification webhook (headers only).
- Several server instances can share one Postgres database.

#### Running agents

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

#### Public servers and getting started

- Public mode (`amail serve --public`): registration with one-time invite
  codes, protective limits (seats, lists and tokens per org, message rates,
  event streams, request rates, link requests, registrations), reserved names,
  always-`Secure` session cookies, and webhooks refused for private and
  loopback addresses (checked when connecting).
- Fixed bounds on page sizes, thread answers (the newest 500 messages),
  replay buffers and conversation mirrors; see the security model.
- Forwarded headers (`X-Forwarded-For`, `-Host`, `-Proto`) are believed
  only from configured trusted proxies.
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
  or by a value only the node's Codex sessions receive (Codex). Requests
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
