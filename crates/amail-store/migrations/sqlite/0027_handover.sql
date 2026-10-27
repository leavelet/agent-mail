-- M9: seat handover. The handover note (one live note per seat, every
-- revision kept) and the worklog (an append-only record per seat of what
-- the seat did). Both count against the org's storage quota in
-- `orgs.task_bytes`. Keep in sync with migrations/postgres.

-- Per seat: the latest note revision, the team file setting
-- `handover = "private"`, and the last request for a note.
CREATE TABLE handover_seats (
    seat_id       BIGINT  PRIMARY KEY REFERENCES seats(id),
    org_id        BIGINT  NOT NULL REFERENCES orgs(id),
    note_rev      BIGINT  NOT NULL DEFAULT 0,
    -- Set by the team file: every note of the seat is private.
    private       BOOLEAN NOT NULL DEFAULT FALSE,
    requested_at  BIGINT,
    requested_by  BIGINT  REFERENCES seats(id)
);
CREATE INDEX handover_seats_org ON handover_seats(org_id);

-- One row per seat and revision. The seven fields are kept as seven
-- columns, in the order the note shows them.
CREATE TABLE handover_notes (
    id               INTEGER PRIMARY KEY,
    org_id           BIGINT  NOT NULL REFERENCES orgs(id),
    seat_id          BIGINT  NOT NULL REFERENCES seats(id),
    rev              BIGINT  NOT NULL,
    state            TEXT    NOT NULL,
    summary          TEXT    NOT NULL,
    next             TEXT    NOT NULL,
    waiting_on       TEXT    NOT NULL,
    traps            TEXT    NOT NULL,
    verify           TEXT    NOT NULL,
    pointers         TEXT    NOT NULL,
    -- `pointers` was filled in by the server from the worklog.
    pointers_filled  BOOLEAN NOT NULL DEFAULT FALSE,
    private          BOOLEAN NOT NULL DEFAULT FALSE,
    -- agent, compaction or server.
    source           TEXT    NOT NULL,
    session_id       TEXT,
    -- The user's name when the user wrote the note for the seat.
    written_by       TEXT,
    written_at       BIGINT  NOT NULL,
    read_by_session  TEXT,
    read_at          BIGINT,
    -- Bytes counted against the storage quota.
    bytes            BIGINT  NOT NULL
);
CREATE UNIQUE INDEX handover_notes_seat_rev ON handover_notes(seat_id, rev);
CREATE INDEX handover_notes_org ON handover_notes(org_id);

CREATE TABLE worklog (
    id          INTEGER PRIMARY KEY,
    org_id      BIGINT  NOT NULL REFERENCES orgs(id),
    seat_id     BIGINT  NOT NULL REFERENCES seats(id),
    session_id  TEXT,
    at          BIGINT  NOT NULL,
    kind        TEXT    NOT NULL,
    text        TEXT    NOT NULL,
    -- Task and document numbers of the org (not references: a removed
    -- task or document leaves the entry as it was).
    task_num    BIGINT,
    doc_num     BIGINT,
    -- server or node.
    source      TEXT    NOT NULL,
    bytes       BIGINT  NOT NULL
);
CREATE INDEX worklog_seat ON worklog(seat_id, id);
CREATE INDEX worklog_seat_at ON worklog(seat_id, at);
CREATE INDEX worklog_seat_task ON worklog(seat_id, task_num) WHERE task_num IS NOT NULL;
CREATE INDEX worklog_org ON worklog(org_id, id);
CREATE INDEX worklog_at ON worklog(at);
