-- Mail of deleted orgs that an org keeps is no longer counted in its
-- `stored_bytes` (the bytes its sends are refused over), but in
-- `kept_bytes`: for each message of a deleted org that the org keeps
-- deliveries of, the message's full bytes and the org's own deliveries of
-- it (the same amounts migration 0013 charged to `stored_bytes`).
-- Retention deletes such mail once it is older than the retention period,
-- and the org's user can delete it (`DELETE /v1/threads/{id}`,
-- `POST /v1/archive/purge-deleted-orgs`).
ALTER TABLE orgs ADD COLUMN kept_bytes BIGINT NOT NULL DEFAULT 0;
UPDATE orgs SET kept_bytes = COALESCE((
    SELECT SUM(512 + octet_length(m.subject) + octet_length(m.body) + octet_length(m.meta)
               + octet_length(m.to_addrs) + octet_length(m.cc_addrs)
               + 384 * (SELECT COUNT(*) FROM deliveries d WHERE d.message_id = m.id AND d.org_id = orgs.id))
      FROM messages m
     WHERE m.org_id IN (SELECT o.id FROM orgs o WHERE o.deleted_at IS NOT NULL) AND NOT m.notice
       AND EXISTS (SELECT 1 FROM deliveries d WHERE d.message_id = m.id AND d.org_id = orgs.id)
), 0) WHERE deleted_at IS NULL;
UPDATE orgs SET stored_bytes = stored_bytes - kept_bytes WHERE deleted_at IS NULL;
