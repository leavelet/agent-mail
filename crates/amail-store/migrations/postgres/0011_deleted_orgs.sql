-- A deleted org whose mail other orgs received stays as a row marked
-- deleted, so that the copies those orgs keep still name their sender.
-- Its `name` becomes `deleted:<id>`, which no org can take, so the name is
-- free for a new org; `deleted_name` keeps the old name for display.
ALTER TABLE orgs ADD COLUMN deleted_at BIGINT;
ALTER TABLE orgs ADD COLUMN deleted_name TEXT;
