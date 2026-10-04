-- Per-seat counters, so that pending and unread counts are read from the
-- seat's row instead of counted over its deliveries. Triggers on
-- `deliveries` keep them current in the transaction that inserts, changes
-- or deletes deliveries (sending, claiming, reading, acknowledging,
-- deleting an org).
--   pending_to/_cc/_list: not notified, mode `immediate`, by class;
--   pending_urgent: not notified, mode `immediate`, urgent message;
--   pending_digest: not notified, mode `digest`;
--   pending_digest_since: creation time of the oldest of those;
--   unread: not read, mode other than `archive`.
ALTER TABLE seats ADD COLUMN pending_to BIGINT NOT NULL DEFAULT 0;
ALTER TABLE seats ADD COLUMN pending_cc BIGINT NOT NULL DEFAULT 0;
ALTER TABLE seats ADD COLUMN pending_list BIGINT NOT NULL DEFAULT 0;
ALTER TABLE seats ADD COLUMN pending_urgent BIGINT NOT NULL DEFAULT 0;
ALTER TABLE seats ADD COLUMN pending_digest BIGINT NOT NULL DEFAULT 0;
ALTER TABLE seats ADD COLUMN unread BIGINT NOT NULL DEFAULT 0;
ALTER TABLE seats ADD COLUMN pending_digest_since BIGINT;

UPDATE seats SET
    pending_to = c.pt, pending_cc = c.pc, pending_list = c.pl, pending_urgent = c.pu,
    pending_digest = c.pd, unread = c.un
FROM (
    SELECT d.seat_id,
        SUM(d.notified_at IS NULL AND d.mode = 'immediate' AND d.class = 'to') AS pt,
        SUM(d.notified_at IS NULL AND d.mode = 'immediate' AND d.class = 'cc') AS pc,
        SUM(d.notified_at IS NULL AND d.mode = 'immediate' AND d.class = 'list') AS pl,
        SUM(d.notified_at IS NULL AND d.mode = 'immediate' AND m.priority = 'urgent') AS pu,
        SUM(d.notified_at IS NULL AND d.mode = 'digest') AS pd,
        SUM(d.read_at IS NULL AND d.mode <> 'archive') AS un
    FROM deliveries d JOIN messages m ON m.id = d.message_id
    GROUP BY d.seat_id
) c
WHERE seats.id = c.seat_id;

-- The oldest pending digest delivery of a seat.
CREATE INDEX deliveries_digest ON deliveries(seat_id, seq) WHERE notified_at IS NULL AND mode = 'digest';
UPDATE seats SET pending_digest_since = (SELECT m.created_at FROM deliveries d JOIN messages m ON m.id = d.message_id
            WHERE d.seat_id = seats.id AND d.notified_at IS NULL AND d.mode = 'digest' ORDER BY d.seq LIMIT 1) WHERE pending_digest > 0;

-- The orgs and seats that received mail in a thread, so that whether a
-- caller may see a thread is one index lookup. (Senders are found with
-- `messages_thread_sender`.) Rows are added with deliveries and removed
-- with the org.
CREATE TABLE thread_seats (
    thread_id BIGINT NOT NULL,
    org_id    BIGINT NOT NULL,
    seat_id   BIGINT NOT NULL,
    PRIMARY KEY (thread_id, org_id, seat_id)
) WITHOUT ROWID;
INSERT OR IGNORE INTO thread_seats (thread_id, org_id, seat_id)
SELECT m.thread_id, d.org_id, d.seat_id
  FROM deliveries d JOIN messages m ON m.id = d.message_id;

-- (SQLite indexes end with the rowid, the message id.)
CREATE INDEX messages_thread_sender ON messages(thread_id, org_id, sender_seat_id);
CREATE TRIGGER deliveries_inserted AFTER INSERT ON deliveries BEGIN
    UPDATE seats SET
        pending_to = pending_to + (NEW.notified_at IS NULL AND NEW.mode = 'immediate' AND NEW.class = 'to'),
        pending_cc = pending_cc + (NEW.notified_at IS NULL AND NEW.mode = 'immediate' AND NEW.class = 'cc'),
        pending_list = pending_list + (NEW.notified_at IS NULL AND NEW.mode = 'immediate' AND NEW.class = 'list'),
        pending_urgent = pending_urgent + (NEW.notified_at IS NULL AND NEW.mode = 'immediate'
            AND (SELECT priority FROM messages WHERE id = NEW.message_id) = 'urgent'),
        pending_digest = pending_digest + (NEW.notified_at IS NULL AND NEW.mode = 'digest'),
        pending_digest_since = CASE WHEN NEW.notified_at IS NULL AND NEW.mode = 'digest'
            THEN COALESCE(pending_digest_since, (SELECT created_at FROM messages WHERE id = NEW.message_id))
            ELSE pending_digest_since END,
        unread = unread + (NEW.read_at IS NULL AND NEW.mode <> 'archive')
    WHERE id = NEW.seat_id;
    INSERT OR IGNORE INTO thread_seats (thread_id, org_id, seat_id)
    VALUES ((SELECT thread_id FROM messages WHERE id = NEW.message_id), NEW.org_id, NEW.seat_id);
