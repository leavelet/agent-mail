-- M8: tasks (one shared record per task per org), their dependencies,
-- linked threads and history. Keep in sync with migrations/postgres.

-- The last task number the org gave out (numbers are never reused).
ALTER TABLE orgs ADD COLUMN org_task_seq BIGINT NOT NULL DEFAULT 0;
-- Hours after which a task in `doing` or `review` that nobody changed is
-- reminded to its assignee; 0 turns this off.
ALTER TABLE orgs ADD COLUMN task_stale_hours BIGINT NOT NULL DEFAULT 24;
-- When the stale pass last ran for the org (at most once an hour).
ALTER TABLE orgs ADD COLUMN task_stale_pass_at BIGINT;
-- Bytes of the org's tasks counted against the storage quota (with
-- `stored_bytes`).
ALTER TABLE orgs ADD COLUMN task_bytes BIGINT NOT NULL DEFAULT 0;

CREATE TABLE tasks (
    id                 INTEGER PRIMARY KEY,
    org_id             BIGINT  NOT NULL REFERENCES orgs(id),
    num                BIGINT  NOT NULL,
    title              TEXT    NOT NULL,
    body               TEXT    NOT NULL DEFAULT '',
    -- open, doing, blocked, review, done, cancelled (`ready` is derived).
    state              TEXT    NOT NULL DEFAULT 'open',
    -- urgent, high, normal, low.
    priority           TEXT    NOT NULL DEFAULT 'normal',
    rank               BIGINT  NOT NULL DEFAULT 0,
    reporter_id        BIGINT  NOT NULL REFERENCES seats(id),
    assignee_id        BIGINT  REFERENCES seats(id),
    team_id            BIGINT  REFERENCES teams(id),
    parent_id          BIGINT  REFERENCES tasks(id),
    -- `,a,b,` (empty: none).
    labels             TEXT    NOT NULL DEFAULT '',
    due                BIGINT,
    waiting_on_id      BIGINT  REFERENCES seats(id),
    blocked_reason     TEXT,
    -- The discussion thread.
    thread_id          BIGINT,
    created_at         BIGINT  NOT NULL,
    updated_at         BIGINT  NOT NULL,
    closed_at          BIGINT,
    -- The last event's seq, and how many old events were deleted.
    event_seq          BIGINT  NOT NULL DEFAULT 0,
    events_compacted   BIGINT  NOT NULL DEFAULT 0,
    stale_reminded_at  BIGINT,
    due_reminded_at    BIGINT
);
CREATE UNIQUE INDEX tasks_org_num ON tasks(org_id, num);
CREATE INDEX tasks_org_state ON tasks(org_id, state);
CREATE INDEX tasks_assignee ON tasks(assignee_id, state);
CREATE INDEX tasks_reporter ON tasks(reporter_id);
CREATE INDEX tasks_waiting_on ON tasks(waiting_on_id) WHERE waiting_on_id IS NOT NULL;
CREATE INDEX tasks_parent ON tasks(parent_id) WHERE parent_id IS NOT NULL;
CREATE INDEX tasks_team ON tasks(team_id) WHERE team_id IS NOT NULL;
CREATE INDEX tasks_thread ON tasks(thread_id) WHERE thread_id IS NOT NULL;
CREATE INDEX tasks_due ON tasks(due) WHERE due IS NOT NULL AND due_reminded_at IS NULL;

-- `task_id` depends on `blocker_id` (`blocker_id` blocks `task_id`).
CREATE TABLE task_deps (
    task_id     BIGINT NOT NULL REFERENCES tasks(id),
    blocker_id  BIGINT NOT NULL REFERENCES tasks(id),
    org_id      BIGINT NOT NULL REFERENCES orgs(id),
    created_at  BIGINT NOT NULL,
    PRIMARY KEY (task_id, blocker_id)
);
CREATE INDEX task_deps_blocker ON task_deps(blocker_id);

-- Threads linked to a task.
CREATE TABLE task_links (
    task_id     BIGINT NOT NULL REFERENCES tasks(id),
    thread_id   BIGINT NOT NULL,
    org_id      BIGINT NOT NULL REFERENCES orgs(id),
    seat_id     BIGINT REFERENCES seats(id),
    created_at  BIGINT NOT NULL,
    PRIMARY KEY (task_id, thread_id)
);
CREATE INDEX task_links_thread ON task_links(org_id, thread_id);

-- The history: appended, never changed while the task exists.
CREATE TABLE task_events (
    task_id     BIGINT NOT NULL REFERENCES tasks(id),
    seq         BIGINT NOT NULL,
    org_id      BIGINT NOT NULL REFERENCES orgs(id),
    at          BIGINT NOT NULL,
    actor_id    BIGINT REFERENCES seats(id),
    written_by  TEXT,
    kind        TEXT   NOT NULL,
    field       TEXT,
    from_value  TEXT,
    to_value    TEXT,
    note        TEXT,
    message_id  BIGINT,
    PRIMARY KEY (task_id, seq)
);
