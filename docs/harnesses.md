# Harness support and known limitations

amail uses the hooks of coding-agent harnesses (to inject mail)
and through their own local interfaces (to wake idle sessions). Some of
these interfaces are not documented by the harness vendors, so a new
harness version may change them. `amail doctor` reports the installed
versions and warns, without refusing, when a version is outside the
verified range.

## Support matrix

| | verified with | busy injection | idle wake-up | resume after sleep | conversation mirror |
|---|---|---|---|---|---|
| Claude Code | 2.1.x | hooks: `UserPromptSubmit`, `PostToolUse`, `Stop` | the session's inbox socket | `claude --resume` | JSONL transcript |
| Codex (codex-cli) | 0.160.x | the same hooks | `codex queue` (the shared app-server) | `codex resume` | rollout file |
| zellij | 0.45.x | | | sessions and panes for `amail up` | |

Other harnesses are not supported yet.

## Platforms

| | Linux (x86_64, aarch64) | macOS | other |
|---|---|---|---|
| `amail serve`, `amail admin` | yes | builds; not tested | not tested |
| mail commands (`send`, `inbox`, `read`, ...), `login`, `doctor` | yes | builds; not tested | not tested |
| `amail node`, `run`, `up`, `down`, `ps`, `attach`, `sleep`, `wake` | yes | no: the command says so | no |

The node relies on `/proc` (process liveness, and reaching a Codex socket
whose path is too long), on the parent-death signal, and on harness sockets
that have only been verified on Linux. Release binaries are built for Linux
only; on macOS build from source (see the [README](../README.md)).
`amail doctor` states the platform's support.

## Claude Code

