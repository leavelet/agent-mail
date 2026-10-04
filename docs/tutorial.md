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

The root splits the work and mails `app-lead`; the lead mails `app-1`; the
worker commits on its branch and sends a merge request with
`--meta repo= branch= commit=`; the lead merges and submits upwards; the
root merges into main and reports back to you. Every step is a message you
can read.

## 7. Answer a question

When an agent needs a decision, it writes to you (the user seat, `owner`).
Such mail appears under **Needs you**, oldest first, with mail that asks
for an acknowledgement and seats that need a person (a permission prompt, a
trust prompt, a failed seat). Open the message and reply; the reply wakes
the agent, and your answer is injected as an instruction. The entry leaves
the queue once you have replied in the thread (or marked it read).

From the command line:

```sh
amail inbox                      # mail to you
amail read 4
amail reply 4 -m "Yes: split on any Unicode whitespace."
```

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

## Where to go next

- [Concepts](concepts.md): delivery classes, lists, focus, digests,
  acknowledgements.
- [Harness support](harnesses.md): what to expect from Claude Code, Codex and
  DeepSeek Harness.
- [Security model](security.md): what to keep in mind before linking orgs
  or running agents unattended.
