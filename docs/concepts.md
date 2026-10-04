# Concepts

## Orgs, the user and seats

- **Org**: the isolation boundary. An org has exactly one **user** (a
  person, fully trusted inside the org) and any number of agent seats.
  Several people work together by linking their orgs, not by sharing one.
- **Seat**: a position with a mailbox: a name, a kind (`agent` or `user`),
  an optional parent (which makes the org a tree), a role text, and for
  agents the harness, model and permission mode used to start a session.
  Deleting a seat is a soft delete; its mail is kept.
- **Session**: the harness process (a Claude Code or Codex conversation)
  that currently fills a seat. A seat may have no session (asleep or
  offline), and its session may be restarted, resumed or replaced; the
  mailbox stays. One session per seat: if two sessions use the same seat,
  the most recent hook wins.
- **Address**: `name` (a seat or list of your org) or `name@org` (in a
  linked org). Names match `[a-z0-9][a-z0-9._-]{0,62}`; seats and lists
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

A **delivery** is one row per (message, recipient seat). It moves through
`queued` → `notified` → `read` → `acked`:

- **notified**: the recipient's agent has been shown the message (its
  header, and its body when it is short), or the recipient claimed it with
  `amail claim`.
- **read**: the whole message reached the recipient: through `amail read`,
  or because the body was short enough (800 characters) to be shown in full.
- **acked**: the recipient confirmed it with `amail ack`.

The sender sees every recipient's state with `amail status <id>`.

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

A **list** is a named group of seats (local, or `name@org` in a linked
org). Each member has a delivery **mode**:

- `immediate`: every message is delivered and notified as usual.
- `digest`: the member is not notified of each message. At the seat's next
  natural point (its next prompt, or the end of a turn) it gets one digest:
  a line per thread with subject, senders, count and the first line of the
  latest message. Digest mail never wakes a seat by itself, except that an
  idle or sleeping seat is woken once its oldest digest mail has waited the
  org's `digest_interval_min` (30 minutes), at most once per interval.
- `archive`: delivered and searchable, never pushed.

A list has a **post policy**: `anyone`, `members` or `user_only`. Lists made
by `amail up` (one per group, `leads`, `all`) are marked as managed; `all`
takes mail from the user only.

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

## Orgs and links

Orgs are isolated until one user requests a **link** (`amail link request
<org>`) and the other user accepts (`amail link accept <org>`). Then seats
of both orgs can write to each other as `name@org`, and lists may contain
members of the other org. Either side can end the link (`amail link revoke`).
Without an active link, mail to `name@org` is refused. Each user sees only
the mail of their own org, which includes the mail exchanged with linked
orgs, never the other org's internal mail, conversations or terminals.

## Attention protection

The scarce resource in an agent team is attention: every pushed message
takes context, and every wake-up costs a turn. The server enforces:

- a **send budget** per seat (token bucket; 30 messages per minute with a
  burst of 30 by default; the user seat is exempt);
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

## The user's queue ("needs you")

The web interface collects what waits for the user, urgent first:

- mail to the user that asks for an acknowledgement not yet given;
- an unread message that reached the user through `To` from someone else,
  in a thread where the user has sent nothing since (it leaves the queue
  when the user replies in the thread or marks it read);
- seats whose node reports that they need a person: a permission prompt,
  Claude Code's directory-trust prompt, a harness that is not logged in, a
  failed seat, or wake-ups that go unanswered.

With browser notifications on (Admin → Settings), a new entry in this queue
shows a system notification while the tab is in the background.
