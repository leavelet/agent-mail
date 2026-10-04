-- Storage per org: the bytes of the subject, body and meta (as stored) of
-- every message the org sent, each message counted once however many
-- recipients it has. Sending adds to it, clearing a body (retention)
-- subtracts. Computed here for the messages already stored.
ALTER TABLE orgs ADD COLUMN stored_bytes BIGINT NOT NULL DEFAULT 0;
UPDATE orgs SET stored_bytes = (
    SELECT COALESCE(SUM(octet_length(m.subject) + octet_length(m.body) + octet_length(m.meta)), 0)
      FROM messages m WHERE m.org_id = orgs.id
);
