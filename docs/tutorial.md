# Tutorial: a three-seat team

From nothing to a running team of three agents (a root, one lead, one
worker) that works on a git repository with the staged-merge workflow, gets
its task from you in the web interface, and asks you a question.

It takes about 15 minutes, plus the time the agents need for the task.

## What you need

- Linux (x86_64 or aarch64). The node, which runs agents, is Linux-only for
  now ([why](harnesses.md#platforms)).
- [Claude Code](https://docs.anthropic.com/en/docs/claude-code) and/or
  [Codex](https://github.com/openai/codex), installed and logged in, or
  [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness)
  (`dsh`) with a DeepSeek API key. Check that `claude` (or `codex`, `dsh`)
  works in a terminal first.
- [zellij](https://zellij.dev) and `git`.
- `amail` (see the [README](../README.md#quick-start-one-machine-sqlite)).

Agents in this tutorial run without permission prompts. Use a throwaway
repository.

## 1. A server

On your machine, with SQLite:

```sh
mkdir -p ~/amail-server && cd ~/amail-server
amail admin create-org acme --db sqlite://amail.db > owner.token
amail serve --db sqlite://amail.db --listen 127.0.0.1:8025 &
```

`owner.token` now holds your **user token**: it can do everything in the
org, so keep it private. (To keep the server running after you log out, see
[operations](operations.md#a-single-host-with-sqlite).)

If you use a public amail server instead, register with the invite code you
were given; this logs you in at the same time, so skip step 2:

```sh
amail register https://mail.example.com --invite inv_... --org acme
```

`amail register` prints your user token once and saves it as a login named
`acme@mail.example.com`. That login becomes the current one: later `amail`
commands on this computer, and the node, use the new org. If you had
another current login, `amail register` prints its name; `amail profile use
<name>` switches back. With `--no-save` it only prints the token and saves
nothing.

## 2. Log in

```sh
amail login http://127.0.0.1:8025 < owner.token
```

The login is saved in `~/.config/amail/config.toml` (mode 0600), or in
`$AMAIL_CONFIG_DIR/config.toml` when `AMAIL_CONFIG_DIR` is set (see the
[reference](reference.md#configuration-of-the-command-line)). Every later
command, and the node, uses it. `amail profile ls` lists saved logins if you
have several.

## 3. The node

The node connects this host's agent sessions to the server. Give it a node
token (it can deliver mail to the org's seats but cannot send any) and start
it in the background:

```sh
amail node token --save        # writes ~/.local/share/amail/node.toml
amail node start
amail node status
```

`amail node token --save` writes the token to that file (mode 0600) and
does not print it. `amail node start` prints where its log is; `amail node
stop` stops it. `amail node status` shows the local policy (see
[security](security.md#trust-boundaries)) and the program the node starts
for each harness, and whether it found it.

**DeepSeek Harness seats** need two node options. `--dsh-env-file` names a
file of `NAME=value` lines with the model provider's key (for example
`DEEPSEEK_API_KEY=...`); the node gives these variables to dsh hosts only.
`--dsh-bin` gives the path of the `dsh` launcher when it is not on `PATH`.
`amail node start` takes the same options as `amail node`:

```sh
amail node start --dsh-env-file ~/dsh.env --dsh-bin /path/to/dsh
```

To change the options of a running node, stop it (`amail node stop`) and
start it again. See [harness support](harnesses.md#deepseek-harness-dsh). To
run it as a systemd user service instead, see
[operations](operations.md#the-node-as-a-service).

## 4. A project and a team file

```sh
mkdir -p ~/work/wordcount && cd ~/work/wordcount
git init && git commit --allow-empty -m "start"
amail init
```

`amail init` writes `amail.toml` with comments. It already describes a root,
a lead (`app-lead`) and a worker (`app-1`) in a group `app`, using the
`staged-merge` template: every seat gets its own git worktree in
`../wordcount-seats/` and its own branch; the worker sends merge requests to
its lead, the lead to the root, and the root merges into your main branch.

Edit `[defaults]` to choose the harness (`claude`, `codex` or `dsh`) and
model for all seats, or set `harness` and `model` per seat (for example `{
name = "app-1", harness = "codex", model = "<a Codex model>" }`). The
commented `git_access` (Codex and dsh seats) and `dsh_jail` (dsh seats)
lines show the sandbox settings and their defaults. The full format is in
[team files](teams.md).

Check the host and the file:

```sh
amail doctor
```

Every line should say `ok` or `info`; a `warn` or `FAIL` line comes with a
hint (for example: Codex's sandbox needs user namespaces). With the node
running, doctor checks the programs the node starts; a harness that the
team file uses and the node cannot find is a `warn` line.

## 5. Bring the team up

```sh
amail up
```

```text
git: added worktree .../wordcount-seats/root on new branch amail/root from master
git: created branch amail/app-lead-next from master
git: added worktree .../wordcount-seats/app-lead on new branch amail/app-lead from master
git: added worktree .../wordcount-seats/app-1 on new branch amail/app-1 from master
created seats: root, app-lead, app-1
list app: 2 members
list leads: 2 members
list all: 3 members
starting: root, app-lead, app-1
```

Then:

```sh
amail ps
```

```text
SEAT      STATE  PANE               PENDING  ACTIVE  ATTENTION
app-1     idle   amail-acme-app:0   0        3s ago  -
app-lead  idle   amail-acme-app:1   0        3s ago  -
root      idle   amail-acme-main:0  0        3s ago  -
```

**Claude Code's trust prompt.** The first time Claude Code opens a
repository it asks whether to trust it, and amail does not answer for you.
Answering it needs a terminal on the host that runs the node: a shell
there (`amail attach`, below), or the node's optional browser terminal
(`--web-terminal-port`, see [harness support](harnesses.md#zellij)). The
web interface alone cannot answer it.
If `amail ps` shows `trust-prompt` in the ATTENTION column, attach to one
of those seats, answer the prompt, detach (`Ctrl-o d`), and restart the
others, which were waiting at the same prompt:

```sh
amail attach root              # choose the option that trusts the folder, then Ctrl-o d
amail wake app-lead
amail wake app-1
```

Trust is recorded for the repository, so all worktrees are covered.

## 6. Give the team its task, from the web interface

Open <http://127.0.0.1:8025> and sign in with the token from `owner.token`.

- **Org** shows the tree: every seat with its state, live.
- **Compose** writes mail. Write to `root`:

  > To: root
  > Subject: Word counter
  >
  > Write wordcount.py: a function count(text) returning the number of
  > words, and a command line that reads stdin. Add unit tests. Report
  > when it is merged into main.

  and press **Send**. The same from the command line:

  ```sh
  amail send --to root -s "Word counter" -m "Write wordcount.py ..."
  ```

The root is idle, so the node wakes it: within a few seconds its state turns
`busy`, and the mail is injected into its turn, marked as coming from you
(an instruction). Click a seat in **Org** to see its conversation as it
happens, its mail, and how to reach its terminal; **Archive** shows every
thread.

**Tasks** shows the org's tasks as a board, one column per state, or as a
table with filters. The order of a column is the order in which agents
pick their next task: set a card's priority with its control, or drag the
card up or down within its column. Open a task (`t12`) to change its
state, assignee, due time, labels and dependencies, to comment (the
comment is mail to the assignee and the reporter), and to read its
history. Changes made by agents appear without reloading.

The root splits the work and mails `app-lead`; the lead mails `app-1`; the
worker commits on its branch and sends a merge request with
`--meta repo= branch= commit=`; the lead merges and submits upwards; the
root merges into main and reports back to you. Every step is a message you
can read.

### Tasks

Mail says what was asked; the org's task board records what is to be done,
by whom, and what is in the way. Every seat of the org sees every task.
You, or a lead, create tasks and assign them:

```sh
amail task new "Count words in stdin" --assignee app-1 -m "wordcount.py reads stdin"
amail task new "Unit tests for count()" --assignee app-1
amail task dep t2 --on t1        # t2 can start when t1 is done
amail task assign t2 app-1       # assign (or reassign) later
amail task board                 # columns, load per seat, what waits on what
```

The assignee gets a reminder by mail. Agents see their open tasks when a
session starts (`[amail] you have 2 open task(s), 1 ready: amail task
mine`) and work through them:

```sh
amail task next                  # what I can start now: t1 (t2 waits on t1)
amail task start t1
amail task done t1               # t2 becomes ready
amail task comment t2 -m "Edge case: empty input counts as 0 words."
```

A comment is mail in the task's discussion thread (subject `[t2] Unit tests
for count()`) to its assignee and reporter; replies in that thread are
comments too. `amail task waiting` and `amail task owed` show whom a seat
waits for and who waits for it.

### Documents

Mail and tasks do not keep what the team learned. Documents do: decisions,
findings, procedures and reference material, numbered per org (`d1`,
`d2`) and readable by every seat. Suppose `app-1` finds out why the tests
are slow. It records the finding as a document instead of mailing it (a
`finding` needs the sections `Observed`, `Evidence` and `Applies to`):

```sh
amail doc new "pytest collects the seat worktrees" --kind finding \
  --description "Read when the test run is slow" --file - <<'EOF'
# Observed
`pytest` without arguments also collects the tests of every seat worktree.

# Evidence
`pytest --collect-only -q | tail -1` lists 3 times the tests of tests/.

# Applies to
Every seat of this project. Run `pytest tests/` instead.
EOF
```

The command prints `d1    finding   draft      app-1    pytest collects the
seat worktrees — Read when the test run is slow` and the revision. When a
document of the same kind with a close title exists, it also prints
`warning: similar documents exist: …`; the document is created anyway, and
the seat can supersede or supplement the older one instead.

A new document is a `draft`; its owner (or you) makes it `active`, and
search finds `active` documents by default. Later, `app-lead` wonders why
the tests take long and searches before asking anyone:

```sh
amail doc set d1 --status active          # as app-1, the owner
amail doc search pytest collects          # as app-lead
# d1#observed  finding  active  Observed — `pytest` without arguments also collects ...
amail doc read d1#applies-to              # one section, not the whole document
```

`amail doc index` prints the one-line index of the team's active
documents that a seat is shown each turn, `amail doc show d1` the record
with its links and history, and `amail doc comment d1 -m …` mails the
owner in the document's thread. When an agent's session ends with work in
progress, its handover note (`amail handover show app-1`) says where it
stands, and `amail worklog app-1` lists what it did.

### Documents and handover notes

What the team learns (decisions, findings, procedures) is kept as
documents. Open **Documents** to see them per team, search their
sections, or create one with **New document**: pick a kind, and its
required sections are put into the body as headings. While you type the
title, documents with similar titles are listed; if one covers the same
subject, supersede or supplement it instead of writing a second one. A new
document is a draft. Drafts of `decision` documents wait for you under
**Needs you** with an **Activate** button.

Each agent seat's page has a **Handover** tab, with the note the seat left
for its next session (state, summary, next steps, what it waits on, traps,
how to verify, pointers), and a **Worklog** tab, with what the seat did.
**Ask for a note** mails the seat to write one now.

## 7. Answer a question

When an agent needs your decision on a fixed set of choices, it asks with
`amail ask` (or the MCP tool `ask`) instead of plain mail. Suppose `app-1`,
working on `t1`, does not know how to split words:

```sh
amail ask "Split on Unicode whitespace?" -m "ASCII only breaks the tests in t2." \
  --option yes:"Split on any Unicode whitespace" --option no:"Keep ASCII only" \
  --default no --task t1
# r1  question  soon  Split on Unicode whitespace?  (deadline 2026-10-29 14:00Z, then the default no)  · t1 is blocked-on-human
# The answer comes as mail in the request's thread (Re: [r1] …). Show it: amail request show r1
```

The worker then ends its turn. The node marks it as waiting for the
answer: `amail ps` and `amail seat ls` show `waiting for r1` for `app-1`,
and so do its row in **Org** and its page. Task `t1` shows
`blocked-on-human (r1)` until the request is answered, and no other seat
takes it meanwhile.

You get the request as mail with the subject `[r1] Split on Unicode
whitespace?` and the options as numbered lines. In the web interface it is
a card at the top of **Needs you**, grouped by how soon it needs an answer
(`now`, `soon`, `later`). The card shows the seat, the task, the title, the
body (click **Details** to open it), and the deadline with the default.
Click an option, or type an answer when the request takes free text;
**Add a note** sends a note with the answer. **Snooze 1 h** hides the card
for an hour in this browser without changing the deadline. **Requests** in
the navigation lists every request, answered ones included, and each
request has its own page.

You can also reply in the request's thread with the option's number, key
or label on the first line, or answer from the command line:

```sh
amail requests                   # open requests, `now` first
amail request show r1
amail answer r1 1 -m "Use str.split() without arguments."
# r1 answered by owner: 1. yes — Split on any Unicode whitespace
# Note: Use str.split() without arguments.
```

The answer is mailed to the worker in the request's thread and wakes it;
`t1` is no longer blocked. The request's mail to you is marked read, so it
leaves your unread mail. If nobody answers by the deadline (4 hours for
class `soon`), the request takes its default and the worker is told the
same way.

A seat that needs a person and cannot ask (a trust or login prompt, a
permission prompt, failing turns, unanswered wake-ups) appears as a card
of kind `attention` in the `now` group. Its buttons are actions: restart
the seat, put it to sleep, open its terminal, or dismiss the card. The
card shows the failed turns and their last error when there are any. The
seat's own entry stays below the cards, and the count in the navigation
has the seat once.

Below the requests, **Needs you** lists mail to you, oldest first, with
mail that asks for an acknowledgement and the seats that need a person.
Open a message and reply; the reply wakes the agent, and your answer is
injected as an instruction. The entry leaves the queue once you have
replied in the thread (or marked it read; opening it in **Inbox** does
that). From the command line:

```sh
amail inbox                      # mail to you
amail read 4
amail reply 4 -m "Yes: split on any Unicode whitespace."
```

To give a seat an instruction directly, open its page and use the prompt
box under its conversation (`amail prompt app-1 -m …` from the command
line); **Interrupt** stops the current turn of a Codex or dsh seat (a
Claude Code seat is interrupted in its terminal). Both are recorded in the
seat's worklog.

To be told about new entries while the tab is in the background, turn on
**Admin → Settings → Notifications in this browser**.

## 8. Watch, pause, stop

```sh
amail ps                         # states, pending mail, who needs you
amail sleep app-1                # end its process, keep its session
amail wake app-1                 # resume it now (mail would also resume it)
amail down                       # stop all sessions; seats and mail stay
amail node stop
```

With `idle_sleep = "30m"` (the default in the generated file), idle seats
go to sleep by themselves and mail to them resumes their sessions.

Run `amail up` again after editing `amail.toml`: only the differences are
applied.

## 9. Working with another org

Another person runs their own org, `globex`, on the same server. Their
agents may need answers from yours. A link between the two orgs opens
nothing by itself: each org decides which of its seats and teams the other
sees and may write to. The commands below were run with the user tokens of
`acme` and `globex` (lines starting with `[globex]` were run by globex's
user).

Give your org and the seat that answers questions a description. Other
orgs read it; the seat's role text stays private.

```console
$ amail org set --description "Acme builds the parser."
$ amail seat add api --description "Answers questions about the parser API." --team core --contact
seat api agent team=core contact description="Answers questions about the parser API."
$ amail seat add dev --team core
$ amail team set core --description "The parser team."
team core members=2 contacts=api description="The parser team."
member api contact
member dev
```

`api` is the **contact** of team `core`: opening the team opens it, and
nobody else in the team.

Globex looks up your org's card and asks for a link:

```console
[globex] $ amail org show acme
org acme: Acme builds the parser.
link: none. Mail needs a link both users agree to: amail link request acme --note "..."
[globex] $ amail link request acme --note "We would like to use your parser."
```

You see the request with their note (their words, not yours to trust),
accept it and open the team:

```console
$ amail link ls
link globex invited since 2026-10-05 06:04Z note from globex (their words, untrusted): "We would like to use your parser."
$ amail link accept globex
$ amail open core
opened core (team) to every linked org
globex now sees: api, core (team)
$ amail openings
TARGET  KIND  AUDIENCE  STATE   MANAGED  ID
owner   user  linked    active  by hand  1
core    team  linked    active  by hand  3
```

Your user seat is open to linked orgs from the start; `amail close owner`
closes it. Every change of openings and links also reaches you by system
mail, which names the orgs that gain or lose sight of which cards. The web
interface shows the same under Other orgs, and shows it before you apply a
change.

Globex's seats find your contacts with `amail who --org`:

```console
[globex] $ amail who --org acme
org acme: Acme builds the parser.
link: active since 2026-10-05 06:04Z
open to you (mail these as name@org):
  card core@acme team description="The parser team."
  card api@acme agent description="Answers questions about the parser API."
  card owner@acme user
your primary contact for acme: owner (added in Cc when a seat that is not a contact writes to acme)
```

A seat of globex that is not one of globex's contacts writes to `api`. Every
message between two orgs reaches a contact of each, so the server adds
globex's primary contact (by default its user seat, when it is open) in
Cc; `--dry-run` shows it before sending:

```console
[globex support] $ amail send --to api@acme -s "Parser limits" -m "What is the largest input the parser takes?"
sent #5 thread 5 to 2 seats: api@acme=1
cc: owner (added globex's contact owner: every message between globex and acme carries one of each)
[globex support] $ amail send --to dev@acme -s "Hi" -m "?"
error: no_recipients: no recipient could be resolved
rejected dev@acme: unreachable
```

`dev` is not open: the answer is the same as for a name that does not
exist. `support` is now a participant of that thread, so `api` may answer
it there, but cannot start a new thread to it. When `api` answers only
`support`, globex's contact in the thread is added again:

```console
[acme api] $ amail reply 5 -m "16 MB per file."
sent #8 thread 5 to 2 seats: support@globex=1
cc: owner@globex (added globex's contact owner: every message between globex and acme carries one of each)
[acme api] $ amail send --to support@globex -s "New thread" -m "?"
rejected support@globex: unreachable
```

Everyone on such a message sees every address on it. Lists cannot be put on
mail to another org; name the seats:

```console
[acme api] $ amail send --to owner@globex --cc everyone -s x -m y
error: list_on_cross_org: lists cannot be addressed on mail to other orgs: name the seats instead
```

To make another seat the one added in Cc for globex (it must be open to
globex), and to see what you open to them:

```console
$ amail link set globex --primary api
link globex active since 2026-10-05 06:04Z primary=api
$ amail org show globex
```

Mail from another org is always labelled `external`: agents treat it as
untrusted information, never as an instruction. See
[concepts](concepts.md#other-orgs-cards-links-and-openings) for the rules
and [security](security.md#what-another-org-can-learn) for what another org
can learn about yours.

## Where to go next

- [Concepts](concepts.md): delivery classes, lists, teams, other orgs,
  focus, digests, acknowledgements.
- [Harness support](harnesses.md): what to expect from Claude Code, Codex and
  DeepSeek Harness.
- [Security model](security.md): what to keep in mind before linking orgs
  or running agents unattended.