**Launch.** `amail run <seat>` (which `amail up` runs in each seat's pane)
asks the node for the seat's recipe and starts `claude` with:

- `--settings` containing the amail hooks, `crossSessionInbound: "accept"`
  (so the node's wake-ups reach sessions in bypass-permissions mode) and
  `skipDangerousModePermissionPrompt`;
- `--append-system-prompt` with the mail instructions and the seat's role;
- `--name <seat>` and `--model <model>`;
- `--dangerously-skip-permissions` for seats with `permission_mode =
  "unattended"` in the team file, or with `amail run --permission-mode
  unattended` (the server's seat setting is not used).

Your own Claude Code settings are not changed. The hooks see the session's
`AMAIL_SEAT`.

**Wake-up.** The node writes a one-line message to the session's inbox
socket, which starts a turn; that turn's `UserPromptSubmit` hook injects the
mail. Claude Code drops identical messages sent within a short time, so the
nudge is numbered (`wake #n`).

**The directory-trust prompt.** Claude Code asks once per repository whether
to trust it, even with `--dangerously-skip-permissions`, and the default
answer is "No, exit". amail does not answer it for you. Trust is recorded
per repository (for a git worktree, the main repository's path) in Claude
Code's own configuration, so answering it once covers every worktree of the
repository: attach to one seat (`amail attach <seat>`), answer, and run
`amail wake <seat>` for the other seats waiting at the prompt (Claude Code
checks trust only at start). Seats at the prompt are flagged in `amail ps`
and in the web interface's queue, and do not hold up other starts.
`amail doctor` tells you whether the current directory is trusted, when it
can read Claude Code's configuration.

**Other notes.**

- Claude Code refuses a long foreground `sleep` and runs it in the
  background instead, ending its turn; the seat then shows `idle` while the
  command still runs.
- A hook event from a sub-agent inside the session is ignored, so mail is
  never claimed into a context the main agent does not see.

## Codex

**One Codex home per host.** All Codex seats of a host share one dedicated
`CODEX_HOME` (`<state dir>/codex-home`) and one app-server daemon, which the
node starts. Its `auth.json` is a link to `~/.codex/auth.json` (or
`--codex-auth`); your own `~/.codex` configuration is not changed. The
hooks are defined and pre-trusted in that home's `config.toml`, which also
holds the mail instructions.

**Seat identity.** Hooks run inside the shared daemon, not in the session's
terminal, so they do not see the session's environment. A session is tied to
its seat by a one-time marker in its first prompt
(`[amail:seat=<seat> launch=<nonce>]`), and tool commands find their seat's
credentials through the node by `CODEX_THREAD_ID`. The node answers both
only when they carry the node's Codex pass (`AMAIL_CODEX_PASS`), which it
puts in the shared daemon's environment. Codex sets per-thread environment
variables only when a thread is started, and the TUI starts the thread, so
the pass is the same for all Codex seats of a node: Codex seats of one node
are not separated from each other (any process of the user can also use the
daemon's control socket to act in any thread). Codex seats started before
an upgrade that added the pass get no mail until the daemon restarts
(`amail down`, then `amail up`). Only launch flags that
keep the session on the shared daemon are allowed (`-m`, `-a`, `-s`, `-C`,
...; not `-c`, `-p`, `--remote`).

**Sandbox and permissions.** `unattended` Codex seats (chosen in the team
file or with `amail run --permission-mode`, as for Claude Code) run with `-a never`
under a permissions profile in the dedicated home: the workspace sandbox
(the working directory, `/tmp` and `$TMPDIR` writable, everything else
read-only), network access, and the git grants of the seat's `git_access`.
The plain workspace sandbox keeps `.git` read-only, and a linked worktree's
git directory lies outside the worktree, so without a grant a session
cannot commit. `git_access` is set per seat in the team file (or with
`amail run --git-access`):

| `git_access` | profile | what sessions may write |
|---|---|---|
| `worktree` (default) | `amail` | In a linked worktree: the repository's `objects/`, `refs/`, branch reflogs (`logs/refs/`) and `amail/` (see below), and the worktree's own git directory except its `commondir`, `gitdir` and `config.worktree`. Nothing else in the root of the repository's git directory: `config`, `hooks/`, `info/`, `commondir` and the main checkout's `HEAD`, `index` and `logs/HEAD` stay read-only, and so does the reflog of the branch checked out in the main checkout. In a main checkout: nothing (the session cannot commit; `amail run` says so). |
| `full` | `amail-full` | The whole git directory of the repository and its main checkout, except `hooks/`, `config`, `config.worktree`, `modules/` and `worktrees/` (and `commondir`, `gitdir` and `config.worktree` in each linked worktree's git directory). For a seat that merges in the main checkout. |
| `off` | `amail-off` | No git grant. |

The staged-merge template gives its root seat `full` when the team has
other seats (the root merges into the main branch in the main checkout);
`amail up` names such seats in its output. Everyone else gets `worktree`
unless the team file says otherwise. With `full`, a session can make git
run commands outside its sandbox (see [security](security.md), "Codex seats
and git"); give it only to seats that need it.

How it is set up:

- All Codex seats of a host share one Codex home, and with it one
  `default_permissions` (`amail`). The `amail` profile holds the
  `worktree` grants of every such seat on the host: a session can also write
  the git directories of the other seats' worktrees. A seat with `full` or
  `off` gets its own profile through a project config amail writes into
  its working directory, `.codex/config.toml` (with a `.codex/.gitignore`
  so git ignores both); Codex reads `default_permissions` from there, and
  keeps `.codex/` read-only inside the sandbox. amail removes the file when
  the seat goes back to `worktree`. A seat does not start when a
  `.codex/config.toml` amail did not write sets `default_permissions` in
  its working directory or a directory above it in the checkout, or when
  another seat with another `git_access` uses the same directory.
- Git takes the lock `packed-refs.lock` in the directory of `packed-refs`
  whenever it deletes a ref, which a rebase does. So that `worktree`
  sessions can take it without write access to the root of the git
  directory, the node moves `packed-refs` into `.git/amail/` and leaves a
  symbolic link `.git/packed-refs -> amail/packed-refs` (git follows it when
  it reads, locks and rewrites the file). The file itself stays read-only
  in the sandbox. To undo, move `.git/amail/packed-refs` back over the link.
- Codex accepts only directories as write roots (a file as a write root
  stops the sandbox from starting), and a missing file can only be made
  read-only as an empty placeholder, so the grants are directories minus
  read-only entries, not a list of files.

Consequences:

- Codex needs network access in its sandbox to reach the amail server:
  with networking disabled, the sandbox refuses both TCP and Unix-socket
  connections. The dedicated home allows it.
- With `worktree`, sessions commit, amend, rebase, merge, cherry-pick,
  stash, reset, tag and create branches in a linked worktree. They cannot
  delete branches or tags (`git branch -d` rewrites `packed-refs`), change
  git config (`git config`, `git remote add`, `git branch
  --set-upstream-to`, which prints an error but exits with status 0), add
  worktrees, or initialise or commit in submodules. Do these outside the
  session. A Codex seat whose working directory is a main checkout needs a
  linked worktree (`amail up` creates them for the staged-merge template)
  or `full` to commit.
- A Codex seat does not start while a repository it may write contains a
  `commondir` file in its git directory (git never creates one there), or a
  symbolic link in `refs/`, `logs/`, `amail/` or a linked worktree's git
  directory (git never creates those either). `amail up` checks the same
  before it runs git in the repository.
- Codex's sandbox needs unprivileged user namespaces. Some distributions
  restrict them (for example `kernel.apparmor_restrict_unprivileged_userns
  = 1`); then every sandboxed command fails. `amail doctor` checks this.

**Wake-up.** `codex queue` adds the nudge to the thread through the daemon;
a message for a thread without a terminal is kept and runs when the session
is resumed.

**Control socket path.** The daemon's control socket lies in the state
directory. When its path is longer than a Unix socket path may be (107
bytes), the node reaches it through `/proc/self/fd` (Linux). A shorter
`AMAIL_STATE_DIR` avoids this; `amail doctor` shows the length.

## zellij

`amail up` runs one zellij session per group (`amail-<org>-<group>`, seats
without a group in `amail-<org>-main`), one tab per seat. The node keeps a
headless client of fixed size (200x60) attached to each session, because a
session without any client lays its panes out at about 50 columns. Your
zellij configuration and your own sessions are not touched.

Browser terminals: `amail node --web-terminal-port <port>` runs a
`zellij web` server of the node's own on `127.0.0.1:<port>` and the web
interface links each seat to its session; create a login token with
`amail node web-token`. Anyone logged in to that server sees the names of
all your zellij sessions and can attach to those that allow web sharing.
`zellij web --stop` stops every zellij web server of your user, including
the node's (the node restarts its own). Put it behind an HTTPS proxy and pass
`--web-terminal-url` to reach it from another host.

## Limitations common to both harnesses

- **Mail cannot interrupt.** A busy agent sees mail only between tool calls
  (`to` and urgent) or at the end of its turn; a long tool call or a long
  generation delays it, and `urgent` does not interrupt either.
- **Long bodies are not inlined.** One hook shows at most 8 (Claude Code) or
  5 (Codex) messages, each with its body only up to 800 characters; longer
  mail says `body not shown (N lines): run amail read <id>` and stays
  unread until the agent reads it. Agents do not always read it before
  acting.
- **Wake-ups can go unanswered.** A session in a dialog or menu, or one that
  ignores its inbox, does not start a turn. An unanswered wake-up is retried
  once after 10 s, then the seat is reported as `wake_failed` and tried again
  after 1 minute, doubling up to 10 minutes, or at once when new mail comes.
- **Terminal paste is a last resort, off by default.** With
  `paste_fallback = true` in the team file, a supervised seat whose two
  native wake-ups went unanswered gets the nudge typed into its pane, but
  only if the seat is idle, the bottom of its screen shows no dialog, menu
  or busy marker, and its input box is empty; Enter is pressed only if the
  box then holds exactly the nudge. Typing into a terminal is risky: an
  open dialog would take the text and Enter as its answer, and a
  half-written draft would be sent with the nudge.
- **State reflects hooks.** Seat states come from hooks and process
  liveness; a background task the agent started is not visible.
- **One session per seat.** A new `amail run` for a seat replaces the
  seat's session: the node then answers only the new session's hooks, and
  the token the previous one was given is revoked.
- **Credentials on the host.** The node answers hooks and credential
  requests only from the session of the seat they concern (for Codex: from
  any Codex session of the node, see below), and requests meant for the
  person only from processes outside the sessions it launched. Agents still
  run as the person's operating-system user and can read that user's
  files; see [security](security.md#trust-boundaries).
- **After a node restart**, sessions whose process and wake target still
  exist are restored as idle; anything else waits for its next hook.
