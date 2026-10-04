-- Storage per org, counted again under a new rule. A message counts its
-- subject, body, meta and address lists as stored, plus 512 bytes for its
-- row and index entries, plus 384 bytes for each of its deliveries
-- (`STORED_BYTES_PER_MESSAGE` and `STORED_BYTES_PER_DELIVERY`). The sending
-- org is charged for it. Mail of a deleted org is charged to each org that
-- keeps deliveries of it: the message's full bytes and that org's own
-- deliveries. Deleted orgs are charged nothing.
UPDATE orgs SET stored_bytes = 0;
UPDATE orgs SET stored_bytes = COALESCE((
    SELECT SUM(512 + octet_length(m.subject) + octet_length(m.body) + octet_length(m.meta)
               + octet_length(m.to_addrs) + octet_length(m.cc_addrs)
               + 384 * (SELECT COUNT(*) FROM deliveries d WHERE d.message_id = m.id))
      FROM messages m WHERE m.org_id = orgs.id
), 0) WHERE deleted_at IS NULL;
UPDATE orgs SET stored_bytes = stored_bytes + COALESCE((
    SELECT SUM(512 + octet_length(m.subject) + octet_length(m.body) + octet_length(m.meta)
               + octet_length(m.to_addrs) + octet_length(m.cc_addrs)
               + 384 * (SELECT COUNT(*) FROM deliveries d WHERE d.message_id = m.id AND d.org_id = orgs.id))
      FROM messages m
     WHERE m.org_id IN (SELECT o.id FROM orgs o WHERE o.deleted_at IS NOT NULL)
       AND EXISTS (SELECT 1 FROM deliveries d WHERE d.message_id = m.id AND d.org_id = orgs.id)
), 0) WHERE deleted_at IS NULL;
