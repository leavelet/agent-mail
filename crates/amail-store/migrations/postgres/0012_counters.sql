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
        SUM(CASE WHEN d.notified_at IS NULL AND d.mode = 'immediate' AND d.class = 'to' THEN 1 ELSE 0 END) AS pt,
        SUM(CASE WHEN d.notified_at IS NULL AND d.mode = 'immediate' AND d.class = 'cc' THEN 1 ELSE 0 END) AS pc,
        SUM(CASE WHEN d.notified_at IS NULL AND d.mode = 'immediate' AND d.class = 'list' THEN 1 ELSE 0 END) AS pl,
        SUM(CASE WHEN d.notified_at IS NULL AND d.mode = 'immediate' AND m.priority = 'urgent' THEN 1 ELSE 0 END) AS pu,
        SUM(CASE WHEN d.notified_at IS NULL AND d.mode = 'digest' THEN 1 ELSE 0 END) AS pd,
        SUM(CASE WHEN d.read_at IS NULL AND d.mode <> 'archive' THEN 1 ELSE 0 END) AS un
    FROM deliveries d JOIN messages m ON m.id = d.message_id
    GROUP BY d.seat_id
) c
WHERE seats.id = c.seat_id;

-- The oldest pending digest delivery of a seat.
CREATE INDEX deliveries_digest ON deliveries(seat_id, seq) WHERE notified_at IS NULL AND mode = 'digest';
CREATE FUNCTION oldest_pending_digest(seat BIGINT) RETURNS BIGINT LANGUAGE sql STABLE AS $$
    SELECT m.created_at FROM deliveries d JOIN messages m ON m.id = d.message_id
     WHERE d.seat_id = seat AND d.notified_at IS NULL AND d.mode = 'digest' ORDER BY d.seq LIMIT 1
$$;
UPDATE seats SET pending_digest_since = oldest_pending_digest(id) WHERE pending_digest > 0;

-- The orgs and seats that received mail in a thread, so that whether a
-- caller may see a thread is one index lookup. (Senders are found with
-- `messages_thread_sender`.) Rows are added with deliveries and removed
-- with the org.
CREATE TABLE thread_seats (
    thread_id BIGINT NOT NULL,
    org_id    BIGINT NOT NULL,
    seat_id   BIGINT NOT NULL,
    PRIMARY KEY (thread_id, org_id, seat_id)
);
INSERT INTO thread_seats (thread_id, org_id, seat_id)
SELECT DISTINCT m.thread_id, d.org_id, d.seat_id
  FROM deliveries d JOIN messages m ON m.id = d.message_id;

CREATE INDEX messages_thread_sender ON messages(thread_id, org_id, sender_seat_id, id);
-- One statement's deliveries change each seat's counters once.
CREATE FUNCTION deliveries_inserted() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    UPDATE seats s SET
        pending_to = s.pending_to + x.pt, pending_cc = s.pending_cc + x.pc,
        pending_list = s.pending_list + x.pl, pending_urgent = s.pending_urgent + x.pu,
        pending_digest = s.pending_digest + x.pd, unread = s.unread + x.un,
        pending_digest_since = COALESCE(s.pending_digest_since, x.ds)
    FROM (
        SELECT n.seat_id,
            MIN(CASE WHEN n.notified_at IS NULL AND n.mode = 'digest' THEN m.created_at END) AS ds,
            SUM(CASE WHEN n.notified_at IS NULL AND n.mode = 'immediate' AND n.class = 'to' THEN 1 ELSE 0 END) AS pt,
            SUM(CASE WHEN n.notified_at IS NULL AND n.mode = 'immediate' AND n.class = 'cc' THEN 1 ELSE 0 END) AS pc,
            SUM(CASE WHEN n.notified_at IS NULL AND n.mode = 'immediate' AND n.class = 'list' THEN 1 ELSE 0 END) AS pl,
            SUM(CASE WHEN n.notified_at IS NULL AND n.mode = 'immediate' AND m.priority = 'urgent' THEN 1 ELSE 0 END) AS pu,
            SUM(CASE WHEN n.notified_at IS NULL AND n.mode = 'digest' THEN 1 ELSE 0 END) AS pd,
            SUM(CASE WHEN n.read_at IS NULL AND n.mode <> 'archive' THEN 1 ELSE 0 END) AS un
        FROM new_rows n JOIN messages m ON m.id = n.message_id
        GROUP BY n.seat_id
    ) x
    WHERE s.id = x.seat_id;
    INSERT INTO thread_seats (thread_id, org_id, seat_id)
    SELECT DISTINCT m.thread_id, n.org_id, n.seat_id
      FROM new_rows n JOIN messages m ON m.id = n.message_id
     ORDER BY 1, 2, 3
    ON CONFLICT DO NOTHING;
    RETURN NULL;
END $$;

