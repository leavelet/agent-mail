# Team files and `amail up`

A team file (`amail.toml`) describes the seat tree and how this host runs
it. The org comes from your user token (`amail login`). `amail init` writes
a commented starting point in the current directory (a root, one lead and
one worker with the staged-merge template).

## `amail up`

`amail up` (with a user token and a running node on this host):

1. checks the file; errors name the entry, and nothing is changed;
2. prepares working directories (for staged-merge: git worktrees and
   branches);
3. creates or updates the seats (tree, harness, model, role, group) and the
   generated lists;
4. asks the node to start the sessions.

It is idempotent: run it again after editing the file and only the
differences are applied. Changed seats keep running (the output names those
that need a restart); seats that left the file are reported and deleted
only with `--prune`. Changed `[team]` settings (`idle_sleep`, `max_live`,
`restart`, `local_policy`) apply at once. The output ends with the node's
local policy (see below). `--only g,s` starts just some groups or seats;
`--no-start` starts nothing.

Generated lists: one per group (its lead and workers), `leads` (every seat
with subordinates) and `all` (every seat; only the user may post). They are
marked as managed; lists you made yourself are never touched.

Related commands: `amail ps` (one line per seat: state, pane, pending mail,
activity, and whether a person is needed), `amail attach <group or seat>`,
`amail sleep <seat>`, `amail wake <seat>`, `amail down [--only g,s]`.

## Reference

```toml
[team]
template = "staged-merge"        # built-in, or a template directory; optional
repo = "/path/to/repo"           # needed by staged-merge (relative paths are relative to this file)
workroot = "/path/to/worktrees"  # seats' working directories go here
idle_sleep = "30m"               # put idle seats to sleep; "off" (default)
max_live = 40                    # seats with a process on this host
restart = "on-failure"           # or "never"
# local_policy = "restricted"    # see "Agents that manage seats" below

[defaults]
harness = "claude"               # or "codex", "dsh"
model = "sonnet"                 # any model name the harness accepts
permission_mode = "unattended"   # or "default" (keep the harness's prompts)
# git_access = "worktree"        # Codex and dsh seats: "worktree" (default), "full" or "off"
# dsh_jail = true                # dsh seats: run unattended hosts in a bwrap jail (default)
# paste_fallback = true          # see docs/harnesses.md; off by default

[[seat]]
name = "root"
role = "Overall technical lead."

[[group]]
name = "backend"
parent = "root"                  # any seat, e.g. another group's lead
lead = { name = "be-lead", harness = "codex", model = "<a Codex model>" }
workers = { count = 3, prefix = "be", role = "..." }   # be-1..be-3, under be-lead

[[group]]
name = "core"
parent = "root"
lead = { name = "core-lead" }
workers = [                      # or one table per worker, harnesses mixed
  { name = "core-1" },
  { name = "core-2", harness = "codex", model = "<a Codex model>" },
]

[[seat]]
name = "core-3"
group = "core"                   # joins the group (under its lead unless `parent` is set)

[[seat]]
name = "docs"
parent = "root"
cwd = "docs"                     # relative to the team file
role_file = "roles/docs.md"
```

`permission_mode = "unattended"` runs agents without permission prompts
(Claude Code: `--dangerously-skip-permissions`; Codex: `-a never` inside its
sandbox; DeepSeek Harness: no prompts, with the whole host in a bwrap jail;
see [harness support](harnesses.md)). Use it only in a repository
you can afford to lose. With `"default"`, or without `permission_mode`, the
harness asks as usual; answer in the seat's pane (`amail attach <seat>`);
such seats show `awaiting` and appear in the web interface's queue.

The team file is what decides the permission mode: `amail up` hands it to
the node, which applies it at each start of the seat's session (a change
takes effect at the next start). `amail up` also writes it into the seat's
settings on the server, but only for display: a permission mode set on the
server (in the web interface or with `amail seat set`) does not change how
the node starts a session, so a server cannot turn off your prompts. When
the server says `unattended` and the team file does not, the node keeps
the prompts and logs a notice. After upgrading from a version where the
server's setting decided, run `amail up` once so that the node learns each
seat's mode from the team file.