END;

CREATE TRIGGER deliveries_updated AFTER UPDATE OF notified_at, read_at, mode, class ON deliveries
WHEN (OLD.notified_at IS NULL) <> (NEW.notified_at IS NULL) OR (OLD.read_at IS NULL) <> (NEW.read_at IS NULL)
     OR OLD.mode <> NEW.mode OR OLD.class <> NEW.class
BEGIN
    UPDATE seats SET
        pending_to = pending_to + (NEW.notified_at IS NULL AND NEW.mode = 'immediate' AND NEW.class = 'to')
            - (OLD.notified_at IS NULL AND OLD.mode = 'immediate' AND OLD.class = 'to'),
        pending_cc = pending_cc + (NEW.notified_at IS NULL AND NEW.mode = 'immediate' AND NEW.class = 'cc')
            - (OLD.notified_at IS NULL AND OLD.mode = 'immediate' AND OLD.class = 'cc'),
        pending_list = pending_list + (NEW.notified_at IS NULL AND NEW.mode = 'immediate' AND NEW.class = 'list')
            - (OLD.notified_at IS NULL AND OLD.mode = 'immediate' AND OLD.class = 'list'),
        pending_urgent = pending_urgent + ((SELECT priority FROM messages WHERE id = NEW.message_id) = 'urgent')
            * ((NEW.notified_at IS NULL AND NEW.mode = 'immediate') - (OLD.notified_at IS NULL AND OLD.mode = 'immediate')),
        pending_digest = pending_digest + (NEW.notified_at IS NULL AND NEW.mode = 'digest')
            - (OLD.notified_at IS NULL AND OLD.mode = 'digest'),
        pending_digest_since = CASE WHEN (NEW.notified_at IS NULL AND NEW.mode = 'digest')
            = (OLD.notified_at IS NULL AND OLD.mode = 'digest') THEN pending_digest_since
            ELSE (SELECT m.created_at FROM deliveries d JOIN messages m ON m.id = d.message_id
            WHERE d.seat_id = NEW.seat_id AND d.notified_at IS NULL AND d.mode = 'digest' ORDER BY d.seq LIMIT 1) END,
        unread = unread + (NEW.read_at IS NULL AND NEW.mode <> 'archive') - (OLD.read_at IS NULL AND OLD.mode <> 'archive')
    WHERE id = NEW.seat_id;
END;

CREATE TRIGGER deliveries_deleted AFTER DELETE ON deliveries BEGIN
    UPDATE seats SET
        pending_to = pending_to - (OLD.notified_at IS NULL AND OLD.mode = 'immediate' AND OLD.class = 'to'),
        pending_cc = pending_cc - (OLD.notified_at IS NULL AND OLD.mode = 'immediate' AND OLD.class = 'cc'),
        pending_list = pending_list - (OLD.notified_at IS NULL AND OLD.mode = 'immediate' AND OLD.class = 'list'),
        pending_urgent = pending_urgent - (OLD.notified_at IS NULL AND OLD.mode = 'immediate'
            AND (SELECT priority FROM messages WHERE id = OLD.message_id) = 'urgent'),
        pending_digest = pending_digest - (OLD.notified_at IS NULL AND OLD.mode = 'digest'),
        pending_digest_since = CASE WHEN OLD.notified_at IS NULL AND OLD.mode = 'digest'
            THEN (SELECT m.created_at FROM deliveries d JOIN messages m ON m.id = d.message_id
            WHERE d.seat_id = OLD.seat_id AND d.notified_at IS NULL AND d.mode = 'digest' ORDER BY d.seq LIMIT 1) ELSE pending_digest_since END,
        unread = unread - (OLD.read_at IS NULL AND OLD.mode <> 'archive')
    WHERE id = OLD.seat_id;
END;

-- Messages each org sent or received, per minute, for the counts of
-- recent mail in `GET /v1/overview`. Server instances add their counts
-- about once a second; rows older than two days are deleted by the
-- background cleanup.
CREATE TABLE org_activity (
    org_id   BIGINT NOT NULL,
    minute   BIGINT NOT NULL,
    messages BIGINT NOT NULL,
    PRIMARY KEY (org_id, minute)
) WITHOUT ROWID;
CREATE INDEX org_activity_minute ON org_activity(minute);
INSERT INTO org_activity (org_id, minute, messages)
SELECT v.org_id, v.created_at / 60000, COUNT(*) FROM (
    SELECT m.org_id, m.id, m.created_at FROM messages m
     WHERE m.created_at >= CAST(strftime('%s', 'now') AS INTEGER) * 1000 - 86400000
    UNION
    SELECT d.org_id, m.id, m.created_at FROM deliveries d JOIN messages m ON m.id = d.message_id
     WHERE m.created_at >= CAST(strftime('%s', 'now') AS INTEGER) * 1000 - 86400000
) v GROUP BY v.org_id, v.created_at / 60000;
