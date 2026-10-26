-- M8.1: idempotent creation, results, imported tasks, dependencies that
-- need an ok result, and the names of imported events' actors. Existing
-- rows are unchanged. Keep in sync with migrations/postgres.

-- The key of an idempotent create (`POST /tasks` with `client_key`).
ALTER TABLE tasks ADD COLUMN client_key TEXT;
-- The result of a closed task: `ok` or `rejected` (NULL: none), the
-- version it is about, a note, the seat that set it (a name: imported
-- results may name seats that do not exist) and when.
ALTER TABLE tasks ADD COLUMN result TEXT;
ALTER TABLE tasks ADD COLUMN result_ref TEXT;
ALTER TABLE tasks ADD COLUMN result_note TEXT;
ALTER TABLE tasks ADD COLUMN result_by TEXT;
ALTER TABLE tasks ADD COLUMN result_at BIGINT;
-- The id of an imported task in the file it came from.
ALTER TABLE tasks ADD COLUMN import_key TEXT;
CREATE INDEX tasks_client_key ON tasks(org_id, client_key) WHERE client_key IS NOT NULL;
CREATE UNIQUE INDEX tasks_import_key ON tasks(org_id, import_key) WHERE import_key IS NOT NULL;

-- A dependency that only an ok result of the blocker satisfies.
ALTER TABLE task_deps ADD COLUMN needs_ok BOOLEAN NOT NULL DEFAULT FALSE;

-- Events of kind `imported`: the actor's name as the file gives it (the
-- seat need not exist).
ALTER TABLE task_events ADD COLUMN actor_name TEXT;