CREATE FUNCTION deliveries_updated() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    UPDATE seats s SET
        pending_to = s.pending_to + x.pt, pending_cc = s.pending_cc + x.pc,
        pending_list = s.pending_list + x.pl, pending_urgent = s.pending_urgent + x.pu,
        pending_digest = s.pending_digest + x.pd, unread = s.unread + x.un,
        pending_digest_since = CASE WHEN x.pd = 0 THEN s.pending_digest_since
            ELSE oldest_pending_digest(s.id) END
    FROM (
        SELECT n.seat_id,
            SUM((CASE WHEN n.notified_at IS NULL AND n.mode = 'immediate' AND n.class = 'to' THEN 1 ELSE 0 END)
              - (CASE WHEN o.notified_at IS NULL AND o.mode = 'immediate' AND o.class = 'to' THEN 1 ELSE 0 END)) AS pt,
            SUM((CASE WHEN n.notified_at IS NULL AND n.mode = 'immediate' AND n.class = 'cc' THEN 1 ELSE 0 END)
              - (CASE WHEN o.notified_at IS NULL AND o.mode = 'immediate' AND o.class = 'cc' THEN 1 ELSE 0 END)) AS pc,
            SUM((CASE WHEN n.notified_at IS NULL AND n.mode = 'immediate' AND n.class = 'list' THEN 1 ELSE 0 END)
              - (CASE WHEN o.notified_at IS NULL AND o.mode = 'immediate' AND o.class = 'list' THEN 1 ELSE 0 END)) AS pl,
            SUM(CASE WHEN m.priority = 'urgent' THEN
                (CASE WHEN n.notified_at IS NULL AND n.mode = 'immediate' THEN 1 ELSE 0 END)
              - (CASE WHEN o.notified_at IS NULL AND o.mode = 'immediate' THEN 1 ELSE 0 END) ELSE 0 END) AS pu,
            SUM((CASE WHEN n.notified_at IS NULL AND n.mode = 'digest' THEN 1 ELSE 0 END)
              - (CASE WHEN o.notified_at IS NULL AND o.mode = 'digest' THEN 1 ELSE 0 END)) AS pd,
            SUM((CASE WHEN n.read_at IS NULL AND n.mode <> 'archive' THEN 1 ELSE 0 END)
              - (CASE WHEN o.read_at IS NULL AND o.mode <> 'archive' THEN 1 ELSE 0 END)) AS un
        FROM old_rows o JOIN new_rows n ON n.id = o.id JOIN messages m ON m.id = n.message_id
        WHERE (o.notified_at IS NULL) <> (n.notified_at IS NULL) OR (o.read_at IS NULL) <> (n.read_at IS NULL)
           OR o.mode <> n.mode OR o.class <> n.class
        GROUP BY n.seat_id
    ) x
    WHERE s.id = x.seat_id AND (x.pt <> 0 OR x.pc <> 0 OR x.pl <> 0 OR x.pu <> 0 OR x.pd <> 0 OR x.un <> 0);
    RETURN NULL;
END $$;

CREATE FUNCTION deliveries_deleted() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    UPDATE seats s SET
        pending_to = s.pending_to - x.pt, pending_cc = s.pending_cc - x.pc,
        pending_list = s.pending_list - x.pl, pending_urgent = s.pending_urgent - x.pu,
        pending_digest = s.pending_digest - x.pd, unread = s.unread - x.un,
        pending_digest_since = CASE WHEN x.pd = 0 THEN s.pending_digest_since
            ELSE oldest_pending_digest(s.id) END
    FROM (
        SELECT o.seat_id,
            SUM(CASE WHEN o.notified_at IS NULL AND o.mode = 'immediate' AND o.class = 'to' THEN 1 ELSE 0 END) AS pt,
            SUM(CASE WHEN o.notified_at IS NULL AND o.mode = 'immediate' AND o.class = 'cc' THEN 1 ELSE 0 END) AS pc,
            SUM(CASE WHEN o.notified_at IS NULL AND o.mode = 'immediate' AND o.class = 'list' THEN 1 ELSE 0 END) AS pl,
            SUM(CASE WHEN o.notified_at IS NULL AND o.mode = 'immediate' AND m.priority = 'urgent' THEN 1 ELSE 0 END) AS pu,
            SUM(CASE WHEN o.notified_at IS NULL AND o.mode = 'digest' THEN 1 ELSE 0 END) AS pd,
            SUM(CASE WHEN o.read_at IS NULL AND o.mode <> 'archive' THEN 1 ELSE 0 END) AS un
        FROM old_rows o JOIN messages m ON m.id = o.message_id
        GROUP BY o.seat_id
    ) x
    WHERE s.id = x.seat_id;
    RETURN NULL;
END $$;

CREATE TRIGGER deliveries_inserted AFTER INSERT ON deliveries
    REFERENCING NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION deliveries_inserted();
CREATE TRIGGER deliveries_updated AFTER UPDATE ON deliveries
    REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION deliveries_updated();
CREATE TRIGGER deliveries_deleted AFTER DELETE ON deliveries
    REFERENCING OLD TABLE AS old_rows FOR EACH STATEMENT EXECUTE FUNCTION deliveries_deleted();

-- Messages each org sent or received, per minute, for the counts of
-- recent mail in `GET /v1/overview`. Server instances add their counts
-- about once a second; rows older than two days are deleted by the
-- background cleanup.
CREATE TABLE org_activity (
    org_id   BIGINT NOT NULL,
    minute   BIGINT NOT NULL,
    messages BIGINT NOT NULL,
    PRIMARY KEY (org_id, minute)
);
CREATE INDEX org_activity_minute ON org_activity(minute);
INSERT INTO org_activity (org_id, minute, messages)
SELECT v.org_id, v.created_at / 60000, COUNT(*) FROM (
    SELECT m.org_id, m.id, m.created_at FROM messages m
     WHERE m.created_at >= (extract(epoch FROM now()) * 1000)::bigint - 86400000
    UNION
    SELECT d.org_id, m.id, m.created_at FROM deliveries d JOIN messages m ON m.id = d.message_id
     WHERE m.created_at >= (extract(epoch FROM now()) * 1000)::bigint - 86400000
) v GROUP BY v.org_id, v.created_at / 60000;
