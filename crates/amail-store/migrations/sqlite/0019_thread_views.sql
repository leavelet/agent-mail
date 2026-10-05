-- What each org knows of a thread's participants: the seats (sender and
-- recipients) of the thread's messages that crossed orgs and that a seat
-- of that org sent or received. Replaces `thread_parts`, which recorded
-- participants per thread for every org alike. Keep in sync with
-- migrations/postgres.
CREATE TABLE thread_views (
    thread_id      BIGINT NOT NULL,
    viewer_org_id  BIGINT NOT NULL,
    seat_id        BIGINT NOT NULL,
    org_id         BIGINT NOT NULL,
    PRIMARY KEY (thread_id, viewer_org_id, seat_id)
) WITHOUT ROWID;

INSERT OR IGNORE INTO thread_views (thread_id, viewer_org_id, seat_id, org_id)
WITH crossing AS (
    SELECT m.id, m.thread_id, m.org_id, m.sender_seat_id FROM messages m
     WHERE EXISTS (SELECT 1 FROM deliveries d WHERE d.message_id = m.id AND d.org_id <> m.org_id)
), viewers AS (
    SELECT c.id, c.org_id AS v FROM crossing c
    UNION SELECT d.message_id, d.org_id FROM deliveries d JOIN crossing c ON c.id = d.message_id
), seats_on AS (
    SELECT c.id, c.sender_seat_id AS s, c.org_id AS o FROM crossing c
    UNION SELECT d.message_id, d.seat_id, d.org_id FROM deliveries d JOIN crossing c ON c.id = d.message_id
)
SELECT c.thread_id, v.v, so.s, so.o FROM crossing c
  JOIN viewers v ON v.id = c.id JOIN seats_on so ON so.id = c.id;

DROP TABLE thread_parts;
