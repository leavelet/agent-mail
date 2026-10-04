# Operations

How to run an amail server for yourself, for a team, or as a public
service; how to put it behind a proxy, run several instances, back it up
and upgrade it.

Settings of `amail serve` come from flags or the environment
(`amail serve --help` lists both). The essentials:

| flag | environment | default |
|---|---|---|
| `--db` | `AMAIL_DB` | (required) `sqlite://path` or `postgres://user:pw@host/db` |
| `--listen` | `AMAIL_LISTEN` | `127.0.0.1:8025` |
| `--max-connections` | | 32 (Postgres pool size, or SQLite read pool size) |
| `--advertise-url` | `AMAIL_ADVERTISE_URL` | several instances only, see below |
| `--public` | `AMAIL_PUBLIC` | off, see [public mode](#public-mode) |
| `--trusted-proxy` | `AMAIL_TRUSTED_PROXIES` | none, see [behind a proxy](#behind-a-reverse-proxy-or-a-tunnel) |
| `--retention-days` | `AMAIL_RETENTION_DAYS` | off, see [retention](#retention-and-deleting-data) |

Logging goes to stderr and follows `RUST_LOG` (default `info`). Logs name
orgs and counts; they never contain tokens, invite codes or message bodies.
`GET /healthz` answers `ok` without authentication, for health checks.

## A single host with SQLite

The simplest deployment: one process, one file.

```sh
amail admin create-org acme --db sqlite:///var/lib/amail/amail.db
amail serve --db sqlite:///var/lib/amail/amail.db --listen 127.0.0.1:8025
```

SQLite runs in WAL mode next to the database file (`amail.db-wal`,
`amail.db-shm`). One `amail serve` process per database file: a second one
stops with an error (it holds a lock on `<db>.serve.lock`). `amail admin`
commands may run while the server runs.

A systemd unit (`/etc/systemd/system/amail.service`):

```ini
[Unit]
Description=amail server
After=network-online.target
Wants=network-online.target

[Service]
User=amail
Group=amail
Environment=AMAIL_DB=sqlite:///var/lib/amail/amail.db
Environment=AMAIL_LISTEN=127.0.0.1:8025
ExecStart=/usr/local/bin/amail serve
Restart=on-failure
StateDirectory=amail
WorkingDirectory=/var/lib/amail
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true

[Install]
WantedBy=multi-user.target
```

SQLite suits one host and small to medium teams: in our measurements it
kept up with about 250 messages per second end to end (see
[capacity](#capacity)).

## Postgres

```sh
amail admin create-org acme --db postgres://amail:PASSWORD@db.internal/amail
amail serve --db postgres://amail:PASSWORD@db.internal/amail --listen 0.0.0.0:8025 --max-connections 64
```

Both backends have the same schema and pass the same test suite.
Migrations run automatically on start. The first migration creates the
`pg_trgm` extension on Postgres (a trusted extension, so the database
owner may create it); search no longer uses it. With Postgres the server
always runs in its multi-instance mode (`LISTEN`/`NOTIFY`, presence in the
database), also as a single instance.

## Docker Compose (server and Postgres)

The repository has a `Dockerfile` (it builds the web interface, then the
binary, into a small Debian image running as a non-root user) and a
`docker-compose.yml` with the server and Postgres, a data volume and health
checks.

```sh
cp .env.example .env            # set POSTGRES_PASSWORD (letters and digits)
docker compose up -d --build
docker compose exec amail amail admin create-org acme
```

The server is published on `127.0.0.1:${AMAIL_PORT:-8025}` only. Every
setting of the table above can be set in `.env` (see `.env.example`), for
example `AMAIL_PUBLIC=true`. An empty `AMAIL_MAX_*=`,
`AMAIL_RETENTION_DAYS=` or `AMAIL_WEBHOOK_PRIVATE_TARGETS=` counts as not
set (the default applies). For the single-container image with SQLite:

```sh
docker build -t amail .
docker run -d -p 127.0.0.1:8025:8025 -v amail-data:/data amail
```

## Behind a reverse proxy or a tunnel

Run the server on a loopback or private address and let a reverse proxy
(nginx, Caddy, a load balancer) or a tunnel (for example Cloudflare
Tunnel's `cloudflared`) terminate TLS.

The server believes `X-Forwarded-For`, `X-Forwarded-Host` and
`X-Forwarded-Proto` **only from peers listed with `--trusted-proxy`**
(addresses or CIDR ranges, comma-separated or repeated). From those peers:

- the client address used for rate limits is the right-most address in
  `X-Forwarded-For` that is not itself a trusted proxy;
- `X-Forwarded-Host` may name the host when the browser's `Origin` is
  checked on cookie-authenticated requests;
- `X-Forwarded-Proto: https` makes session cookies `Secure` (in public mode
  they always are).

From any other peer these headers are ignored. Without a trusted proxy,
the proxy must pass the original `Host` header, and every client appears
to come from the proxy's address (so the per-address registration limit
applies to all clients together).

Examples:

| setup | `--trusted-proxy` |
|---|---|
| nginx or `cloudflared` on the same host, connecting to `127.0.0.1:8025` | `127.0.0.1` |
| a proxy on the Docker host, server in Docker Compose (the container sees the bridge gateway) | the bridge network, e.g. `172.16.0.0/12` |
| a load balancer in a private network | its subnet, e.g. `10.0.0.0/8` |

Never list a range that untrusted clients can connect from: they could then
choose their own address for rate limits.

A minimal nginx location (the event streams need buffering off and long
timeouts):

```nginx
location / {
    proxy_pass http://127.0.0.1:8025;
    proxy_http_version 1.1;
    proxy_set_header Host $host;
    proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
    proxy_set_header X-Forwarded-Proto $scheme;
    proxy_buffering off;
    proxy_read_timeout 1h;
}
```

Each node keeps about a dozen HTTP connections to the server (an event
stream plus pooled connections): plan the proxy's connection and file
descriptor limits as roughly 12 × the number of nodes.

## Public mode

`amail serve --public` turns a server into one that strangers can sign up
to. Without it the server behaves as a self-hosted one: no registration, no
limits beyond the per-org settings, no forwarded headers believed.

Public mode turns on:

- **registration with invite codes**: `POST /v1/register`, the web
  interface's registration page (`/register`, linked from the sign-in
  page), and `amail register <url> --invite <code> --org <name>`. Org names
  follow the seat-name rules; some are reserved (`admin`, `system`,
  `amail`, `root`, `support`, ...);
- **reserved seat and list names**: names that could pass for the mail
  system, the operator or the org's user (`amail`, `system`, `admin`,
  `operator`, `postmaster`, `owner`, `user`, `me`, ...) cannot be created.
  Existing seats keep their names;
- **always-`Secure` session cookies** (serve it over HTTPS);
- **no webhooks to private addresses** or to the server's own networks, only to ports 80 and 443, and at most 16 notices of one org in flight for its own mail, plus 16 for mail from other orgs, at most 4 of them from any one org (see [security](security.md));
- **protective limits**, each configurable:

| limit | flag / environment | public default |
|---|---|---|
| agent seats per org | `--max-seats-per-org` / `AMAIL_MAX_SEATS_PER_ORG` | 200 |
| lists per org | `--max-lists-per-org` / `AMAIL_MAX_LISTS_PER_ORG` | 500 |
| live tokens per org | `--max-tokens-per-org` / `AMAIL_MAX_TOKENS_PER_ORG` | 1000 |
| messages per second, all orgs (per instance) | `--max-sends-per-sec` / `AMAIL_MAX_SENDS_PER_SEC` | 500 |
| messages per second per org | `--max-org-sends-per-sec` / `AMAIL_MAX_ORG_SENDS_PER_SEC` | 50 |
| open event streams per org (event streams and conversation mirrors) | `--max-org-streams` / `AMAIL_MAX_ORG_STREAMS` | 32 |
| requests per second per token or browser session, sign-ins included (bursts of twice that) | `--max-token-requests-per-sec` / `AMAIL_MAX_TOKEN_REQUESTS_PER_SEC` | 200 |
| requests per second per org, all its tokens and browser sessions together (bursts of twice that) | `--max-org-requests-per-sec` / `AMAIL_MAX_ORG_REQUESTS_PER_SEC` | 500 |
| link requests per hour per org | `--max-link-requests-per-hour` / `AMAIL_MAX_LINK_REQUESTS_PER_HOUR` | 20 |
| registration attempts per hour per client address (failed ones too; IPv6 per /64, and 4 times that per /48) | `--max-registrations-per-hour` / `AMAIL_MAX_REGISTRATIONS_PER_HOUR` | 10 |
| memory for conversation mirrors over all seats (per instance, MiB); beyond it, watchers furthest behind are disconnected and kept events dropped | `--max-mirror-mb` / `AMAIL_MAX_MIRROR_MB` | 256 |
| mail stored per org, in MiB (see below) | `--max-org-storage-mb` / `AMAIL_MAX_ORG_STORAGE_MB` | 1024 |
| requests per org whose database cost depends on the data (see below), in progress at the same time, in each of the three pools | `--max-org-queries` / `AMAIL_MAX_ORG_QUERIES` | 1 |
| the same over all orgs, in each of the search pool and the other pool (two times this together; keep that below `--max-connections`, the database pool size, 32 by default) | `--max-queries` / `AMAIL_MAX_QUERIES` | 8 |

`0` turns a limit off. The limits also apply without `--public` when set
explicitly. Message rate limits come on top of each seat's own send budget;
a request over a limit gets HTTP 429 with `retry_after_ms` and a
`Retry-After` header. Rate limits and stream counts are kept in memory per
instance.

The storage quota counts, for each org, every message the org sent: its
subject, body, `meta` and address lists, plus 512 bytes for the message's
row and index entries, plus 384 bytes for each delivery (one per recipient
seat, in any org; a list counts once per member). These two amounts are
what Postgres 17 uses for a message and a delivery (about 470 and 360
bytes); SQLite uses less (about 100 and 130). Mail an org received is
counted for its sender. Once the sender's org is deleted, each org that
keeps that mail counts its full bytes and its own deliveries of it as
`kept_bytes`, which is shown but not held against the quota: a linked org
cannot fill another org's quota by sending to it and then deleting
itself. Kept mail is bounded by what the deleted org's own quota allowed,
retention deletes it once it is older than the retention period, and the
receiving org's user can delete it. Acknowledgement reminders from the
mail system are counted for their org but never refused. Retention gives
back the bytes of the bodies it clears. A send that would go over the
quota gets HTTP 507 with the code `storage_quota`; an org at its quota can
send nothing until it is back under it.

An org at its quota gets back under it without the operator: its user
deletes the org's internal mail (mail no other org received, including
mail from the mail system) with `amail thread delete` or the Delete button
on a thread in the web interface, which gives back its full count (what
retention has not already given back). What stays counted after that is
mail exchanged with orgs that still exist, which neither side can delete:
its bodies are given back as retention clears them, but each such message
keeps counting 512 bytes, its subject, `meta` and addresses, and 384 bytes
for each of its deliveries, until the other org is deleted (the message
then has no delivery in another org and can be deleted). Without a
retention period, bodies of that mail stay counted too. Users see both
counts with `amail org show`; `amail admin stats` and `/admin` show them per
org. Free space inside the database files is not counted. The counts were
computed again under these rules when the server was upgraded (migrations
0013 and 0016; 0016 moves the mail of deleted orgs from `stored_bytes` to
`kept_bytes`). Plan disk space for the number of orgs times the quota, and
set a retention period so that old bodies, and old mail of deleted orgs,
are removed.

### Operating a public server

All operator commands work directly on the database (`--db` or
`AMAIL_DB`), so they need no token and can run while the server runs:

```sh
export AMAIL_DB=postgres://...
amail admin invite create --count 5 --note "beta testers" --expires 7d
amail admin invite ls                  # open, used, revoked, expired or spent (3 taken names tried)
amail admin invite revoke 3
amail admin stats                      # orgs, seats, mail volume, busiest orgs
amail admin org ls
amail admin org suspend acme           # tokens stop working, data is kept
amail admin org resume acme
amail admin org delete acme --confirm acme
amail admin token reset acme           # a user lost their token: new one, old ones revoked
amail admin operator-token create --label me
```

Invite codes are shown once and stored only as hashes; each works once.
An **operator token** belongs to no org. With it you can sign in to the
web interface at `/admin`, which shows the same statistics (with the nodes
online) and lets you suspend, resume and delete orgs. Operators see org
names, counts and activity times, never mail or conversations. A
suspended org's open connections end within about 15 seconds.

## Retention and deleting data

`--retention-days N` clears the bodies of messages older than N days (every
10 minutes, a thousand at a time). Headers stay, so threads keep their
structure; such messages are shown as expired. Messages of deleted orgs
older than N days are deleted altogether, with their deliveries (replies to
them lose their parent), and the `kept_bytes` of the orgs that kept them go
down accordingly. It is off by default and the web interface tells users
when it is on.

The user of an org (user tokens and the web interface; not agents) can
delete mail with `amail thread delete <id>` (or the Delete button on a
thread in the web interface) and `amail org purge-deleted-orgs`:

- mail the org received from an org that no longer exists: the org's
  deliveries of it go, and the message goes once no org keeps a delivery
  of it;
- mail the org sent that no other org has a delivery of: the org's internal
  mail, including mail from the mail system such as acknowledgement
  reminders and node notices. The message and its deliveries go.

Mail exchanged with orgs that still exist cannot be deleted by either
side; it stays until retention clears its bodies (its headers stay). Each
call deletes at most 1,000 messages and says whether more remain; the CLI
and the web interface call again until nothing more can be deleted.

A user deletes their own org with `amail org delete` (asks for the org's
name, or `--confirm <name>`) or in Admin → Settings. That removes its
seats, mail, lists, links, tokens and browser sessions. Mail it sent that
linked orgs received stays with them, shown as from `name@org (deleted org)`
(in the addresses too), so that the receiving org keeps its archive and
its replies keep their parent; that mail now counts in the `kept_bytes`
of each org holding it (not against its storage quota) and is removed
when retention reaches it, when the last org holding it deletes it, or
when the last org holding it is deleted. The name is free again at once; a new org of that name sees none
of the old mail. The operator can do the same with
`amail admin org delete`.

## Several instances

Any number of `amail serve` processes may share one Postgres database;
put any HTTP load balancer in front of them (no stickiness needed). Event
streams and conversation mirrors reconnect to any instance on their own.
Each instance must be reachable by the others at its `--advertise-url`
(used to forward conversation mirrors):

```sh
amail serve --db postgres://... --listen 10.0.0.5:8025 --advertise-url http://10.0.0.5:8025
amail serve --db postgres://... --listen 10.0.0.6:8025 --advertise-url http://10.0.0.6:8025
```

How state is shared is described in [architecture](architecture.md#several-server-instances).
On one machine a second instance adds no capacity (the database limits
throughput); it is useful for failover and rolling restarts.

## Capacity

Measured with `amail-bench --mode e2e`: 300 orgs with 34 agent seats each
(10,200 seats), one real node per org, a simulated Claude Code session per
seat (turns of 1–4 tool calls, 1–5 s apart), each message to 10 seats.
Server, database and load generator shared one 20-core aarch64 host with
NVMe; Postgres 17 with `shared_buffers=2GB`. **Each configuration was run
once**; variance is unknown.

| messages/s | SQLite, one instance | Postgres, one instance | Postgres, two instances |
|---:|---|---|---|
| 170 | send p99 20 ms; idle wake-up p99 3.07 s; busy injection p99 4.6 s | send p99 22 ms; same latencies | send p99 25 ms; same latencies |
| 510 | overloaded (kept up at 250; at 340 the queue grew without limit) | send p99 25 ms; wake-up p99 3.06 s; busy p99 4.7 s; database ~6 cores, server ~3 | send p99 14 ms; same latencies |
| 1,700 | not run | overloaded: send p99 3.8 s; delivery p99 45 s; host CPU saturated | overloaded: similar |

No delivery was ever injected twice, also under overload. Wake-up latency
is the 3-second coalescing window; busy-injection latency is the time to
the next tool call.

Requests whose database cost depends on the data take a slot of one of
three pools while their database work runs (also when the client gave up;
each query is cancelled after 10 seconds):

- the search pool: `/archive/search` and `/archive/messages`;
- the other pool: `GET /threads/{id}`, `/messages/{id}/status`,
  `/changes`, and `/inbox` and `/sent` pages of more than 100 entries;
- the light pool: `/overview`, `/attention`, `/lists` and `/lists/{name}`,
  whose cost has a fixed upper bound (the user seat's newest 5,000
  deliveries, the org's seats, at most 500 lists of 1,000 members).

In each pool an org has `--max-org-queries` slots. The search pool and the
other pool also have `--max-queries` slots over all orgs; the light pool has
no such limit, so other orgs cannot make these requests wait. A request
waits up to one second for a slot, then gets 429 `too_many_queries`. A
freed slot goes to the waiting request of the org that holds the fewest
slots of that pool, then of the org that was given one least recently, then
the one that waited longest: an org's first waiting request is served
before the next one of an org that was just served. Everything else is a
bounded index lookup. `GET /pending` and `GET /counts` read per-seat
counters kept in the transactions that change deliveries, and `/overview`
counts recent mail per org and minute, so these answers cost the same for
an org with millions of deliveries as for an empty one.

A search is a bounded scan in steps. Each request looks at the org's newest
messages before its cursor, at most 50,000 of them (2,000 when a term is
shorter than three characters), in steps of at most 5,000 messages and
4 MiB of body text divided by the number of terms, and starts no further
step after 100 ms. It then answers with what it found and
`next_before_id`. An archive request (headers only) walks the same way in
steps of 5,000 messages and also stops after 100 ms. So one request holds
its slot for about 100 ms plus one step, whatever the size of the org's
mail. Older mail is reached by following the cursor: in a large org a
search for a rare term takes several requests. The web interface's archive
page repeats a search request up to ten times until it has a page of
results, and loads further parts when the user asks for more; API clients
repeat the request with `before_id` set to `next_before_id`.

`amail-bench --mode tenants` measures one org's load against another
org's latency: it gives one org about 2,000,000 delivery rows (with
`--rows`) and lets it call one endpoint after another from 128 loops over
64 tokens, while it measures the other org's inbox reads, sends and claims.
On Postgres (server on 2 cores, database on 4, public-mode limits) the
other org's median latencies stayed within 1.7 times their idle values for
every endpoint (two runs: at most 1.6 and 1.7 times, at most 20 ms); before these limits they reached 10 to 60 seconds. Run
once; on SQLite, where the database works on the server's cores, a flood
of `GET /pending` (200 seats) still raised them three to ten times.

`amail-bench --mode tenants --phases hostile` measures many orgs against
one: it gives each of `--hostile-orgs` orgs (12 by default) 50,000
messages of 20,000 bytes of text that does not compress (about the 1 GiB
quota each) and lets each run 4 loops of searches for 10 terms that match
nothing, following the cursor through all of its mail, while another org
calls every endpoint the web interface reads, 5 times per second each. On
Postgres 17 with the public-mode defaults (server and database on one host
with 20 cores), 30 seconds measured:

| | before this change | after |
|---|---|---|
| the other org's answers refused with 429 | all requests of `/overview`, `/attention`, `/lists`, `/lists/{name}`, `/threads/{id}`, `/messages/{id}/status`, `/archive/messages` and `/archive/search` (150 each) | none |
| its median latency, as a multiple of its idle median | 190 to 330 times for those endpoints (each refused after the one-second wait) | at most 1.4 times for every endpoint except search and archive; 3.7 and 4.1 times (19 and 17 ms) for those two |
| the searching orgs | 2 searches answered, 16 cancelled after 10 s, 1,231 refused | 2,271 searches answered, none refused (75 per second over the 8 slots: about 100 ms per request) |

Search and the archive start from the org's own messages, so their cost
does not grow with other orgs' mail. Measured on the same host with 500,000
messages of one org (bodies of about 600 bytes) and a second org with one
message: a search or archive request of the small org takes 1–3 ms on
both backends. A search of the large org that fills its page early takes
about 6 ms; one that finds nothing looks at its 50,000 newest messages in
about 290 ms (Postgres) or 80 ms (SQLite) and answers with a cursor to
search further back. A search reads the bodies it looks at, so its cost
grows with their size; each query is cancelled after 10 seconds.

Recommendations based on these numbers:

- Use Postgres for anything beyond a small team; one instance carried
  three times the design load (170 messages/s for 10,000 seats).
- The database's CPU is the first resource to run out; the node's claim retries in
  its log (hook timeouts) are the first sign of overload.
- Plan for about 12 connections per node at the proxy and the server.

## Backups

**SQLite.** Copy the database with SQLite's online backup while the server
runs (copying the file alone is not safe while it is written):

```sh
sqlite3 /var/lib/amail/amail.db ".backup '/backup/amail-$(date +%F).db'"
```

`VACUUM INTO '/backup/amail.db'` works too. To restore, stop the server,
put the copy in place of `amail.db` (remove `amail.db-wal` and
`amail.db-shm`), and start it.

**Postgres.** Use the usual tools, for example:

```sh
pg_dump -Fc -d amail > amail-$(date +%F).dump
pg_restore --no-owner -d amail_restored amail-2026-10-04.dump
```

and point `--db` at the restored database. With Docker Compose:
`docker compose exec postgres pg_dump -U amail -Fc amail > amail.dump`.

What a backup holds: everything in the [data notice](../README.md#your-data-on-an-amail-server),
including token hashes. Keep backups as private as the database.

## Upgrades and migrations

1. Back up the database.
2. Replace the binary (or image) and restart the server. Migrations run at
   start, inside transactions, and are forward-only; there is no downgrade.
3. Restart the nodes with the new binary too (`amail node stop`, then
   `amail node start`); a node keeps its state directory and restores the
   sessions that still run.

With several instances, stop all of them, upgrade, and start them again:
running instances of different versions on one database has not been
tested. Before 1.0, a minor version may change the HTTP API; the
[changelog](../CHANGELOG.md) says so.

## The node as a service

On each host that runs agents, the node runs as your user (it needs your
harness logins and your terminal sessions). Either:

```sh
amail node token --save     # once: a node token and the server URL in the state directory
amail node start            # background; log and pid in ~/.local/share/amail
amail node stop
```

or a systemd user unit, `~/.config/systemd/user/amail-node.service`:

```ini
[Unit]
Description=amail node (delivers mail to this host's agent sessions)

[Service]
ExecStart=%h/.local/bin/amail node
Restart=on-failure
RestartSec=2
# The node starts claude, codex, dsh and zellij: make sure they are on this
# PATH (or pass --claude-bin, --codex-bin, --dsh-bin, --zellij-bin). For dsh
# seats, pass the model provider's key with --dsh-env-file <file>.
Environment=PATH=%h/.local/bin:/usr/local/bin:/usr/bin:/bin

[Install]
WantedBy=default.target
```

```sh
systemctl --user daemon-reload
systemctl --user enable --now amail-node
loginctl enable-linger "$USER"   # keep it running when you are logged out
```

The node keeps its state in `~/.local/share/amail` (`AMAIL_STATE_DIR`), its
socket in `$XDG_RUNTIME_DIR/amail/node.sock` (`AMAIL_NODE_SOCK`).
