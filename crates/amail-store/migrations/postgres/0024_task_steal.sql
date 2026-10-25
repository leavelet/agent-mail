-- M8 work stealing: a pending request of another seat to take over an
-- assigned task that is not started (one per task). Keep in sync with
-- migrations/sqlite.
ALTER TABLE tasks ADD COLUMN steal_requested_by BIGINT REFERENCES seats(id);
ALTER TABLE tasks ADD COLUMN steal_requested_at BIGINT;
CREATE INDEX tasks_steal ON tasks(steal_requested_at) WHERE steal_requested_at IS NOT NULL;
