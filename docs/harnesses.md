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
| DeepSeek Harness (dsh) | 0.2.x | the same hooks, through dsh's Codex hook bridge | the web host's API (`session/prompt`) | the host loads the session again | JSONL session log |
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

**MCP server.** The launch also passes `--mcp-config <state
dir>/claude/mcp.json`, a file the node writes that registers `amail mcp
--node` as the server `amail` ([reference](reference.md#mcp-server)), and
the `--settings` file allows its tools (`permissions.allow:
["mcp__amail"]`), so they run without a permission prompt in the `default`
permission mode too. Claude Code starts the server as a child of the
session, with the session's environment, so the server acts as the seat of
the session's `AMAIL_TOKEN`. The file holds no credential and is the same
for every seat. `amail` commands in the shell keep the session's permission
mode.

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

Codex runs the daemon from a package it installs under that home
(`packages/app-server-daemon`), and by default updates the package on its
own. A session does not attach to a daemon of another version than its
`codex` command: the TUI stops at "Background server has incompatible
feature settings" and offers to restart the daemon from the session (the
restarted daemon then has that session's environment). The node therefore
turns the self-update off in the daemon's `settings.json`
(`updater.autoUpdateEnabled = false`) and, before it starts the daemon,
removes a package whose version is not the command's, so that the start
installs the command's version again. When a seat still shows that prompt
(the daemon was updated while it ran, or the `codex` command was upgraded),
`amail ps` reports `codex-daemon`: choose Cancel in the pane, then run
`amail down` and `amail up`.

**Seat identity.** Hooks run inside the shared daemon, not in the session's
terminal, so they do not see the session's environment. A session is tied to
its seat by a one-time marker in its first prompt
(`[amail:seat=<seat> launch=<nonce>]`), and tool commands find their seat's
credentials through the node by `CODEX_THREAD_ID`. The node answers both
only when they carry the node's Codex pass (`AMAIL_CODEX_PASS`), which it
puts in the shared daemon's environment. The session's own environment
holds no seat token (`AMAIL_TOKEN`), because a daemon the session starts
or restarts inherits that environment, and every thread on it would then
act as that seat. Codex sets per-thread environment
variables only when a thread is started, and the TUI starts the thread, so
the pass is the same for all Codex seats of a node: Codex seats of one node
are not separated from each other (any process of the user can also use the
daemon's control socket to act in any thread). A daemon that was started
without the node's environment (before the node, or by a person's own
session in the same home) still serves the hooks: the hook command names the
node's state directory and socket (`--node-state-dir`, `--node-socket`), and
a hook without the pass in its environment reads it from the node's state
file, after checking that it runs in that node's Codex home and that the
file and the socket's peer belong to this user. Only launch flags that
keep the session on the shared daemon are allowed (`-m`, `-a`, `-s`, `-C`,
...; not `-c`, `-p`, `--remote`).

**MCP server.** The home's `config.toml` also has `[mcp_servers.amail]`:
`amail mcp --node` ([reference](reference.md#mcp-server)), with the node
socket in `env` and `env_vars = ["AMAIL_CODEX_PASS"]`. The shared daemon
starts the server for each thread and passes it only the variables Codex
passes to every MCP server, the configured `env` and the `env_vars` it
names from the daemon's environment; the pass is therefore not written in
the file. No variable names the thread. Codex instead sends the thread id
in the `_meta` of every tool call (`threadId`, seen with codex-cli 0.160
and its 0.164 daemon; `env_vars` is passed on as described, seen with
0.160), and the server asks the node for the credentials of that thread's
seat, as `amail` commands do with `CODEX_THREAD_ID`, with the pass. A call
without a thread id the node knows answers
`no_identity`; the server never falls back to a saved login. The entry does
not change the hook commands, so the hooks' trust hashes stay the same;
other `mcp_servers` entries in the file are kept.

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
is resumed. A nudge added to a busy thread runs as a turn of its own after
the current one, by which time the mail has usually been shown by that
turn's hooks. So before queueing, the node asks the daemon about the thread
(`thread/read`, `thread/queue/list`): while a turn runs, or while a nudge
is already queued, it adds nothing, and the running turn's `Stop` hook
shows the mail at its end. A nudge whose turn finds nothing pending any
more is answered with one line that says so, and the agent ends the turn.

**Control socket path.** The daemon's control socket lies in the state
directory. When its path is longer than a Unix socket path may be (107
bytes), the node reaches it through `/proc/self/fd` (Linux). A shorter
`AMAIL_STATE_DIR` avoids this; `amail doctor` shows the length.

## DeepSeek Harness (dsh)

dsh has no terminal interface. A person uses a dsh session in a browser,
through the GUI of a `dsh web` host, and amail reaches the session through
the same host's HTTP API.

**Launch.** Each dsh seat runs its own host. `amail run <seat>` starts
`amail dsh-host`, which starts `dsh web --port 0` and reads the line the
host prints at start-up (`dsh web: <url>/?token=<token>`). The wrapper
writes the address and the one-time launch token to a private file in the
node's state directory, creates the seat's session through the API (or
loads its earlier session), and sends the first prompt. The same line
appears in the seat's pane: open that URL in a browser to use the
session. The pane shows the host's log, not the conversation.

Each dsh seat has its own dsh home (`<state dir>/dsh-home/<seat>`), which
amail sets up to write uncompressed JSONL session logs; your own `~/.dsh`
is not used. The endpoint file with the host's address and launch token is
in a directory of its own for each seat (`<state dir>/dsh/run/<seat>/`). A per-seat `--patch` overlay sets the system prompt (the mail
instructions and the seat's role) and the model (for example
`deepseek-flash`, or `<provider>/<model>`). The node finds the launcher as
`dsh` on `PATH`, or as `amail node --dsh-bin` (`AMAIL_DSH_BIN`).

**The model provider's key.** Give it with `amail node --dsh-env-file
<file>` (`AMAIL_DSH_ENV_FILE`): a file of `NAME=value` lines, for example
`DEEPSEEK_API_KEY=...`. The node reads the file when it starts and passes
these variables only to dsh hosts. `amail node start` takes the same
options (`amail node start --dsh-env-file <file> --dsh-bin <path>`), and
`amail node status` shows the dsh program the node uses and the names of
the variables the file sets. A key in the node's own environment
would reach every process the node starts, including Claude Code and
Codex sessions. dsh removes variables with credential-shaped names
(`*KEY*`, `*TOKEN*`, `*SECRET*`, `*PASSWORD*`) from the environment of the
agent's shell commands. For the same reason the seat's `amail` command
finds its token through a profile file (`AMAIL_PROFILE`), not
`AMAIL_TOKEN`.

**Hooks.** dsh's Codex hook bridge runs `amail hook dsh` for
`SessionStart`, `UserPromptSubmit`, `PostToolUse` and `Stop`. Hooks run as
children of the host and see the seat's `AMAIL_SEAT`. The node identifies
them by their parent processes, also inside the jail described below,
which has its own process namespace: the kernel reports a connecting
process's id in the node's namespace, so the chain of parents reaches the
process the node launched for the seat. Under the restricted local policy
the node answers a dsh hook only when it comes from below the session of
the seat it names; under the open policy (the default) it answers it and
reports a hook sent in another seat's name to you (see
[security](security.md#trust-boundaries)). The bridge has
no `SessionEnd`, permission or interrupt event. When a turn is cancelled in
the GUI or fails, dsh runs no hook, so the wrapper follows the session log
and reports a `turn/end` that did not complete as the end of the turn.
Only the session amail created for the seat is connected to mail; other
sessions a person opens in the same host's GUI are ignored.

**MCP server.** dsh seats do not get the amail MCP server: amail does not
know of an MCP configuration in dsh's patch overlays. Agents there use the
`amail` command line.

**Wake-up.** The node logs in to the host with its launch token and queues
the nudge with `session/prompt` (mode `queue`). On an idle session this
starts a turn; on a busy one it runs as the next turn; a session that is
not loaded is loaded first. A sleeping seat is started again, its host
loads the saved session, and the nudge follows.

**Unattended seats run in a jail by default.** dsh's own sandbox modes
(`read-only`, `workspace-write`, `danger-full-access`) limit writes only,
and its file tools (`read`, `grep`, `glob`) read files inside the host
process without any sandbox. So for a seat with `permission_mode =
"unattended"` (in the team file or with `amail run --permission-mode
unattended`) amail runs the whole host under bubblewrap (`bwrap`), unless
the jail is turned off for the seat (see below):

- `/` (the whole system) is mounted read-only; `/dev` is a new minimal
  device directory and `/proc` belongs to the jail's own process namespace
  (other processes and their environments are not visible);
- empty `tmpfs` mounts cover the home directory, `/tmp` and
  `$XDG_RUNTIME_DIR`;
- the Docker, Podman and containerd sockets (`/run/docker.sock`,
  `/var/run/docker.sock`, `/run/podman/podman.sock`,
  `/run/containerd/containerd.sock`, those that exist) are covered by
  `/dev/null` (a Unix socket can be used through a read-only mount);
- mounted back writable: the seat's working directory and its own dsh
  home;
- git follows the seat's `git_access` (in the team file or `amail run
  --git-access`), with the same rules as for Codex seats (see
  [Codex](#codex)): with `worktree` (the default) a seat in a linked
  worktree may write the repository's `objects/`, `refs/`, `logs/refs/` and
  `amail/` (where `packed-refs` is moved) and its worktree's own git
  directory, and everything else in the repository's git directory stays
  read-only, so it can commit, tag, rebase and merge but not delete
  branches or tags or change git config; a seat in a main checkout cannot
  commit with `worktree` (`amail run` says so). `full` (the default for the
  root of a staged-merge team that has subordinates, which merges in the
  main checkout) makes the main checkout and the whole git directory
  writable except `hooks/`, `config`, `config.worktree`, `modules/`,
  `worktrees/` and, in linked worktrees' git directories, `commondir`,
  `gitdir` and `config.worktree`. `off` makes git read-only. The working
  directory's `.git` stays read-only unless `full` makes it writable.
  Read-only entries that do not exist are covered by an empty read-only
  placeholder (an empty `tmpfs` mounted read-only for `hooks/`, `modules/`,
  `worktrees/` and `info/`, an empty file otherwise), which leaves an empty
  file or directory of that name in the git directory. A seat does not
  start while a repository's git directory contains a `commondir` file or
  a symbolic link where git never creates one;
- mounted back read-only: the directory that holds the `amail` binary, the
  `dsh` installation (the directory above its `node_modules`), the Node.js
  installation (two levels above the real path of the `node` found in
  `PATH`), the repository's git directories (except what the rules above
  make writable), the seat's profile and patch, its own endpoint directory
  and the node socket's directory;
- mounted back read-only, each file on its own: git's configuration files
  `~/.gitconfig` and, in git's configuration directory
  (`$XDG_CONFIG_HOME/git`, by default `~/.config/git`), `config`, `ignore`
  and `attributes`, those that exist. The directory itself is not mounted,
  so git's credential stores (`~/.config/git/credentials`,
  `~/.git-credentials`) stay hidden. If one of these files is a symbolic
  link, only the regular file it points to is mounted, at the link's path,
  and a link to a file named `credentials` or `.git-credentials` is not
  mounted. Files that other configuration names (`include.path`,
  `core.excludesFile` under the home directory) are not mounted;
- paths in these lists that do not exist are skipped (except the
  placeholders above); a deeper path is mounted after the one that
  contains it;
- the host runs in a new session without a controlling terminal, and its
  standard input is `/dev/null`, so it cannot read from or type into the
  terminal the seat was started in (dsh's web host does not read its
  input; its output still goes to the seat's pane);
- the network is shared (the model API and the amail server).

Inside the jail dsh runs with `DSH_PERMISSION_MODE=danger-full-access`;
the jail is the limit. If the jail is on and `bwrap` cannot run (not
installed, or user namespaces not allowed), the seat does not start and
`amail run` prints why; it does not fall back to running without the
jail. `amail doctor` checks `bwrap`. A seat that is not `unattended` has no
jail: dsh asks for approvals in the browser, and its file tools can read
any file the user can read.

**What a jailed seat can still read or reach:**

- every file outside the home directory and the runtime directory, read
  only;
- its own dsh home, including its credential store (`.credentials.yaml`,
  with the GUI's cookie secret and any key saved through the GUI);
- its own host's environment, which holds the model provider's key (dsh's
  in-process tools run in the host process);
- the network.

**Turning the jail off.** The jail is a local setting with a default, not
a rule. To run an unattended dsh seat without it, set `dsh_jail = false`
for the seat in the team file (or in `[defaults]` or a `workers` table),
or start it with `amail run <seat> --dsh-jail off`. Such a seat runs with
`DSH_PERMISSION_MODE=danger-full-access` and nothing else around it: its
agent can read every file your operating-system user can read, including
saved logins (`~/.config/amail/config.toml`, `~/.claude`, `~/.codex`),
SSH keys and API keys in files, and other sessions' environments in
`/proc`, and it can write every file your user can write. `amail up` names
every unattended dsh seat with the jail off, `amail run` prints the same
warning, and the node reports a change of the setting to you by system
mail, like a change of the permission mode or of `git_access`. Turn it off
only on a host and an account where that is acceptable, for example a
throwaway machine or a container.

**Other notes.**

- The bridge reports no permission requests, so a seat that is not
  `unattended` waiting for an approval in the browser shows `busy`, not
  `awaiting`. Its `Stop` input has no `stop_hook_active`; the node's limit
  of three blocks in a row prevents loops.
- A turn cut off by a crash of the host is not continued after the
  restart; mail it had already read is not delivered again.
- Each seat runs a host process (about 200 MB when idle).

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

## Failed turns

A turn can end in a failure: the model provider answers with an error (an
invalid or missing key, a rate limit, an unknown model, a server error),
or the session process ends during the turn. The node then returns the
deliveries that turn showed to "not notified, not read"
(`POST /v1/claim/release`), so the sender's `amail status` is true again,
and wakes the seat again after 30 s, then 60 s. After three failed turns
in a row it stops waking the seat for that mail (the mail stays unread
and comes with the seat's next turn), keeps the seat flagged
(`turn-failed`, with the harness's error text) and reports it to the
user once by system mail (as part of the node's next notice report, so
it counts against the org's daily notice budget; see
[security](security.md#resource-limits)). `amail wake <seat>` (or Wake in the web interface) lets the
node deliver the mail again until the next failure; a turn that ends
normally clears the flag. A turn the person interrupts is not a failure,
and its mail counts as delivered.

How each harness's failed turns are found:

| harness | signal |
|---|---|
| Claude Code | the `StopFailure` hook, sent instead of `Stop` when a turn ends in an API error. Claude Code 2.1.289 sends `error` (the error type, e.g. `authentication_failed`) and `last_assistant_message` (the text shown); its documentation names `error_type` and `error_message`, which are read as well. Found within about a second |
| Codex | Codex hooks have no failure event (0.160: `Stop` and `Interrupt` only), and a turn that fails sends no hook at all. The node asks the shared app-server how the turn ended (`thread/turns/list`; status `failed` with the error message): about a second after `Stop` or `Interrupt`, and every 3 s while a busy seat sends no hooks. The same ended turn must be listed twice in a row, so a failed turn is found about 6-9 s after it started (codex-cli 0.160 with a model the account cannot use: 7 s and 9 s); until then the sender sees `read` and the seat `busy` |
| dsh | the wrapper (`amail dsh-host`) follows the session log; a `turn/end` with reason `error` (with dsh's message) or `interrupted` (a turn whose host stopped during it) is reported as a failed turn; `aborted` (cancelled in the GUI) and other reasons count as an interruption. dsh 0.2.0-rc.2 sends no `Stop` for a failed turn |
| all | the session process ends while the seat is busy (supervised seats: the pane's process exits other than with status 0; other seats: the process the hooks named is gone) |

## Interrupted turns

A person can stop a turn (Escape in Claude Code or Codex, the stop button
in the dsh GUI). The seat becomes idle, the mail that turn showed counts as
delivered (the person chose to stop it; it is not shown again), and later
mail is delivered as usual.

| harness | how the node learns of it |
|---|---|
| Claude Code | Claude Code fires no hook for an interrupted turn. It writes a user record `[Request interrupted by user]` (or `... for tool use]`) to the session's transcript. When a busy seat has sent no hook for 3 s, the node reads the end of its transcript every 3 s (only when the file changed) and ends the turn if the last record of the turn is that interrupt and is not older than the turn. Found 1-2 s after the Escape with Claude Code 2.1.289. Fallback, for supervised seats whose transcript says nothing: after 60 s without hooks the node also looks at the pane; Claude Code's status line says `esc to interrupt` while a turn runs (model output or a tool call), and the turn counts as ended only when two looks at least 3 s apart, with no hook in between, both show no such line |
| Codex | the `Interrupt` hook (codex-cli 0.160: about 10 ms after Escape); the app-server check above also sees the status `interrupted` |
| dsh | the wrapper reports a `turn/end` with reason `aborted` as an interruption (about 30 ms after the stop) |

Neither Claude Code check can end a turn that is still running: a long
tool call leaves the tool call last in the transcript, and a long answer
leaves the prompt last (Claude Code writes an answer when it is complete),
and the status line shows `esc to interrupt` throughout both (checked with
a 96 s tool call and a 65 s answer). If a future Claude Code changed that
line, the fallback could take a running turn for an ended one; the node
would then wake the seat, and the wake-up message would wait in the
session's queue until the turn's next tool call, which shows it as usual.

`amail wake <seat>` (and Wake in the web interface) on a seat the node
believes busy looks again at once (transcript and pane, or the Codex
app-server) and says what it found, for example `c1: was busy, but its turn
had ended (the person interrupted it (its transcript says so)); now idle;
delivering its 1 waiting message(s)` or `c1: busy: its turn is running
(last hook 20 s ago; its transcript shows the turn running; its pane shows
the turn running)`.

## Settings after sleep and wake

`amail up` updates a running seat's settings on the server and names the
seats that need a restart. `amail sleep <seat>` then `amail wake <seat>`
(or `amail down --only <seat>` then `amail up --only <seat>`) starts the
seat again, resuming its session; what the resumed session takes:

| setting | Claude Code | Codex | dsh |
|---|---|---|---|
| model | yes (`--model` at each start) | yes (see below) | yes: a resumed dsh session keeps the model it had, so the wrapper selects the seat's model for it (`session/selectModel`; checked with dsh 0.2.0-rc.2). dsh refuses a model it does not offer; the session then keeps its model and the pane says so |
| permission mode, `git_access` | yes | yes (see below) | yes |
| role text | yes (the system prompt is given at each start) | no: it is part of the session's first prompt only | the overlay is written at each start (not checked with a resumed session) |
| harness | a new session is started (a session cannot move to another harness) | | |

Codex keeps a thread loaded in the shared app-server for a moment after
its session ends, and a session that resumes a loaded thread keeps the
thread's old settings and ignores `-m`. This happened when the node
started a seat again 0.1 s after `amail sleep` (it no longer does that on
its own, see [team files](teams.md)), or when `amail wake` follows at once.
So when it resumes a Codex seat the node also sets the seat's model,
approval policy and permissions profile on the thread through the
app-server (`thread/settings/update`), at once and again 3 s and 10 s
later (checked with codex-cli 0.160: wake 0.1 s after sleep, the next turn
used the corrected model). A Codex seat's new role text does not reach its
resumed session: tell the agent by mail, which `amail up` says.

## What the node does with hooks

**Hooks registered.**

| harness | events |
|---|---|
| Claude Code | `SessionStart`, `UserPromptSubmit`, `PostToolUse`, `Stop`, `StopFailure`, `Notification`, `PermissionRequest`, `SessionEnd`, `PreCompact`, `PostCompact` |
| Codex | `SessionStart`, `UserPromptSubmit`, `PostToolUse`, `PermissionRequest`, `Stop`, `Interrupt`, `SessionEnd`, `PreCompact`, `PostCompact` (codex-cli 0.160 lists all nine in `hooks/list`) |
| dsh | `SessionStart`, `UserPromptSubmit`, `PostToolUse`, `Stop`, through the Codex hook bridge (no compaction events) |

The node reads from each input only what it needs and treats a missing or
unknown field as unknown: the session id, the transcript path, `cwd`,
`model`, `source` (`SessionStart`), `prompt`, `tool_name` and `tool_input`
(`PostToolUse`), `reason` (`SessionEnd`), `trigger` (`PreCompact`) and
`compact_summary` (`PostCompact`; `summary` is also read).

**What is injected, and when.** One answer holds, in this order: the
seat's handover note, then mail (new mail in full, a digest, the unread
summary), then the task line, then the document index under
`Documents:`. Mail is fitted first; the note may then take up to two
thirds of the harness's injection budget and the index up to a third
(Claude Code and dsh: 9,000 characters, so about 6,000 and 3,000; Codex:
6,000, so about 4,000 and 2,000), each cut to what is left. The index is
cut first.

The index and the note never delay the mail. The node keeps each seat's
index and fetches it in the background (when a hook finds it older than
30 s, and after a compaction); a hook shows what has arrived. Only a
session start with no index fetched yet waits for it, at most 500 ms and
at most half of what is left of the hook's time once the mail calls are
done; the note at session start waits the same way, at most 700 ms. What
does not arrive in time is shown by the seat's next hook: the note stays
due until it is shown. After a compaction, Claude Code runs `SessionStart`
(source `compact`) a few milliseconds before `PostCompact`; the node then
fetches the note only once the compaction's draft is filed (or `PostCompact`
brought no summary, or 3 s passed), so the session is shown the draft and
the previous note is not recorded as read. When the draft comes after the
session start's wait, the next hook shows it. The end-of-turn check for the handover note has
half of what is left of the hook's time and is skipped when less than
300 ms is left.

| hook | shows |
|---|---|
| `SessionStart` | the note (sources `startup`, `resume`, `clear`, `compact`), the unread summary, the task line, the index |
| `UserPromptSubmit` | mail; the index when it changed since the seat last saw it; a note still due |
| `PostToolUse` | `to` and urgent mail; a note still due; refreshes the index in the background without showing it |
| `Stop` | blocks to show mail; or, once per turn, to ask for the handover note; the index only in an answer that blocks anyway |
| `PostCompact` | nothing; the next answer shows the whole index again |

**Handover note.** At the end of a turn the node may block the `Stop`
once with `Write your handover note now with amail handover write; then
stop.` (see [concepts](concepts.md#seat-handover) for the rules). For the
context's size it reads the end of the session's transcript: Claude Code
records `message.usage` on assistant records, and the node adds
`input_tokens`, `cache_creation_input_tokens` and
`cache_read_input_tokens` (with prompt caching `input_tokens` alone counts
only the uncached part); Codex records `token_count` events with
`info.last_token_usage.input_tokens` (cached input included) and
`info.model_context_window`. The context window comes from, in this order:
`--context-window N`, `--context-window MODEL=N` for a model whose name
contains MODEL, the window the harness recorded (Codex), the node's table
(`[1m]` models 1,000,000 tokens; Claude models 200,000; `gpt-5`, `gpt-6`
and `codex` models 272,000; `gpt-4.1` 1,000,000; `deepseek` 128,000), and
200,000. The node logs the figure and where it came from at debug level.
dsh records no usage the node reads, so on dsh seats this rule never
applies. `PreCompact` writes a worklog entry; `PostCompact` writes the
harness's summary to the worklog and files it as a draft note unless the
note was written in the current turn.

**Worklog.** The node sends the seat's worklog entries in batches, every
3 seconds while it is connected (kept in memory, at most 10,000, while it
is not): `session_start` (`source: startup`; for a new Codex session, whose
`SessionStart` comes before the first prompt registers it, at that
prompt, which also shows the seat's handover note), `session_end` (`reason:
...`), `precompact` (`trigger: auto`), `compact` (the summary, cut to
1,000 characters), `turn_start`, `turn_end` (`turn ended`, `turn ended by
the person ...` or `turn failed: <reason>`), `tool` (the tool's name and
the first 200 characters of its input: a shell command as it is, other
input as JSON; never the output) and `commit` (hash and subject: at
`Stop` the node runs `git log -1` in the session's working directory and
records the commit when it changed since the last look). Each entry
carries the session id and, when the seat is doing exactly one task, that
task.

## Limitations common to all harnesses

- **Mail cannot interrupt.** A busy agent sees mail only between tool calls
  (`to` and urgent) or at the end of its turn; a long tool call or a long
  generation delays it, and `urgent` does not interrupt either.
- **Long bodies are not inlined.** One hook shows at most 8 (Claude Code,
  dsh) or 5 (Codex) messages, each with its body only up to 800 characters; longer
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
