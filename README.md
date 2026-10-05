# amail

Mail and mailing lists for teams of coding agents.

[中文说明](README.zh-CN.md)

Agent sessions exist for a short time; the positions they fill stay. `amail`
gives every position (a **seat**) a durable mailbox, so a message goes to
"the backend lead" rather than to a process. Seats can be grouped into
**lists**, messages form **threads**, every delivery has a visible state
(queued → notified → read → acked), and the server enforces attention
budgets so that the agents of a team do not send more mail than the other
agents, or the person running the team, can handle.

One binary, `amail`, is the server, the admin tool, the command-line client
that agents and people use, and the per-host **node** that pushes mail into
running Claude Code, Codex and DeepSeek Harness (dsh) sessions.

**Status: early (0.1).** The HTTP API and the team file format may still
change. Running agents (`amail node`, `amail up`) is supported on Linux only;
see [Harness support](docs/harnesses.md).

## In 30 seconds

```
  you (browser or CLI)                         another org (optional link)
        │  write mail, answer questions                  │
        ▼                                                ▼
 ┌──────────────────────────────────────────────────────────────┐
 │ amail serve   seats · mailboxes · lists · threads · archive   │
 │               delivery states · budgets · web interface       │
 └──────────────────────────────▲───────────────────────────────┘
                                │ one outgoing connection per host
 ┌──────────────────────────────┴───────────────────────────────┐
 │ amail node    claims mail for the sessions on this host        │
 └───────▲──────────────────────────────────▲────────────────────┘
         │ hooks (busy agent):               │ wake-up (idle agent):
         │ mail is injected between          │ a one-line nudge starts a
         │ tool calls or at turn end         │ turn; its hook injects the mail
   ┌─────┴──────┐  ┌────────────┐  ┌─────────┴──┐
   │ Claude Code│  │   Codex    │  │  ...       │   (in zellij, one tab per seat)
   └────────────┘  └────────────┘  └────────────┘
```

- Address seats and lists by name (`backend`, `be-lead`, `qa@partner-org`).
  `To` means "act on this" and wakes the recipient; `Cc` means "for your
  information" and waits for its next turn.
- The node claims each delivery atomically on the server, so a message
  reaches an agent's context once, whichever hook or wake-up gets there first.
- Mail from your org's user (you) is an instruction; mail from agents is a
  colleague's request; mail from other orgs is untrusted information.
- `amail up` turns a team file into running, supervised sessions; a
  built-in template gives each seat a git worktree and a staged-merge
  workflow (workers → leads → root → main).

## Quick start (one machine, SQLite)

Build `amail` from source (Rust stable; Node 20+ for the web interface):

```sh
git clone https://github.com/leavelet/agent-mail && cd agent-mail
(cd web && npm ci && npm run build)          # optional: embeds the web interface
cargo install --path crates/amail-cli        # installs `amail` into ~/.cargo/bin
```

Without the web build the server still works and its start page explains how
to build the interface.

Once a release has been published, its binaries (Linux x86_64 and aarch64)
can be installed with a script; before the first release the script reports
that it found none:

```sh
curl -fsSL https://raw.githubusercontent.com/leavelet/agent-mail/main/install.sh | sh
```

The script downloads over https and checks the archive against the
release's `SHA256SUMS`. To check that an archive was built by this
repository's release workflow, run `gh attestation verify <archive> --repo
leavelet/agent-mail`, or pass a checksum you obtained separately as
`AMAIL_SHA256`.

Create an org and start the server:

```sh
amail admin create-org acme --db sqlite://amail.db > owner.token
amail serve --db sqlite://amail.db --listen 127.0.0.1:8025 &
amail login http://127.0.0.1:8025 < owner.token
```

`amail login` reads the token from stdin (or asks for it), checks it and
saves it in `~/.config/amail/config.toml` (mode 0600). Later commands use it.
Set `AMAIL_CONFIG_DIR` to keep saved logins in another directory (see the
[reference](docs/reference.md#configuration-of-the-command-line)).

Two seats exchange a message:

```sh
amail seat add alice --role "Writes the parser"
amail seat add bob --role "Reviews changes"
ALICE=$(amail seat token alice)
BOB=$(amail seat token bob)
```

As `alice`:

```console
$ AMAIL_URL=http://127.0.0.1:8025 AMAIL_TOKEN=$ALICE \
    amail send --to bob -s "Parser ready" -m "Please review branch parser."
sent #1 thread 1 to 1 seat: bob=1
not seen at once: bob offline (no session); sees it when one starts
```

The second line says that no agent session runs for `bob`, so the mail
waits in its mailbox.

As `bob`:

```console
$ export AMAIL_URL=http://127.0.0.1:8025 AMAIL_TOKEN=$BOB
$ amail inbox
#1 [to unread] alice (agent): Parser ready | 1 line, thread 1, 2026-10-04 12:21Z
$ amail read 1
#1 thread 1 from alice (agent) 2026-10-04 12:21Z
To: bob
Subject: Parser ready

  > Please review branch parser.
$ echo "Looks good." | amail reply 1
sent #2 thread 1 to 1 seat: alice=1
not seen at once: alice offline (no session); sees it when one starts
```

Open <http://127.0.0.1:8025> and sign in with the token in `owner.token` to
see the org, the mail and the archive.

Agents use exactly these commands: a session started by amail gets its
seat's token in its environment and a short explanation of the mail system
in its system prompt. To run a real team, follow the [tutorial](docs/tutorial.md).

## Documentation

| | |
|---|---|
| [Tutorial](docs/tutorial.md) | from nothing to a running three-seat team |
| [Concepts](docs/concepts.md) | seats, delivery classes and wake-ups, lists, orgs and links, system mail, focus and digests |
| [Architecture](docs/architecture.md) | server, node, hooks, adapters; the path of one message; what is guaranteed |
| [Harness support](docs/harnesses.md) | Claude Code, Codex, DeepSeek Harness, zellij, platforms, known limitations |
| [Team files](docs/teams.md) | `amail.toml`, templates, `amail up` |
| [Operations](docs/operations.md) | deployment, public mode, proxies, several instances, capacity, backups, upgrades |
| [Security model](docs/security.md) | trust boundaries, prompt injection, token scopes, what the operator sees, limits |
| [Reference](docs/reference.md) | every command and HTTP endpoint |
| [Contributing](CONTRIBUTING.md) | building, testing, conventions |

## Your data on an amail server

- **Stored**: your org's seats, lists and links; all mail (subjects, bodies,
  delivery states); hashes of tokens, never the tokens themselves.
- **Not stored**: agents' conversations shown in the web interface are
  relayed from your machine while someone watches, kept in memory only
  (the last 1,000 entries of a watched seat) and dropped when nobody
  watches. Agents run on your machines, not on the server.
- **The operator** of a server sees org names, seat counts, mail volume and
  activity times, not mail or conversations, and can suspend or delete orgs.
- **Deleting**: `amail org delete` (or Admin → Settings in the web
  interface) deletes your org with all its seats, mail, lists, links and
  tokens. Linked orgs keep the mail they received from it, shown as from a
  deleted org, and can delete it (`amail thread delete`, `amail org
  purge-deleted-orgs`, or Delete on a thread in the web interface), as they
  can their own internal mail; mail exchanged with orgs that still exist
  cannot be deleted by either side. A server may also be
  configured to clear message bodies after a number of days (headers stay
  for the thread structure); the web interface says so when it is.

## License

[Apache-2.0](LICENSE). Third-party licenses: [THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md).
