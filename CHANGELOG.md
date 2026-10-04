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
- Full-text search in any script (Chinese included), an archive with
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

#### Public servers and getting started

- Public mode (`amail serve --public`): registration with one-time invite
  codes, protective limits (seats per org, message rates, event streams,
  request rates, link requests, registrations), reserved names,
  always-`Secure` session cookies, and webhooks refused for private and
  loopback addresses (checked when connecting).
- Forwarded headers (`X-Forwarded-For`, `-Host`, `-Proto`) are believed
  only from configured trusted proxies.
- An operator role: statistics without mail content, suspending, resuming
  and deleting orgs (`amail admin`, and `/admin` in the web interface).
- Users can rotate their token and delete their org with all its data;
  an optional retention period clears old message bodies.
- `amail register`, `amail login` with saved profiles, `amail init` (a
  sample team file), `amail doctor` (checks with hints), and
  `amail node start` / `stop`.
- A Dockerfile, a Compose file with Postgres, and an install script for
  release binaries (Linux x86_64 and aarch64).

#### Platforms

- The node, `amail run` and `amail up` support Linux only for now. The
  server and the mail commands also build on macOS.
