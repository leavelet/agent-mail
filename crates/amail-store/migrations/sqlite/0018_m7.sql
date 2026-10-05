-- M7: cards (descriptions), teams, openings (grants), thread participants.
-- Keep in sync with migrations/postgres.

-- The org's card and what it opens.
ALTER TABLE orgs ADD COLUMN display_name TEXT;
ALTER TABLE orgs ADD COLUMN description TEXT;
-- `listed-by-name` or `hidden`.
ALTER TABLE orgs ADD COLUMN visibility TEXT NOT NULL DEFAULT 'listed-by-name';
-- `any-seat` or `open-seats-only`.
ALTER TABLE orgs ADD COLUMN outbound TEXT NOT NULL DEFAULT 'any-seat';
-- New threads to other orgs a seat may start per hour.
ALTER TABLE orgs ADD COLUMN new_threads_per_hour BIGINT NOT NULL DEFAULT 10;
-- A one-time notice for the user (shown by `amail org show` and `amail up`).
ALTER TABLE orgs ADD COLUMN notice TEXT;

-- Teams: several trees per org. A seat belongs to at most one team.
CREATE TABLE teams (
    id            INTEGER PRIMARY KEY,
    org_id        BIGINT  NOT NULL REFERENCES orgs(id),
    name          TEXT    NOT NULL,
    description   TEXT,
    root_seat_id  BIGINT  REFERENCES seats(id),
    parent_id     BIGINT  REFERENCES teams(id),
    managed       BOOLEAN NOT NULL DEFAULT FALSE,
    created_at    BIGINT  NOT NULL
);
CREATE UNIQUE INDEX teams_org_name ON teams(org_id, name);

ALTER TABLE seats ADD COLUMN description TEXT;
ALTER TABLE seats ADD COLUMN team_id BIGINT REFERENCES teams(id);
-- A contact of its team: opening the team opens this seat.
ALTER TABLE seats ADD COLUMN contact BOOLEAN NOT NULL DEFAULT FALSE;
CREATE INDEX seats_team ON seats(team_id);

-- Existing groups become teams (created by `amail up`).
INSERT INTO teams (org_id, name, managed, created_at)
SELECT DISTINCT org_id, group_name, TRUE, CAST(strftime('%s', 'now') AS INTEGER) * 1000
  FROM seats WHERE group_name IS NOT NULL AND deleted_at IS NULL;
UPDATE seats SET team_id = (SELECT t.id FROM teams t WHERE t.org_id = seats.org_id AND t.name = seats.group_name)
 WHERE group_name IS NOT NULL AND deleted_at IS NULL;

-- Openings: the org shows a seat or a team to `audience` (NULL: every org
-- with an active link; else one org, by name, effective while linked).
CREATE TABLE grants (
    id          INTEGER PRIMARY KEY,
    org_id      BIGINT  NOT NULL REFERENCES orgs(id),
    seat_id     BIGINT  REFERENCES seats(id),
    team_id     BIGINT  REFERENCES teams(id),
    audience    TEXT,
    managed     BOOLEAN NOT NULL DEFAULT FALSE,
    created_at  BIGINT  NOT NULL
);
CREATE UNIQUE INDEX grants_target ON grants(org_id, COALESCE(seat_id, 0), COALESCE(team_id, 0), COALESCE(audience, ''));
CREATE INDEX grants_audience ON grants(audience);

-- Every user seat is open to linked orgs (the default of a new org).
INSERT INTO grants (org_id, seat_id, audience, created_at)
SELECT s.org_id, s.id, NULL, CAST(strftime('%s', 'now') AS INTEGER) * 1000
  FROM seats s JOIN orgs o ON o.id = s.org_id
 WHERE s.kind = 'user' AND s.deleted_at IS NULL AND o.deleted_at IS NULL;

-- Participants of threads that cross orgs: every seat (sender or
-- recipient) of a message of the thread that reached more than one org,
-- with the seat's org. Added when such a message is sent.
CREATE TABLE thread_parts (
    thread_id BIGINT NOT NULL,
    seat_id   BIGINT NOT NULL,
    org_id    BIGINT NOT NULL,
    PRIMARY KEY (thread_id, seat_id)
) WITHOUT ROWID;
INSERT OR IGNORE INTO thread_parts (thread_id, seat_id, org_id)
SELECT DISTINCT m.thread_id, m.sender_seat_id, m.org_id FROM messages m
 WHERE EXISTS (SELECT 1 FROM deliveries d WHERE d.message_id = m.id AND d.org_id <> m.org_id);
INSERT OR IGNORE INTO thread_parts (thread_id, seat_id, org_id)
SELECT DISTINCT m.thread_id, d.seat_id, d.org_id FROM deliveries d JOIN messages m ON m.id = d.message_id
 WHERE EXISTS (SELECT 1 FROM deliveries d2 WHERE d2.message_id = m.id AND d2.org_id <> m.org_id);

-- Primary contacts: the seat of an org that the server adds in Cc when a
-- seat that is not a contact writes to a linked org. Per linked org
-- (`links.primary_seat_id`) and per team (`team_primaries`).
ALTER TABLE links ADD COLUMN primary_seat_id BIGINT REFERENCES seats(id);
CREATE TABLE team_primaries (
    team_id      BIGINT NOT NULL REFERENCES teams(id),
    peer_org_id  BIGINT NOT NULL REFERENCES orgs(id),
    seat_id      BIGINT NOT NULL REFERENCES seats(id),
    PRIMARY KEY (team_id, peer_org_id)
);

-- Lists hold seats of their own org only.
DELETE FROM list_members WHERE seat_id IS NULL;

-- Links: when they became active, and the requester's note.
ALTER TABLE links ADD COLUMN active_at BIGINT;
ALTER TABLE links ADD COLUMN note TEXT;
UPDATE links SET active_at = created_at WHERE state = 'active';
ALTER TABLE link_requests ADD COLUMN note TEXT;

-- Orgs with links learn once that links no longer open everything.
UPDATE orgs SET notice = 'Links no longer open your whole org: another org now sees your org''s card and only the seats and teams you open to it (amail open), and mail between orgs always carries a contact of each org. After this upgrade only your user seat is open to linked orgs, and lists hold seats of your own org only. See docs/concepts.md (Other orgs).'
 WHERE deleted_at IS NULL AND id IN (SELECT org_id FROM links);
