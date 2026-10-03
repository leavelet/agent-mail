-- M5: several server instances, idempotent claims, attention features.

-- Seat presence as reported by nodes, shared by all server instances. A row
-- older than the TTL (90 s) counts as absent.
CREATE TABLE presence (
    seat_id         BIGINT  PRIMARY KEY REFERENCES seats(id),
    org_id          BIGINT  NOT NULL REFERENCES orgs(id),
    state           TEXT    NOT NULL,
    wake_failed     BOOLEAN NOT NULL,
    harness         TEXT,
    node            TEXT,
    since           BIGINT  NOT NULL,
    updated_at      BIGINT  NOT NULL,
    attention       TEXT,
    attention_hint  TEXT,
    terminal_url    TEXT
);
CREATE INDEX presence_org ON presence(org_id);

-- Per-seat send budget (token bucket), used when several instances serve
-- one database.
CREATE TABLE send_budget (
    seat_id     BIGINT            PRIMARY KEY REFERENCES seats(id),
    tokens      DOUBLE PRECISION  NOT NULL,
    updated_at  BIGINT            NOT NULL
);

-- Claims made with an idempotency key: a retry with the same key returns the
-- same deliveries (comma-separated ids). Kept for a few minutes.
CREATE TABLE claim_keys (
    seat_id     BIGINT  NOT NULL REFERENCES seats(id),
    key         TEXT    NOT NULL,
    ids         TEXT    NOT NULL,
    created_at  BIGINT  NOT NULL,
    PRIMARY KEY (seat_id, key)
);
CREATE INDEX claim_keys_created ON claim_keys(created_at);

-- Running server instances (heartbeat) and where they can be reached by
-- other instances; the node event streams each holds, per org; the seats
-- whose conversation each instance's browsers watch.
CREATE TABLE server_instances (
    id            TEXT    PRIMARY KEY,
    url           TEXT,
    started_at    BIGINT  NOT NULL,
    heartbeat_at  BIGINT  NOT NULL
);
CREATE TABLE node_streams (
    instance_id  TEXT    NOT NULL,
    org_id       BIGINT  NOT NULL,
    streams      BIGINT  NOT NULL,
    PRIMARY KEY (instance_id, org_id)
);
CREATE TABLE mirror_watch (
    org_id       BIGINT  NOT NULL,
    seat         TEXT    NOT NULL,
    instance_id  TEXT    NOT NULL,
    expires_at   BIGINT  NOT NULL,
    PRIMARY KEY (org_id, seat, instance_id)
);

-- Attention: acknowledgement deadlines and escalation, focus mode, digests.
ALTER TABLE orgs ADD COLUMN ack_timeout_min BIGINT NOT NULL DEFAULT 15;
ALTER TABLE orgs ADD COLUMN focus_max_min BIGINT NOT NULL DEFAULT 240;
ALTER TABLE orgs ADD COLUMN digest_interval_min BIGINT NOT NULL DEFAULT 30;
ALTER TABLE seats ADD COLUMN focus_until BIGINT;
-- When the next escalation step of an unacknowledged message is due (NULL:
-- nothing more to do), and how many steps were taken.
ALTER TABLE messages ADD COLUMN ack_due BIGINT;
ALTER TABLE messages ADD COLUMN ack_stage BIGINT NOT NULL DEFAULT 0;
CREATE INDEX messages_ack_due ON messages(ack_due) WHERE ack_due IS NOT NULL;
-- Every org has one hidden seat of kind `system` that sends system mail. It
-- is created deleted, so it is never in the directory and never addressable,
-- and its name never conflicts with a real seat.
CREATE UNIQUE INDEX seats_org_system ON seats(org_id) WHERE kind = 'system';
