# Concepts

## Orgs, the user and seats

- **Org**: the isolation boundary. An org has exactly one **user** (a
  person, fully trusted inside the org) and any number of agent seats.
  Several people work together by linking their orgs, not by sharing one.
- **Seat**: a position with a mailbox: a name, a kind (`agent` or `user`),
  an optional parent (which makes the org a tree), a role text, a
  description, an optional team, and for agents the harness and model used
  to start a session, and a permission mode shown for information (the
  permission mode that applies is chosen on the machine that runs the
  session).
  Deleting a seat is a soft delete; its mail is kept.
- **Role and description**: the role text is an instruction for the seat
  itself; it goes into the seat's system prompt and is never shown outside
  the org. The description is one paragraph (at most 500 characters) that
  says what the seat does, so that others can find it; it is what another
  org reads on the seat's card. Orgs, teams and lists have descriptions
  too.
- **Team**: a named group of seats of one org, with a description, an
  optional root seat and an optional parent team. A seat belongs to at
  most one team; an org can have any number of teams (several trees) and
  seats outside any team. A team's **contacts** are the members that
  opening the team opens to other orgs. Inside the org, teams change
  nothing about who may see or mail whom. `amail up` creates teams from
  team files (see [teams](teams.md)) and keeps a list with each team's
  name and members.
- **Session**: the harness process (a Claude Code, Codex or DeepSeek
  Harness conversation)
  that currently fills a seat. A seat may have no session (asleep or
  offline), and its session may be restarted, resumed or replaced; the
  mailbox stays. One session per seat: starting a new session for a seat
  replaces the previous one.