`git_access` (Codex and dsh seats; in `[defaults]`, a `workers` table or a seat)
sets what a session's sandbox may write in the git repository. The default,
`"worktree"`, lets a seat commit, rebase and merge in its own linked
worktree while the repository's git config and hooks stay read-only.
`"full"` also lets it write the whole git directory and the main checkout,
which a seat needs to merge in the main checkout; such a session can make
git run commands outside its sandbox. `"off"` grants nothing. The
staged-merge template gives the root seat `"full"` when it has
subordinates, unless the file sets `git_access` for it; `amail up` names
every Codex seat with `"full"`. See [harness support](harnesses.md) for the
exact rules.

`dsh_jail` (DeepSeek Harness seats; in `[defaults]`, a `workers` table or a
seat) decides whether an unattended dsh seat's host runs in a bwrap jail
that hides your home directory and limits writes to the seat's working
directory and git grants. The default is `true`. With `false` the seat runs
without the jail, and its agent can read and write every file your
operating-system user can, saved logins and keys included; `amail up`
names such seats, and the node reports the change to you by system mail.
`amail run --dsh-jail on|off` sets it for one start. See
[harness support](harnesses.md#deepseek-harness-dsh).

### Agents that manage seats

Whether an agent may run `amail up`, `amail ps`, `amail sleep`, `amail wake`
and the other seat commands depends on the node's local policy
(`amail node --local-policy`, see
[security](security.md#trust-boundaries)). Under `open`, agents of the
sessions the node launched may use these commands, and the user gets a
system mail that lists what each session did (at most one per session
every five minutes). Under `restricted`, the node refuses these commands
to sessions. `local_policy = "restricted"` under `[team]` makes the node
use the restricted policy while the team is up; the team file cannot make
the policy open. `amail up` and `amail ps` print the policy in effect.

## Templates

Templates decide working directories and role texts. Role texts go into
every session's system prompt, so they are short.

- **`staged-merge`** (built in): every seat gets a git worktree
  `workroot/<seat>` of `repo` on branch `amail/<seat>`; seats with
  subordinates also get an integration branch `amail/<seat>-next` (the root
  integrates into the main branch, in the main checkout). The role texts say
  who the seat's maintainer and subordinates are, how to submit (a worker
  rebases its branch onto its maintainer's integration branch, tests, then
  mails the maintainer with `--meta repo= branch= commit=`), and how to
  handle merge requests: merge into the integration branch, never rebase it
  (catch up by merging the parent's branch into it), reply
  "merged as <commit>". Maintainers forward requirement and interface
  changes to every affected member of their team and wait for confirmation.
- **No template**: the working directory is `cwd` (or `workroot/<seat>`),
  the role is `role` or `role_file`, plus who is above and below the seat.
- **A directory** with `root.md`, `lead.md`, `worker.md` and optionally
  `template.toml` (`workdir = "worktree"` or `"none"`). Placeholders:
  `{seat}`, `{parent}`, `{subordinates}`, `{group}`, `{workdir}`,
  `{branch}`. Templates are text only.

## Sessions and their lifecycle

The node runs each group in its own zellij session (`amail-<org>-<group>`;
seats without a group in `amail-<org>-main`), one tab per seat, each running
`amail run <seat>`. A pane keeps its last output when its command ends.

Seat states: `starting`, `busy`, `idle`, `awaiting` (a permission prompt),
`dormant` (asleep), `offline`, `restarting`, `failed`.

- A seat idle for `idle_sleep` with nothing pending, or the least recently
  active idle seats when more than `max_live` run, or `amail sleep`, is put
  to sleep: its process ends; its session id and pane are kept.
- `to` or urgent mail for a sleeping seat (or `amail wake`) starts it again
  in its pane, resuming its session; the mail then arrives as usual. At most
  4 sessions start at a time (`amail node --start-limit`), so mail to
  everyone does not start dozens of processes at once.
- A process that ends without `amail down` or sleep is restarted (resuming)
  after 2 s, doubling per consecutive crash; after 3 restarts the seat is
  `failed`. A clean exit (status 0, e.g. `/exit`) is not a crash: the seat
  goes offline.
- A seat whose session cannot be resumed starts a new session that is told
  its earlier context is gone.
- `amail ps` flags seats that need a person, with what to do.
