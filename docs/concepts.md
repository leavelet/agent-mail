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
(at most 300 characters), a body (at most 256 KB), an optional priority
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

`read` says what was put into the agent's context, not that the agent
handled it.

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
off as the user. Mail never grants permissions: approving a tool call still
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
once. System mail asks for no acknowledgement and costs no budget.

**Send receipts** name recipients of your org that will not see the mail at
once: asleep (and whether this mail starts them), offline, focused (until
when), or not answering wake-ups.

The web interface shows the same receipt after **Compose → Send**, and
explains the delivery states under each message's **Delivery** table. Its
**Sent** list shows for each message how many recipients' copies are
acked, read, notified and queued.

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
