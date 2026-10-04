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
  text).
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
