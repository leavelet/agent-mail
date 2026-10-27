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
3. creates or updates the teams, the seats (tree, harness, model, role,
   description, team, contact mark), the openings the file declares, and
   the generated lists;
4. asks the node to start the sessions.

It is idempotent: run it again after editing the file and only the
differences are applied. Changed seats keep running (the output names those
that need a restart); seats that left the file are reported and deleted
only with `--prune`. Seats the node runs that were deleted on the server
(for example with `amail seat rm` on another host, or in the web
interface) are stopped and dropped from the node, and the output names
them. Changed `[team]` settings (`idle_sleep`, `max_live`,
`restart`, `local_policy`) apply at once. The output ends with the node's
local policy (see below). `--only g,s` starts just some groups or seats;
`--no-start` starts nothing.

Generated lists: one per team (its members), `leads` (every seat with
subordinates) and `all` (every agent seat of the org; only the user may
post). `leads` and `all` are computed from the seats on the server, so they
stay right when several team files are up in one org. They are marked as
managed; lists you made yourself are never touched.

### Teams

A team is an object on the server (see
[concepts](concepts.md#orgs-the-user-and-seats)): a name, a description, a
root seat, a parent team, its members and its contacts. A team file
creates its teams:

- With `[team] name = "..."`, the file is one team: its `[[group]]`s are
  sub-teams of it (nested as their parent seats are), and seats outside any
  group are members of the file's team.
- Without a name, each `[[group]]` is a team. A group whose parent seat is
  in another group is a sub-team of that group's team; seats outside any
  group are in no team.

Several team files can be up in one org (run `amail up -f` with each).
`--prune` only removes seats of the teams the file owns (for a file
without teams: seats in no team that are in one of its trees), never the
seats of another file. The `[team]` settings (`idle_sleep`, `max_live`,
`restart`, `local_policy`) are the node's: the last `amail up` sets them.
`amail team ls`, `amail team show <team>` and `amail team set <team>
[--description d] [--root seat] [--parent team] [--contacts a,b]` show and
change teams by hand.

### Openings in a team file

`open = "linked"` (every org with an active link) or `open = ["org", ...]`
on a seat, on a lead or worker, on a `[[group]]` or under `[team]` opens
that seat or team to other orgs (see
[concepts](concepts.md#openings)). An open group or team opens its
contacts: `contacts = ["seat", ...]` on the group or `[team]`; without it,
an open group's contact is its lead and an open named team's is its root
seat. `amail up` adds the openings the file declares and removes the ones
it added earlier that the file no longer declares; openings the user made
by hand (`amail open`) are left alone. The output says which orgs gain or
lose sight of which cards, and the user gets a system mail for each
change. Agents cannot change openings; only the user token used by `amail
up` can.

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
# handover_idle = "30m"          # ask an idle seat with work for its handover note (default 30m; "off")
# docs.activate_by = "human"     # every document of the teams' seats waits for you; or "owner"

[defaults]
harness = "claude"               # or "codex", "dsh"
model = "sonnet"                 # any model name the harness accepts
permission_mode = "unattended"   # or "default" (keep the harness's prompts)
# git_access = "worktree"        # Codex and dsh seats: "worktree" (default), "full" or "off"
# dsh_jail = true                # dsh seats: run unattended hosts in a bwrap jail (default)
# paste_fallback = true          # see docs/harnesses.md; off by default
# handover = "private"           # who reads the seats' handover notes; "org" (default)

[[seat]]
name = "root"
role = "Overall technical lead."
description = "Leads the project; answers questions about its plan."  # shown to other orgs when open
open = "linked"                  # or ["org", ...]: open this seat to other orgs
# handover = "private"           # its handover notes: the seat, its lead and you only

[[group]]
name = "backend"
parent = "root"                  # any seat, e.g. another group's lead
lead = { name = "be-lead", harness = "codex", model = "<a Codex model>" }
workers = { count = 3, prefix = "be", role = "..." }   # be-1..be-3, under be-lead
description = "The backend: API and storage."          # the team's description
# open = ["partner-org"]         # open the team; its contacts (default: the lead)
# contacts = ["be-lead"]
# docs.activate_by = "owner"     # this team only; default: [team]'s

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
# session = "<harness session id>"  # resume an existing conversation (see below)
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

### Documents and handover notes

`docs.activate_by` decides who moves a document from `draft` to `active`
(see [documents](concepts.md#documents)). With `"human"`, every document
whose owner is a seat of the team waits for you to activate it, whatever
its kind says. With `"owner"`, or without the setting, each document kind
decides (a `decision` always waits for you). Under `[team]` the setting
applies to every team the file declares (the file's own team and its
groups); a `[[group]]` can set its own. `amail up` applies it with `PUT
/v1/teams/{name}` (`doc_activate_by`), and removing the line from the file
sets the team back to `"owner"` at the next `amail up`.

`handover` (in `[defaults]`, a `workers` table or a seat) decides who reads
a seat's [handover notes](concepts.md#seat-handover). `"org"`, the
default, lets every seat of the org read them; `"private"` limits them to
the seat itself, its lead (the root seat of its team) and you. `amail up`
applies it to each seat of the file (`PUT
/v1/seats/{name}/handover/private`) and reports the seats it changed. The
setting holds for all of the seat's notes, whatever flag a note was
written with.

`handover_idle` (under `[team]`, a duration, default `30m`) is how long a
seat may sit idle with work in progress (an unfinished task assigned to
it) before the node asks it for its handover note, by a request mail,
when its note is older than its last turn. When the team also sets
`idle_sleep`, the node asks at half of `idle_sleep` if that comes first,
so the seat can write before it is put to sleep. `"off"` turns these
requests off. The node asks at most once per idle period and not while
an earlier request is unanswered.

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

## Starting from existing conversations

A seat can continue a Claude Code, Codex or dsh conversation that already
exists on this host instead of starting a new one. Give the seat the
conversation's session id:

- in the team file: `session = "<id>"` on a `[[seat]]`, a group's `lead`
  or a table in a group's `workers` array (not on `workers = { count =
  ... }`). `amail up` records it as the session the seat resumes when the
  seat is created and whenever the value changes; the next start of the
  seat resumes it. Removing the field changes nothing: the seat keeps the
  session it has. Writing the same id again later adopts it again.
- for a seat you start by hand: `amail run <seat> --session <id>`.
- for a seat the node already supervises: `amail seat adopt <seat> <id>`.
  A running seat keeps its current session and resumes the adopted one at
  its next start; `--restart` restarts it now (`amail sleep <seat>` then
  `amail wake <seat>` does the same).

Where to find the id: Claude Code names the session's transcript file
after it (`projects/<project directory>/<id>.jsonl` in its config
directory); Codex names the rollout file after it
(`sessions/YYYY/MM/DD/rollout-<time>-<id>.jsonl` in its home).

A seat with `session` and no `cwd` runs in the directory the session was
started in (Claude Code and Codex record it in the transcript or rollout
file), when that directory still exists; `amail up` says so. With `cwd`
set, or when the directory is gone, the seat runs where the file says, and
for a Claude Code session in another directory `amail up` warns (see
below). `amail run <seat> --session <id>` runs in the current directory.
Worktree templates fix every seat's directory.

Before it accepts an id, the node checks:

- The session belongs to the seat's harness and exists on this host:
  - Claude Code: a transcript `<id>.jsonl` under `projects/` of the
    node's Claude config directory (`$CLAUDE_CONFIG_DIR` of the node, else
    `~/.claude`), in any project directory. The node reports the directory
    the session was started in and warns when the seat runs elsewhere:
    Claude Code keeps sessions per project directory and resumes a session
    from the directory it was started in, so `claude --resume` from another
    directory can fail (not tried with a real Claude Code; if it fails,
    the seat falls back as described below). Leave the seat's `cwd` out,
    or set it to that directory.
  - Codex: a rollout file under `sessions/` of the node's Codex home
    (`<node state dir>/codex-home`) whose first line is the `session_meta`
    of that id, or, when the node's Codex app-server runs, an answer to
    `thread/read` for it. Codex seats do not use `~/.codex`: a conversation
    from your own Codex home is not found until its rollout file is copied
    to the same place under the node's Codex home. Codex then resumes the
    copied file with its history (seen with codex-cli 0.160); the thread
    does not have to be in Codex's thread store.
  - dsh: the session's directory under `sessions/` of the seat's dsh home
    (`<node state dir>/dsh-home/<seat>`).
- A session is the session of one seat only: an id that another seat of
  this node resumes or has registered is refused with that seat's name, and
  so are two seats of one team file with the same id.

`amail up` checks every new `session` before it changes anything; an error
names the seat and the reason, and nothing is changed.

On its first start as a seat, the adopted session learns that it is a
seat:

- Claude Code: every start, a resumed one included, gets the mail
  instructions and the seat intro (who the seat is, its lead, its role) in
  `--append-system-prompt`.
- Codex: a resumed thread keeps the instructions of its first prompt, so
  the node starts it with `codex resume <id> <prompt>`, where the prompt
  says that the conversation now runs as the seat, with the seat intro and
  the mail rules. A resumed thread has no registration marker; the node ties
  the thread to the seat when it adopts it.
- dsh: the overlay (system prompt) is written at every start, and the host
  also sends the same prompt as for Codex to the loaded session.

The prompt is given once: the session's first hook ends it, and later
starts resume without it. From then on the seat's session is whatever its
hooks report, as for any seat.

If Claude Code cannot resume the adopted session (`No conversation found
with session ID`), the seat starts a new session that is told its earlier
context is gone; `amail ps` flags the seat `adopt-failed` with the id until
the next adoption or `amail wake <seat>`.

## Sessions and their lifecycle

The node runs each group in its own zellij session (`amail-<org>-<group>`;
seats without a group in `amail-<org>-main`), one tab per seat, each running
`amail run <seat>`. A pane keeps its last output when its command ends.

`amail attach <seat>` attaches your terminal to the seat's session and
shows the seat's tab. `amail attach <group>` attaches to the group's
session on the tab its other clients show. If another terminal attached to
the same session has been typed in, amail cannot choose the tab of the new
one (zellij 0.45 sends tab commands to the terminal that was typed in
last); `amail attach` then says which tab to go to (`Ctrl t`, then the
arrow keys).

`amail seat rm <seat>` deletes the seat on the server and, when the node on
this host runs it, stops it and drops it from the node, so `amail ps`,
`down` and `up` no longer list it.

Seat states: `starting`, `busy`, `idle`, `awaiting` (a permission prompt),
`dormant` (asleep), `offline`, `restarting`, `failed`.

- A seat idle for `idle_sleep` with nothing pending, or the least recently
  active idle seats when more than `max_live` run, or `amail sleep`, is put
  to sleep: its process ends; its session id and pane are kept.
- `to` or urgent mail for a sleeping seat (or `amail wake`) starts it again
  in its pane, resuming its session; the mail then arrives as usual. A
  seat the person put to sleep (`amail sleep`, or Sleep in the web
  interface) stays asleep with the mail already waiting for it; mail that
  arrives after the sleep, `amail wake` or `amail up` starts it again (this
  survives a node restart; mail that arrives while the node is down is
  only noticed at the next new mail). A sleeping seat whose turns failed
  three times in a row is not started for mail either (`amail wake` does). At most
  4 sessions start at a time (`amail node --start-limit`), so mail to
  everyone does not start dozens of processes at once.
- A process that ends without `amail down` or sleep is restarted (resuming)
  after 2 s, doubling per consecutive crash; after 3 restarts the seat is
  `failed`. A clean exit (status 0, e.g. `/exit`) is not a crash: the seat
  goes offline.
- A seat whose session cannot be resumed starts a new session that is told
  its earlier context is gone.
- `amail ps` flags seats that need a person, with what to do.