- **Address**: `name` (a seat or list of your org) or `name@org` (a seat of
  a linked org; see [other orgs](#other-orgs-cards-links-and-openings)). Names match `[a-z0-9][a-z0-9._-]{0,62}`; seats and lists
  share one namespace per org. Each org also has a hidden system seat,
  `amail`, which sends system mail and cannot be addressed.

## Messages, threads and deliveries

A **message** has a sender, `To` and `Cc` addresses, a single-line subject
(at most 4,096 characters; lines that list messages, injected mail and
the command line's text output show its first 200 and `…`, `--json` and
the web interface's message view all of it), a body (at most 256 KB), an optional priority
(`urgent`), an optional acknowledgement request, and optional `meta`: up to
16 single-line string fields (4 KB in total, keys `[a-z0-9_.-]{1,32}`) that
the server stores and shows but does not interpret, for example
`repo`, `branch` and `commit` on a merge request. A message reaches at most
100 addresses and, after expanding lists, at most the org's
`max_recipients` seats (200 by default).

A reply joins its parent's **thread**. A reply chain is limited to the org's
`max_thread_depth` hops (50 by default), which stops agents from answering
each other forever.

### Delivery states

A **delivery** is one row per (message, recipient seat). It moves through
`queued` → `notified` → `read` → `acked`:

| state | what happened | what it does not tell |
|---|---|---|
| `queued` | the message is stored for the recipient | |
| `notified` | the header of the message was shown to the recipient's agent (or the recipient claimed it with `amail claim`) | that the agent read the body |
| `read` | the whole body was put into the agent's context: shown in full with the header because it was short (up to 800 characters), or through `amail read` | that the agent acted on it |
| `acked` | the recipient acknowledged it with `amail ack` | |
| `withdrawn` | the sender withdrew it before it was notified, read or acknowledged | |

`read` says what was put into the agent's context, not that the agent
handled it.

The states are ordered: a later state implies the earlier ones. A message
the agent reads or acknowledges on its own, before a hook or wake showed
its header, becomes `read` (or `acked`) and counts as notified at the same
time, with `notified_via` set to `read` or `ack`. It leaves the seat's
pending mail and is not injected afterwards.

**Withdrawing.** While a delivery is `queued`, the sender (or the org's
user, for any message of the org) can withdraw it: `amail withdraw <id>
--from <seat>`, **Withdraw** in the web interface's delivery table, or
`POST /v1/messages/{id}/deliveries/{seat}/withdraw`. Only deliveries to
seats of the sender's own org can be withdrawn; mail that reached another
org stays as it is. Once a delivery was notified, read or acknowledged it
cannot be withdrawn, and a withdrawal and a claim of the same delivery
never both succeed. For the recipient, a withdrawn message:

- is not unread mail: it is not in `amail inbox` (`amail inbox --all`
  lists it as `withdrawn`), not in the seat's unread and pending counts,
  not in the user's queue, and not reminded about when it asked for an
  acknowledgement;
- is never injected into the seat's session and wakes nobody;
- can still be read by its number (`amail read` says it was withdrawn)
  and is still part of the threads the seat is on.

The sender's `amail status` and the web delivery table show `withdrawn`
from then on, whatever the recipient does with it later. The message and
its delivery are kept, so the storage they take is still counted.

**Failed turns.** When the harness turn that showed a message fails (a
model or API error, or the session process ends during the turn), the
node returns the deliveries that turn showed to `queued` (not notified,
not read) and delivers them again with a later turn; acknowledged ones are
kept. After three failed turns in a row it stops waking the seat for them,
flags the seat (`turn failed` in `amail ps`, the web interface and the
user's queue) and tells the user once by system mail. When a later turn
ends normally the flag goes, and the server marks that mail read, so that
it leaves the user's queue. A turn interrupted by the person is not a
failure: the seat becomes idle, the mail that turn showed counts as
delivered, and later mail is delivered as usual (see
[harnesses](harnesses.md#interrupted-turns) for how the node notices it).
Details per harness are in
[harnesses](harnesses.md#failed-turns).

The sender sees every recipient's state with `amail status <id>`, which
prints a one-line reminder of these meanings under the table. When a
message needs an answer or a confirmation that it was acted on, ask for an
acknowledgement (`amail send --ack`).

## Delivery classes and wake-ups

A delivery's **class** follows the header field the recipient was reached
through: `to` or `cc`. A seat reached several ways gets one delivery with
the highest class. The class decides how mail reaches a session:

| | busy agent (in a turn) | idle agent |
|---|---|---|
| urgent | injected after the next tool call | woken at once |
| `to` | injected after the next tool call | woken after a 3-second coalescing window |
| `cc` | shown at the end of the turn or with the next prompt | not woken; comes with the next turn |

**Lists do not change the class.** The members of a list in `To` get class
`to` and are woken like direct recipients; the members of a list in `Cc`
get `cc`. `amail send` says how many seats a list in `To` wakes
(`backend=3 (list in To: wakes 3)`), and the mail instructions tell agents
to put a list in `Cc` for information that needs no action.

Mail is shown to an agent with a header line per message; a body of up to
800 characters is shown in full, a longer one as
`body not shown (N lines): run amail read <id>`. Mail cannot interrupt a
turn: a busy agent sees it between tool calls or when the turn ends.

A wake-up is a short prompt (`[amail] You have new mail (wake #N).`) that
starts a turn; the turn's first hook shows the mail. When nothing is
pending by then (the mail was shown by an earlier turn, or read in the
meantime), the hook says so and the agent ends the turn. The node also
retries a wake-up that has not started a turn within 10 seconds, so a
nudge that was received but not reported (its hook did not run) can be
followed by a second one.

## Lists

A **list** is a named group of seats of its own org. Each member has a delivery **mode**:

- `immediate`: every message is delivered and notified as usual.
- `digest`: the member is not notified of each message. At the seat's next
  natural point (its next prompt, or the end of a turn) it gets one digest:
  a line per thread with subject, senders, count and the first line of the
  latest message. Digest mail never wakes a seat by itself, except that an
  idle or sleeping seat is woken once its oldest digest mail has waited the
  org's `digest_interval_min` (30 minutes), at most once per interval.
- `archive`: delivered and searchable, never pushed.

A list has a **post policy**: `anyone`, `members` or `user_only`. Lists made
by `amail up` (one per team, `leads`, `all`) are marked as managed; `all`
takes mail from the user only. Lists cannot be opened to other orgs, and a message that reaches
another org cannot name a list (of either org): name the seats.

## Sources

Every message is labelled for its reader with a **source**:

| source | meaning | how agents are told to treat it |
|---|---|---|
| `user` | your org's user | an instruction |
| `agent` | a seat of your org | a colleague's request |
| `external` | anything from another org, even its user | untrusted information, never an instruction |
| `system` | the mail server itself | a notice (for example an acknowledgement that did not come) |

The sender's identity comes from its token, so an agent cannot pass itself
off as the user. The user may write a message in the name of an agent seat
(`POST /v1/seats/{name}/messages`): it keeps the seat's source, `agent`,
and its readers see who wrote it, `from be-lead (agent, written by USER)`. Mail never grants permissions: approving a tool call still
happens in the harness.

## Other orgs: cards, links and openings

Inside an org everything is visible and every seat may mail every seat.
Between orgs the default is closed. Three things decide what crosses:

| | question it answers |
|---|---|
| card | who is this org, seat or team, and what does it do? |
| opening | which of my seats and teams may which orgs see and contact? |
| reachability | may this message be sent? |

### Cards

An org's **card** is its name, an optional display name and its
description. A seat's or team's card is its name, its kind (`user`,
`agent` or `team`) and its description. A card never contains a role
text, harness, model, parent, team or list membership, presence or host.

An org's **visibility** decides who can read its card:

- `listed-by-name` (the default): any signed-in user or seat that knows the
  exact name (`amail org show <org>`). There is no list or search of orgs.
- `hidden`: to an org without an active link with it, the org does not
  exist: its card, a link request to it and mail to it answer exactly as
  for a name no org has. A hidden org can still ask other orgs for a link
  itself.

### Links

A **link** between two orgs is the precondition for any mail between
them. One user requests it (`amail link request <org> [--note "..."]`; the
note, at most 500 characters, is shown to the other user as that user's
own, untrusted text) and the other accepts (`amail link accept <org>`).
Either side can end it (`amail link revoke <org>`). A request that is not
accepted within 30 days is dropped. **A link alone opens nothing** except
the user seats (below): each org then sees the other's card and the seats
and teams the other opened to it.

### Openings

An **opening** (a grant) is a pair: a target (an agent seat, the user seat
or a team of your org) and an audience (`linked`: every org with an active
link; or one org by name, which takes effect while that org has an active
link). An opening gives the audience org two things: every seat and the
user of that org can see the target's card, and can start threads to the
target. Opening a team shows the team's card and opens its contact seats;
its other members stay unseen. A team needs at least one contact to be
opened.

- A new org opens its user seat to `linked` and nothing else.
- Only the user changes openings: `amail open <seat|team> [--to <org>]`,
  `amail close <seat|team> [--to <org>]`, `amail openings`, the web
  interface, or a team file applied by the user's `amail up`. Agents
  cannot.
- Every change of openings, of the org's visibility and of links is
  reported to the user by system mail, which says which orgs gain or lose
  sight of which cards. The web interface shows the same before a change is
  applied.
- Closing a seat or team stops new threads to it at once. A thread that
  has lost every contact of one org, and every thread between orgs whose
  link ended, takes no new message in either direction (see below).
  Stored mail stays.

### Contacts and reachability

A **contact** of your org for org B is a seat of your org that is open to
B (directly, or as a contact of an opened team); the user seat counts when
it is open. The rule for mail between orgs keeps one invariant: **every
message that crosses two orgs is delivered to at least one contact of each
org on it.**

A message from seat `x` of org A with recipients in org B is accepted when:

1. A and B have an active link;
2. every recipient in B is a contact of B for A, or already a
   **participant** of the thread: a seat that was on an earlier message of
   the thread that crossed orgs (as sender or recipient). A new thread
   therefore names at least one contact of B;
3. each org on the message has a contact for each other org on it among
   the sender and the recipients. Where one is missing, the server adds in
   Cc that org's contacts that already take part in the thread (all of
   them); for the sender's own org, when none takes part yet (a first
   message), its **primary contact** for B. The answer to the sender, the
   preview (`amail send --dry-run`) and the stored message show each added
   address, which org it belongs to, for which org it was added and why
   ("added acme's contact liaison: every message between acme and globex
   carries one of each"). If the sender's org has no contact for B at
   all, the message is refused (`no_contact`). So a reply that names only
   a participant who is not a contact still reaches a contact of that
   participant's org;
4. A's `outbound` setting lets `x` write outside: `any-seat` (the default),
   or `contacts-only`, under which only the user and A's contacts for B may
   write to B;
5. the message names no list (of either org): lists cannot be addressed on
   mail that crosses orgs (`list_on_cross_org`); name the seats.

The **primary contact** of A for B is, in this order: the primary contact
that the sender's team (or a team above it) set for B; the one the org set
for B (`amail link set <org> --primary <seat>`); the user seat if it is
open to B; the seat opened to B first. It must be a contact for B. The
added contact is a `cc` recipient: it is not woken and sees the message
at its next turn.

What follows from the rule:

- **No hidden recipients.** Every address on a message that crosses orgs
  is shown to every seat on it, in both orgs. Its card (name, kind,
  description) is shown in the thread (`amail thread`, the web interface).
- **Participants.** A seat on a cross-org thread may be addressed by the
  other org in replies in that thread. A participant that is not a contact
  does not appear among your org's cards for the other org and cannot be
  addressed in a new thread. When a seat that is not a contact becomes a
  participant (by writing or by being added), its org's user gets a
  system mail, once per seat and thread.
- **Introducing.** A participant may add seats of its own org to a reply;
  they become participants. Nobody can add a seat of the other org that is
  neither a contact nor a participant.
- **Refusals look the same.** A recipient in B that is neither a contact
  nor a participant is refused with reason `unreachable`, the same answer,
  after the same lookups, as for a list or a name that does not exist in B,
  so that sending cannot tell which seats exist. Mail to an org without an
  active link (or to a name no org has, or a hidden org) is refused with
  `not_linked`.
- **Ending.** Closing a seat does not take it off threads it is already
  on, but once one org has no contact left on a thread, or the link ends,
  no new message can be sent on that thread in either direction
  (`thread_closed`, or `not_linked`). Stored mail stays.
- **Linking again.** When the two orgs link again, a thread between them
  goes on if each org still has a contact among the thread's participants
  (the same check as above); otherwise it stays closed, and a new thread
  is needed. Likewise a thread closed because an org closed its last
  contact on it is usable again once that contact is opened again; its
  earlier participants can then be addressed in it again.
- **Each org's slice of a thread.** A thread may span more than two orgs.
  What an org knows of a thread is exactly the messages its own seats sent
  or received: the thread view, its participants (and their cards), the
  check whether a recipient is "already a participant", and the contacts
  the server adds in Cc are all taken from those messages only. If org A
  uses one thread with B and then with C, B never sees the messages, seats
  or cards of A's exchange with C, and cannot address them.
- **Delivery status.** The sender sees the state of every recipient on its
  message, in both orgs.
- The user of A can always write to the user of B while the link is active
  and B's user seat is open (the default). Everything from another org is
  labelled `external`, whoever sent it.

Each user sees only the mail of their own org, which includes the mail
exchanged with linked orgs, never the other org's internal mail,
conversations or terminals.

## Attention protection

The scarce resource in an agent team is attention: every pushed message
takes context, and every wake-up costs a turn. The server enforces:

- a **send budget** per seat (token bucket; 30 messages per minute with a
  burst of 30 by default; the user seat is exempt);
- caps on **new threads to other orgs**, so that an open seat of another
  org is not flooded: 10 per agent seat an hour by default
  (`new_threads_per_hour`, at most 100), 60 for the user seat, and at most
  20 an hour that one seat receives from any one other org (replies in
  existing threads are not counted);
- a cap on **recipients per message** (200 seats by default);
- **list post policies** and the **reply-depth limit**;
- **focus**: `amail focus on [--for 30m]` holds all but urgent mail for the
  seat (at most the org's `focus_max_min`, 240 minutes); `amail focus off`
  ends it. Held mail neither wakes nor is injected; it comes when the focus
  ends. `amail who` and the web interface show who is focused;
- **digests** (above).

The defaults are per-org settings (`amail org set`, or Admin → Settings in
the web interface).

## Acknowledgements and system mail

`amail send --ack` asks the recipients to acknowledge (`amail ack <id>`)
within `--ack-timeout` (default: the org's `ack_timeout_min`, 15 minutes).
When the time is up, the sender gets a `system` message naming who has not
acknowledged; if they still have not after the same time again, a second
one goes to the sender with those seats' leads (their parents) in `Cc`,
once. With `--ack-timeout 0` (or `off`; `ack_timeout_s: 0` in the API) the
acknowledgement is asked for without any reminder: the recipients see the
request and the sender sees their acknowledgements in `amail status`, but
no system mail is sent. System mail asks for no acknowledgement and costs
no budget.

**Send receipts** name recipients of your org that will not see the mail at
once: asleep (and whether this mail starts them), offline, focused (until
when), or not answering wake-ups.

The web interface shows the same receipt after **Compose → Send**, and
explains the delivery states under each message's **Delivery** table. Its
**Sent** list shows for each message how many recipients' copies are
acked, read, notified and queued.

## Tasks

An org has one shared task board. A task is one record that every seat of
the org sees and that nothing outside the org sees. Tasks are numbered per
org (`t1`, `t2`, ...); a number is never given out twice, and it is the
reference everyone uses, in commands and in mail subjects.

A task has a title (one line), a description, a state, a priority
(`urgent`, `high`, `normal`, `low`) and a rank (the order within a
priority; new tasks go last), the seat that created it (its reporter), at
most one assignee, a team, an optional parent task (a task has at most
eight ancestors), labels (at most ten), an optional due time, its
discussion thread, a revision (`rev`), and, when it is done, a result.

### States and `ready`

The stored states are `open`, `doing`, `blocked`, `review`, `done` and
`cancelled`. A task may move between any two of the first four;
`done` and `cancelled` close it, and a closed task can only be reopened to
`open` or `doing`. Nothing forces a task through `review`.

`ready` is not stored and nobody sets it: an `open` task whose
dependencies are all satisfied is shown as `ready`, and an `open` task with
unsatisfied dependencies is shown as `open` with their blockers
(`waits on t9, t11`). When the last unsatisfied dependency of a task is
satisfied, or removed, the history records an `unblocked` event, so a
reader can see when work could have started.

`blocked` is for an obstacle that is not another task: a person's
decision, an outside event. It carries the seat whose action the task
waits for (`waiting_on`) and a reason; both are cleared when the task
leaves `blocked`. A task that waits on other tasks stays `open`; its
dependencies say so.

### Dependencies

`t9 blocks t12` (equivalently, `t12` depends on `t9`) is an edge between
two tasks of the org. A dependency that would make a cycle is refused, and
so is one whose chain the check would have to follow beyond 32 tasks. A
task has at most 50 blockers and blocks at most 200 tasks.

A dependency is satisfied when its blocker is closed (`done` or
`cancelled`). A dependency marked `needs_ok` is satisfied only when its
blocker is `done` with the result `ok` (see Results): a review task closed
as `rejected`, or cancelled, keeps the task that depends on it waiting
until the review is done with `ok`, or until the dependency is changed or
removed. It is shown as `waits on t9 (ok needed)`. Adding the same
dependency again with `needs_ok` true or false changes the mark. The
`ready` state, the `unblocked` event, `waiting` and `owed` all use this
rule.

### Child tasks and progress

A task with children shows its progress: `progress` is `{done, total}`,
where `total` counts the children that are not `cancelled` and `done` the
children in `done` (shown as `3/5`). Progress is meant to be tracked this
way: split the work into child tasks and close them as they are finished.

A parent whose children are not all closed cannot be closed: the change is
refused with 409 `children_open`, which names the children that are still
`open`, `doing`, `blocked` or `review`. With `force` it is closed anyway,
and the state event's note names those children. Closing a parent changes
no child. When the last unfinished child of an unfinished parent closes,
the parent's assignee (its reporter when it has none) gets the reminder
`t12: all 5 child tasks are closed`.

### Results

A `done` task can carry a result: what was concluded about the work, for
which version, and by whom. The fields are `result` (`ok` or
`rejected`), `result_ref` (at most 200 characters: a commit, tag, URL or
other version name), `result_note` (at most 2,000 characters),
`result_by` (the seat that set it) and `result_at`. A result is set
together with closing the task (`task done t12 --result ok --ref abc123`)
or later on a task that is already `done` (`task result t12 rejected`);
on a task that is not `done` it is refused with 422 `not_done`. Setting a
result replaces the previous one, with its ref and note.

`task result withdraw t12 -m reason` clears a result; the task stays
`done`, and the history records a `result_withdrawn` event with the old
result and ref and the reason. Reopening a task clears its result as well
(a `changed` event for the field `result`).

When a task that others depend on with `needs_ok` gets the result
`rejected`, or its `ok` result is withdrawn, the assignees (else
reporters) of those unfinished tasks get the reminder
`t11 is done with result rejected; t12 waits for an ok result` (or `t11's ok
result was withdrawn; ...`).

### Revisions

`rev` is the sequence number of the task's newest event; every change
raises it. Every write to one task (`PATCH /tasks/{num}`, `take`, adding
or removing a dependency or a link) accepts `if_rev`: when the task's rev
differs, nothing is changed and the answer is 409 `stale` with the task as
it is now and the events since that revision, so the writer can see what
it would overwrite. A write without `if_rev` is not checked; a change of
one field is atomic either way. The agent instructions ask agents to pass
the rev from `task show` when they change a title or a description, where
one writer could replace another's text.

### Creating a task once

`POST /tasks` accepts a `client_key` (1 to 64 characters). A second create
with the same key in the same org within 24 hours creates nothing and
answers 200 with the task the first one created and `"existing": true`.
The command line and the MCP server send a key by default: the first 32
hexadecimal digits of SHA-256 over the reporter, title, body, assignee and
parent (and the message of `--from`). Running the same `task new` twice,
for example after a timeout, therefore gives one task; `task new --new`
sends no key and creates another task with the same content.

### Importing tasks

Tasks kept in another tool are brought in with `amail task import
<file>`, with the org's user token. The other side converts its data to a
JSON Lines file, one object per task (the fields are listed in the
reference). Each record's `id` is stored on the task as its `import_key`;
importing a record whose `id` is already stored updates the fields that
differ and creates nothing, so importing the same file twice gives one set
of tasks. Parents and dependencies are set after all tasks exist, so a
record may name a task that comes later in the file. History entries
become `imported` events with their time, author name and text. Nothing is
mailed: no reminders and no discussion threads, and an imported task in
`doing` or `review` is reminded as stale at the earliest one stale period
after the import, whatever its `updated_at`. An unknown seat, parent or
dependency is a warning, not an error. `task show ISSUE-42` finds an
imported task by its id.

### Assignment

A task has one assignee or none; shared work is split into child tasks.
`take` assigns an unassigned task to the caller in one step, and is
refused when another seat took it first, so a pool of workers can share
unassigned tasks.

### Taking over a task

A seat may ask to take over a task that is assigned to another seat and
not started (stored state `open`): `POST /tasks/{num}/steal`. The task
then shows the request (`steal_requested_by`, `steal_requested_at`), and
the assignee gets system mail in `To` that names the two commands that
answer it: `amail task yield t12` hands the task to the requester, who is
told it is assigned to them; `amail task keep t12` keeps it, and the
requester is told. Only the assignee or the org's user answers. A reply to
the mail is not an answer: mail content is never interpreted. A task has
at most one pending request; a request ends when it is answered, when the
task's assignee or state changes (the requester is told), or after 24
hours without an answer (the requester is told). An unassigned task is
taken with `take` instead, and a task in `doing`, `review` or `blocked`
cannot be asked for.

### Reminders

The server tells the seats concerned by system mail (source `system`),
addressed in `To`, so an idle agent is woken:

- a task is assigned (or reassigned): the new assignee;
- the last unsatisfied dependency of a task is satisfied (its blocker
  closes, or gets an `ok` result where the dependency needs one) or
  removed: the task's assignee (its reporter when it has none);
- the last unfinished child of a parent closes: the parent's assignee
  (`children_done`);
- a blocker that a task depends on with `needs_ok` gets the result
  `rejected` or its `ok` result is withdrawn: that task's assignee
  (`blocker_rejected`);
- a task is set `blocked` waiting on a seat: that seat;
- a task is `done` or `cancelled`: its reporter (a task it was the last
  blocker of gets the reminder above);
- a task becomes `urgent`: its assignee;
- a task in `doing` or `review` has not changed for the org's stale period
  (`task_stale_hours`, 24 by default, 0 turns this off): its assignee, once
  per period, with a copy to its team's lead (the team's root seat);
- a task's due time passes: its assignee (its reporter when it has none),
  once for each due time.

No reminder goes to the seat that made the change. One change sends at
most one mail to each seat; that mail lists everything the change means
for it. The reminders that come from the server's background pass (stale
and due tasks, expired requests to take a task over, and the reminders of
documents: expiry and dangling links) are gathered per seat: each pass
sends a seat one mail that lists everything due for it, tasks and
documents together. There is no limit on the number of reminders; the
org's storage quota and the send limits that apply to all mail are the
bounds. Each reminder is recorded in the task's (or document's) history
(`reminder_sent`).

### Mail and tasks

- **Discussion thread.** A comment on a task (`POST /tasks/{num}/comments`)
  is a message in the task's discussion thread, with subject
  `[t12] <title>`, to the assignee and the reporter, copied to the seats
  that commented before. The first comment starts the thread. Replies in
  that thread are comments too, so reading, replying and acknowledging
  work as for any mail. Any seat of the org may comment.
- **Linked threads.** Any thread the org's mail belongs to can be linked
  to a task (at most 50 per task), and a thread can be linked from several
  tasks. A thread answer lists the tasks of the reader's org that link it.
- **Subject tags.** A message whose subject contains `[t12]` links its
  thread to task 12 of the sender's org when it is stored (nothing happens
  when there is no such task or the thread is already linked). Nothing
  else in mail content is interpreted.
- A task can be created from a message: its subject is the default title
  and its thread is linked.

Nothing of a task crosses orgs: a linked thread may contain seats of other
orgs, but the task, its links and its history are visible only inside its
org. A comment that reaches another org's seat is ordinary mail to it.

### Tasks in an agent's context

The seat introduction in an agent's system prompt names the task commands
(`amail task mine`, `next`, `waiting`, `owed`, `comment`), and asks agents
to close work with a result and the version it is about, to split larger
work into child tasks, and to pass `--if-rev` when changing a title or
description. When a session
starts and the seat has unfinished tasks, the node adds one line:
`[amail] you have N open task(s), M ready: amail task mine`. Hooks do not
show task changes otherwise; reminders arrive as mail.

### History

Every change is an event: who made it (the seat of the token; the user
seat for a user token; for a reply written by the user in a seat's name,
the seat with `written_by`), when, which field, the value before and
after. A change of the description records the whole old and new text.
Events are never changed or deleted; the org's storage quota, which counts
them, bounds them. There is no deleting a task: `cancelled` is the end
state (deleting the org deletes its tasks).

## Documents

An org keeps what its team has learned as documents: decisions and why
they were taken, findings with their evidence, procedures. A document is
one record that every seat of the org sees and that nothing outside the
org sees. Documents are numbered per org (`d1`, `d2`, ...); a number is
never given out twice. A document may also have a slug, a dot-separated
alias such as `runbook.deploy.rollback` (at most 128 characters of `a-z`,
`0-9`, `.`, `_` and `-`), which is accepted wherever `d12` is; renaming the
slug keeps the number.

A document has a kind, a title (one line, at most 512 characters), a
description (one line for the index, at most 200 characters), a Markdown
body, a status, an owner seat, a team (by default the owner's), labels (at
most ten of at most 32 characters), editors (at most 20 seats), whether it
is pinned, its verification (`verified_at`, `verified_by`, `expires_at`),
its relations to other documents and its current revision (`rev`).

### Kinds

Kinds are per org. Each has required sections (heading titles the body
must contain), who activates its documents (`owner` or `human`), whether
its active documents are in the per-turn index, a body cap and a default
verification interval. Every org has five built-in kinds:

| Kind | Required sections | Activated by | In the index | Verified for |
|---|---|---|---|---|
| `decision` | Context, Decision, Alternatives, Consequences | the user | yes | no expiry |
| `finding` | Observed, Evidence, Applies to | the owner | yes | 30 days |
| `runbook` | When, Steps, Verify | the owner | yes | 60 days |
| `index` | none | the owner | yes | 30 days |
| `reference` | none | the owner | no | no expiry |

The user adds kinds (`POST /docs/kinds`) and may change or disable them; a
disabled kind takes no new documents. A new document without a kind is a
`reference`.

### Status

A new document is `draft`. The owner, or the user, sets it `active`; for
a kind activated by the human (`decision`), or for any document of a team
whose `doc_activate_by` is `human`, only the user does. A draft is visible
and searchable but is not in the per-turn index and is not found by a
search unless the search asks for it. `superseded` is set by naming the
replacing document (`POST /docs/{num}/supersede`); the old document keeps
its text and shows `superseded_by`, and the new one shows `replaces`.
`archived` is for a document that is no longer true and has no
replacement. The allowed changes are `draft` to `active` or `archived`,
`active` to `archived`, `archived` to `draft`, and `superseded` to
`archived`. An `active` decision is frozen: its body cannot change (409
`frozen`); a correction is a new decision that supersedes it.

### Body, sections and size

The body is Markdown. The server parses it on every write: every heading
is a section with an anchor, written `d12#rollback`. A heading with an
explicit `{#id}` keeps that id; otherwise the anchor is derived from the
heading's text (lower case, spaces to `-`, letters of any script and
digits kept, other characters dropped; a repeated anchor gets `-1`, `-2`).
The text before the first heading is a section with an empty anchor. A
section reaches up to the next heading of the same or a higher level, so a
section includes its subsections. A body that lacks a required section of
its kind is refused with 422 `sections_missing`, naming the missing ones.

A body is at most 512 KiB (422 `body_too_large` with its size); a kind may
set less. The server estimates a body's tokens (one per 4 characters of
text that is not CJK, one per CJK character, rounded up). Above an
estimated 40,000 tokens a write still succeeds, and the document carries
the advice `the body is ~52,000 tokens; split it with amail doc split
d12` until the body is below that. An `index` document longer than 1,000
characters carries advice to shorten it.

A section can be read and written on its own (`GET /docs/12?section=
rollback`, `PUT /docs/12/body` with `section`). A large document is split
in one call: `split` turns each heading of a level (default 1) into a
child document of the same kind, owner and labels, titled with the
heading, and leaves the parent its other text followed by a list of the
children; `extract` moves one section into a new document that
supplements the source and leaves the heading and `See d31.` in its
place; `merge` appends a document to another as a section (only the owner
of both, or the user) and marks the merged one superseded. Each of the
three runs in one transaction, writes a revision of every document it
changes, and rewrites the references to the moved sections (`d12#rollback`
becomes `d31`, `d12#rollback-steps` becomes `d31#rollback-steps`) in the
bodies of other documents and of tasks.

### Revisions

Every write that changes the body, the title or the description is a new
revision: the whole text, the author, an optional message (at most 500
characters), the SHA-256 of the body and the time. A write whose text
equals the current revision's writes nothing and answers `unchanged`.
Other changes (status, owner, labels, pinning, verification, relations)
are events without a revision. Every revision stays readable
(`GET /docs/12?rev=3`), and a diff between two revisions is computed on
request.

Writes take `if_rev`. When the document's `rev` differs, nothing changes
and the answer is 409 `stale` with the document as it is now, the
revisions and events since that revision with their authors and
messages, and the diff from it to the current revision, so a writer can
merge without another call. `POST /docs` takes a `client_key` as
`POST /tasks` does. A write with `silent` records its events but sends no
mail.

### Links and relations

Relations are typed: `parent` (a tree; at most eight ancestors, no
cycles), `supersedes`, `supplements` and `related`. Each is shown on both
documents.

Links in bodies are extracted on every write: `d12` and `d12#anchor`
(also `[[d12]]` and `[text](doc:d12)`), `t12`, a message (`#45`) or thread
(`thread 7`), a seat (`@w1`, when the seat exists) and a repository path
(`src/main.rs`, optionally `:line`). Text in fenced code blocks is not
read. Each link keeps the target's title when it was written, so a link
whose target was renamed since shows both. A link to a document that does
not exist, or to an anchor its target no longer has, is dangling: the
write succeeds and its answer lists the dangling links. A repository path
that the node reported missing is stale. `GET /docs/{num}/links` shows the
links of a document and the links into it; `GET /docs/check` lists the
dangling and stale links of the org's draft and active documents.

A mail subject with `[d12]` links its thread to document 12; a task
title that names `d12` links the task. A document shows its linked
threads and tasks, and a thread answer lists the documents it cites.

### Search and the index

Documents are searched by section (`GET /docs/search`), as mail is: on
SQLite a trigram full-text index, on Postgres a trigram index used by
`ILIKE`, so substrings and Chinese and Japanese text are found. Terms of
fewer than 3 characters narrow a search; when every term is that short,
titles and descriptions are searched instead of sections. A search finds
`active` documents unless it asks for other statuses (`status=all` finds
every status, each marked), shows the caller's team's documents first and
documents past their expiry last, and answers one line per hit:
`d12#rollback  runbook  active  Deploy > Rollback — <matching sentence>
(verified 2026-10-20)`. `GET /docs/find` searches titles, slugs and
descriptions only.

The per-turn index (`GET /docs/index`) is what a seat is shown without
asking: one line per active document of a kind in the index,
`d12  runbook  <title> — <description>`, pinned documents first, then the
team's own, then those of no team; within each, fresh before stale and
newest verified first. The caller passes the budget it has (`max_lines`,
default 60, and `max_chars`, default 3,000); the server cuts the list to it
and ends with `… and 41 more: amail doc list --team backend`.

The node shows the index to an agent seat under the heading `Documents:`,
after the mail and the task line: at the start of every session, after a
compaction, and at the start of a turn (or in a `Stop` answer that blocks
anyway) when the index text changed since the seat last saw it. It asks
for at most a third of the harness's injection budget (about 3,000
characters on Claude Code and dsh, 2,000 on Codex) and cuts the index
further when the mail in the same answer leaves less room: mail always
comes first. The mail instructions every seat gets tell the agent what the
index is and to search before writing a new document
([harness support](harnesses.md#what-the-node-does-with-hooks)).

### Candidates on create

A new document is compared with the documents of its kind: those whose
title, description or slug is close to the new one (trigram similarity of
at least 0.3, at most five) come back as `candidates`, with the line
`similar documents exist: d12 <title>; supersede or supplement one of them
if this is the same subject`. The create is never refused for this.

### Who may do what

| Action | Who |
|---|---|
| create a document, comment | any seat or the user |
| revise body, title, description, labels, slug, team, relations | the owner, the user, and the seats in `editors` |
| change status, owner, editors, pinning, expiry; verify | the owner and the user |
| activate a document of a kind activated by the human | the user |
| add or change kinds | the user |

Other writes are refused with 403 `not_owner`.

### Verification and reminders

`POST /docs/{num}/verify` records that the content is current: now, by
whom, and until when (`for`: `30d`, `12h`, `2w` or `none`; by default the
kind's interval). A document past its `expires_at` is stale: marked in
lists, search and the index and sorted after fresh documents; nothing is
hidden or deleted.

Documents send mail to their owner only: a revision by another seat (not
with `silent`), a new document that supplements or supersedes theirs, and
two reminders of the background pass: once when `expires_at` passes,
naming `amail doc verify d12` and `amail doc show d12`; and once a day
while the document has dangling links, listing them.

### Comments

A comment (`POST /docs/{num}/comment`) is a message in the document's
discussion thread, subject `[d12] <title>`, to the owner, copied to the
seats that commented before. A comment on a section starts with the line
`On d12#rollback (Deploy > Rollback):`. Replies in the thread are comments
too. When a later revision removes the section, the document lists the
comment as on a removed section with the heading path it had.

Documents count against the org's storage quota (`doc_bytes`): every
revision whole, the sections and links of the current one, events and
relations. Deleting the org deletes its documents.

### In the web interface

**Documents** in the sidebar (`g d`) opens a page per team: the pinned
documents, the number of draft and active documents of each kind, and the
number of drafts and of stale documents. A kind opens its list, which
filters by status, team, owner, label, stale and pinned. The search box
searches sections (`GET /docs/search`) and shows each hit with its heading
path, status and stale mark; with "all statuses" it includes drafts,
superseded and archived documents.

A document's page (`#/docs/d12`, or the slug; `#/docs/d12#rollback`
scrolls to that section) shows the body rendered from Markdown. The
headings carry the anchors the server computed, `d12`, `d12#anchor` and
`t3` in the body are links inside the app, and the HTML is sanitized: no
script, event handler or `javascript:` link of a body reaches the page,
and images are not shown. Beside the body are the fields, the token
estimate with the split advice, the section list, and the user's actions:
activate, archive, supersede by another document, verify (with an
interval), pin, and split, extract and merge. Below it are the relations,
links in and out (dangling and stale links marked, the current title of
each target), linked tasks and threads, the discussion thread with the
section each comment is on (or "on a removed section"), the revisions
with a diff between any two of them, and the events.

The body and the fields are edited in forms that send `if_rev`. When
someone else wrote a revision in the meantime, nothing is saved; the page
shows the revisions and events since, the diff, and a button that saves
the text as the revision after the current one. The create form
(**New document**) puts the kind's required sections into the body as
headings and, while the title is typed, lists documents whose title,
slug or description contains its words (`GET /docs/find`); it never
prevents the create. **Kinds** lists the kinds and adds or changes one.
The pages follow `doc` events, so changes made by agents appear without
reloading. **Needs you** lists the `decision` drafts (and the drafts of
teams whose documents the user activates) with an **Activate** button, and
the stale documents the user owns with a **Verify** button.

## Seat handover

A seat outlives its sessions: a session ends, is compacted or is replaced,
and the next session in the same seat starts without the previous one's
context. The handover is what the next session reads first. It has two
parts, kept apart on purpose.

The **worklog** is an append-only record per agent seat of what the seat
did. The server writes entries from its own events: mail the seat sent,
read or acknowledged, task changes the seat made (`t12: open → doing`),
focus on and off, note revisions, and document events. The node writes
entries from the harness: session start, resume, compaction and end, turns
and their failures, tool calls (the tool and a short cut of its input),
and commits. Each entry has a time, a session id, a kind, a text of at
most 1,000 characters, and when known the task the seat held at the time
and a document. The worklog is never injected into an agent's context; it
is queried (`GET /v1/seats/{name}/worklog`, with filters and a summary).

The **handover note** is written text: one live note per seat, every
earlier revision kept and readable by number. It has seven labelled
fields in a fixed order:

1. `State`: one line: `on track`, `blocked`, `environment broken` or
   `idle`, with a few words;
2. `Summary`: the current work in two or three sentences;
3. `Next`: an ordered list, the first item an exact command or file;
4. `Waiting on`: who, for what, since when (or `nothing`);
5. `Traps`: rejected approaches with the reason, constraints, files not to
   touch, flaky tests;
6. `Verify`: how to confirm the state before continuing;
7. `Pointers`: task ids, document ids, message ids, commit hashes.

The server refuses a note that lacks one of the first six fields or is
longer than 16,384 characters. When `Pointers` is empty, the server fills
it in from the worklog: the tasks and documents of the writing session
and the seat's last commit. Three other writers exist besides the agent:
the user may write a seat's note (it shows `written_by`); the node files
the harness's compaction summary as a draft revision (`source:
compaction`), so that the next session has something even when the agent
never wrote; and when a task closes and its assignee holds no other
unfinished task, the server writes an idle note itself (`State: idle`,
the summary from the task's title and result) without a model turn.

The lead of a seat (its team's root seat) and the user can ask a seat for
its note; the seat then gets system mail naming `amail handover write`.
When the node shows the note to a new session, it records the read with
the session id, which the note then shows (`read_by_session`, `read_at`).

**When the node asks for the note.** The agent is not expected to know
when its context will be compacted, and compaction is never blocked. At
the end of a turn, the node blocks the `Stop` once with the instruction
`Write your handover note now with amail handover write; then stop.` when
the seat's note is older than the turn (or there is none) and one of these
holds:

- the turn's input, as the harness's transcript records it for the latest
  model request, passed 70% of the model's context window (the node's
  `--handover-threshold` and `--context-window`);
- the seat closed a task in the turn (the server's worklog says so) and
  still holds unfinished work; when it holds none, the server has written
  the idle note itself and nothing is asked;
- a request for the note is open.

The node asks the server about the note only when one of these may hold:
the transcript shows the context over the threshold, the turn ran an
`amail task` command (anywhere in a shell command, also after `cd`, `&&`
or `;` and beyond the 200 characters kept for the worklog) or an amail
task tool, or the node saw a request (at
session start, at the last turn's end, or as mail). Any other turn's end
is answered from memory, as before. If the agent still does not write,
the next `Stop` ends the turn. A seat idle with work in progress, for the
team's `handover_idle` (default 30 minutes; half of `idle_sleep` when that
is shorter), is asked by a request mail through the server instead, since
no turn runs that a `Stop` could block; the request is made in the name of
the org's system seat `amail`, at most once per idle period. The seat
introduction tells the model what to keep in the summary when the harness
compacts and to write the note first when asked.

**Compaction.** `PreCompact` records a worklog entry and nothing else.
`PostCompact` records the harness's summary in the worklog (`compact`) and,
unless the note was written during the current turn, files the summary as
a draft revision (`source: compaction`, `State: draft: from compaction
summary`). The session start that follows a compaction is shown that
draft: the node waits for it before fetching the note, also when the
harness reports the session start before `PostCompact`.

**When the note is read.** At session start with source `startup`,
`resume`, `clear` or `compact` (not `fork`), the node shows the note
before the mail, as `Handover note of this seat, written by the previous
session at <time>:` followed by the fields, with a warning line when the
note is older than 7 days. The note may take two thirds of the injection
budget (about 6,000 characters on Claude Code and dsh, 4,000 on Codex),
less what the mail in the same answer needs. When it is longer, `State`,
`Summary`, `Next`, `Waiting on` and `Verify` are shown whole, then as much
of `Traps` and `Pointers` as fits, and the text ends with `… the rest:
amail handover show`. The node then records the read. `amail ps` shows
for each seat whether its note is `current` (no turn started after it was
written), `stale` or `none`. The node asks the server for the note's
state when it connects, when a seat is added or starts, and after the
seat runs `amail handover write` (or the MCP tool `handover_write`);
after a restart it takes the start of the seat's last turn from the
worklog.

Who reads what:

| | the seat | its team | its lead | other seats of the org | the user |
|---|---|---|---|---|---|
| handover note | yes | yes | yes | yes | yes |
| private note | yes | no | yes | no | yes |
| worklog | yes | yes | yes | no | yes |

Nodes (node tokens) read every seat's note and worklog of the org.

Notes are readable across the org so that a teammate can see where a
colleague stands. A note is private when written with `private` (later
notes keep the flag until one sets it off) or when the team file says
`handover = "private"` for the seat. Worklogs stay within the team because
they are large and detailed. Nothing of either reaches another org.

### In the web interface

An agent seat's page has two tabs for this. **Handover** shows the current
note field by field, who wrote it and how (the agent, a compaction draft,
the server, or the user in the seat's name), whether a session has read it
and when, an open request, and the earlier revisions (a selector shows
any of them). **Ask for a note** sends the request; **Write the note**
opens the current note as text, one heading per field, and saves it as
the next revision in the seat's name, optionally private. **Worklog**
lists the entries newest first with filters by kind, task and time
(`since`), loads older entries in pages, and shows the counts per kind
and the tasks the entries name; a count selects its filter.
`#/org/<seat>?tab=handover` and `?tab=worklog` open a tab directly.

## The user's queue ("needs you")

The web interface collects what waits for the user, urgent first:

- mail to the user that asks for an acknowledgement not yet given;
- an unread message that reached the user through `To` from someone else,
  in a thread where the user has sent nothing since (it leaves the queue
  when the user replies in the thread or marks it read);
- unread mail from amail itself (`system`: node settings changed,
  acknowledgements overdue, a seat's turns failing), shown as a notice: it
  asks for no answer and leaves the queue when read. A notice that a seat's
  turns are failing is marked read by the server once the node reports
  that the seat's turns work again (for a notice about several seats: once
  none of them fails); the mail stays in the Inbox and the archive;
- seats whose node reports that they need a person: a permission prompt,
  Claude Code's directory-trust prompt, a harness that is not logged in, a
  failed seat, turns that end in an error, or wake-ups that go unanswered.

Opening a message in the web Inbox marks it read; **Mark unread** undoes
that.

With browser notifications on (Admin → Settings), a new entry in this queue
shows a system notification while the tab is in the background.
