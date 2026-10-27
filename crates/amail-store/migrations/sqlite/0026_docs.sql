-- M9: documents (an org's knowledge base): kinds, documents, their
-- revisions, the sections and links of the current revision, relations,
-- links to threads and tasks, and history. Keep in sync with
-- migrations/postgres (the search index differs).

-- The last document number the org gave out (numbers are never reused).
ALTER TABLE orgs ADD COLUMN org_doc_seq BIGINT NOT NULL DEFAULT 0;
-- Bytes of the org's documents counted against the storage quota (with
-- `stored_bytes` and `task_bytes`).
ALTER TABLE orgs ADD COLUMN doc_bytes BIGINT NOT NULL DEFAULT 0;
-- `human`: every document of the team's seats waits for the user to
-- activate it (NULL: the kind decides).
ALTER TABLE teams ADD COLUMN doc_activate_by TEXT;

-- Document kinds, per org. The five built-in kinds are created for every
-- org (here for existing orgs, by the server for new ones).
CREATE TABLE doc_kinds (
    id                 INTEGER PRIMARY KEY,
    org_id             BIGINT  NOT NULL REFERENCES orgs(id),
    name               TEXT    NOT NULL,
    description        TEXT    NOT NULL DEFAULT '',
    -- Heading titles the body must contain, `,`-separated (empty: none).
    required_sections  TEXT    NOT NULL DEFAULT '',
    -- `owner` or `human`: who may move a document from `draft` to `active`.
    activate_by        TEXT    NOT NULL DEFAULT 'owner',
    in_index           BOOLEAN NOT NULL DEFAULT TRUE,
    body_cap_bytes     BIGINT  NOT NULL DEFAULT 524288,
    -- Default interval of `verify`, in days (NULL: no expiry).
    verify_days        BIGINT,
    builtin            BOOLEAN NOT NULL DEFAULT FALSE,
    -- No new documents of this kind.
    disabled           BOOLEAN NOT NULL DEFAULT FALSE,
    created_at         BIGINT  NOT NULL
);
CREATE UNIQUE INDEX doc_kinds_org_name ON doc_kinds(org_id, name);

INSERT INTO doc_kinds (org_id, name, description, required_sections, activate_by, in_index, verify_days, builtin, created_at)
SELECT o.id, k.name, k.description, k.required_sections, k.activate_by, k.in_index, k.verify_days, TRUE,
       CAST(strftime('%s', 'now') AS INTEGER) * 1000
  FROM orgs o, (
    SELECT 'decision' AS name, 'why something was decided; frozen once active' AS description,
           'Context,Decision,Alternatives,Consequences' AS required_sections, 'human' AS activate_by,
           TRUE AS in_index, NULL AS verify_days
    UNION ALL SELECT 'finding', 'a verified fact about a system or harness, with the evidence',
           'Observed,Evidence,Applies to', 'owner', TRUE, 30
    UNION ALL SELECT 'runbook', 'a procedure', 'When,Steps,Verify', 'owner', TRUE, 60
    UNION ALL SELECT 'index', 'a curated list of documents on a topic', '', 'owner', TRUE, 30
    UNION ALL SELECT 'reference', 'imported or long material; searchable, never injected', '', 'owner', FALSE, NULL
  ) k
 WHERE o.deleted_at IS NULL;

CREATE TABLE docs (
    id                    INTEGER PRIMARY KEY,
    org_id                BIGINT  NOT NULL REFERENCES orgs(id),
    num                   BIGINT  NOT NULL,
    slug                  TEXT,
    kind                  TEXT    NOT NULL,
    -- The current revision's title and description (also in the revision).
    title                 TEXT    NOT NULL,
    description           TEXT    NOT NULL DEFAULT '',
    -- draft, active, superseded, archived.
    status                TEXT    NOT NULL DEFAULT 'draft',
    owner_id              BIGINT  NOT NULL REFERENCES seats(id),
    team_id               BIGINT  REFERENCES teams(id),
    -- `,a,b,` (empty: none); editors are seat names.
    labels                TEXT    NOT NULL DEFAULT '',
    editors               TEXT    NOT NULL DEFAULT '',
    pinned                BOOLEAN NOT NULL DEFAULT FALSE,
    -- The current revision, its body's size, token estimate and SHA-256.
    rev                   BIGINT  NOT NULL DEFAULT 0,
    body_bytes            BIGINT  NOT NULL DEFAULT 0,
    tokens                BIGINT  NOT NULL DEFAULT 0,
    sha256                TEXT    NOT NULL DEFAULT '',
    verified_at           BIGINT,
    verified_by           TEXT,
    expires_at            BIGINT,
    expiry_reminded_at    BIGINT,
    dangling_reminded_at  BIGINT,
    -- The discussion thread.
    thread_id             BIGINT,
    -- The last event's seq.
    event_seq             BIGINT  NOT NULL DEFAULT 0,
    client_key            TEXT,
    created_at            BIGINT  NOT NULL,
    updated_at            BIGINT  NOT NULL
);
CREATE UNIQUE INDEX docs_org_num ON docs(org_id, num);
CREATE UNIQUE INDEX docs_org_slug ON docs(org_id, slug) WHERE slug IS NOT NULL;
CREATE INDEX docs_org_kind ON docs(org_id, kind, num);
CREATE INDEX docs_org_status ON docs(org_id, status, num);
CREATE INDEX docs_owner ON docs(owner_id);
CREATE INDEX docs_team ON docs(team_id) WHERE team_id IS NOT NULL;
CREATE INDEX docs_thread ON docs(thread_id) WHERE thread_id IS NOT NULL;
CREATE INDEX docs_expires ON docs(expires_at) WHERE expires_at IS NOT NULL AND expiry_reminded_at IS NULL;
CREATE INDEX docs_client_key ON docs(org_id, client_key) WHERE client_key IS NOT NULL;

-- Every revision, whole (title, description, body). Never changed.
CREATE TABLE doc_revisions (
    doc_id       BIGINT NOT NULL REFERENCES docs(id),
    rev          BIGINT NOT NULL,
    org_id       BIGINT NOT NULL REFERENCES orgs(id),
    title        TEXT   NOT NULL,
    description  TEXT   NOT NULL DEFAULT '',
    body         TEXT   NOT NULL,
    sha256       TEXT   NOT NULL,
    message      TEXT,
    author_id    BIGINT REFERENCES seats(id),
    written_by   TEXT,
    at           BIGINT NOT NULL,
    PRIMARY KEY (doc_id, rev)
);

-- The sections of each document's current revision: one per heading, plus
-- the text before the first heading (level 0, anchor ''). `start_byte`,
-- `end_byte`: the section with its subsections; `text`: the section's own
-- text (up to the next heading), which search reads.
CREATE TABLE doc_sections (
    id          INTEGER PRIMARY KEY,
    doc_id      BIGINT  NOT NULL REFERENCES docs(id),
    org_id      BIGINT  NOT NULL REFERENCES orgs(id),
    rev         BIGINT  NOT NULL,
    ord         BIGINT  NOT NULL,
    level       BIGINT  NOT NULL,
    anchor      TEXT    NOT NULL,
    heading     TEXT    NOT NULL,
    path        TEXT    NOT NULL,
    start_byte  BIGINT  NOT NULL,
    end_byte    BIGINT  NOT NULL,
    tokens      BIGINT  NOT NULL,
    text        TEXT    NOT NULL
);
CREATE INDEX doc_sections_doc ON doc_sections(doc_id, ord);
CREATE INDEX doc_sections_anchor ON doc_sections(doc_id, anchor);

-- Substring search over sections (any script, including Chinese): an
-- external-content FTS5 table with the trigram tokenizer. Sections are
-- only inserted and deleted.
CREATE VIRTUAL TABLE doc_sections_fts USING fts5(
    path, text, content='doc_sections', content_rowid='id', tokenize='trigram'
);
CREATE TRIGGER doc_sections_fts_insert AFTER INSERT ON doc_sections BEGIN
    INSERT INTO doc_sections_fts(rowid, path, text) VALUES (new.id, new.path, new.text);
END;
CREATE TRIGGER doc_sections_fts_delete AFTER DELETE ON doc_sections BEGIN
    INSERT INTO doc_sections_fts(doc_sections_fts, rowid, path, text) VALUES ('delete', old.id, old.path, old.text);
END;

-- Links out of each document's current revision. `target_kind`: doc,
-- task, message, thread, seat, path. `target_num`: the number of a doc,
-- task, message or thread; `target_id`: the target as text (number, seat
-- name, path).
CREATE TABLE doc_links (
    id                     INTEGER PRIMARY KEY,
    org_id                 BIGINT  NOT NULL REFERENCES orgs(id),
    src_doc                BIGINT  NOT NULL REFERENCES docs(id),
    src_rev                BIGINT  NOT NULL,
    src_anchor             TEXT    NOT NULL DEFAULT '',
    target_kind            TEXT    NOT NULL,
    target_id              TEXT    NOT NULL,
    target_num             BIGINT,
    target_anchor          TEXT,
    target_line            BIGINT,
    target_title_at_write  TEXT,
    -- Repository paths: whether the node found the path (NULL: not known).
    path_exists            BOOLEAN
);
CREATE INDEX doc_links_src ON doc_links(src_doc);
CREATE INDEX doc_links_target ON doc_links(org_id, target_kind, target_num);

-- Typed relations: `parent` (src's parent is dst), `supersedes` (src
-- replaces dst), `supplements` (src supplements dst), `related`.
CREATE TABLE doc_relations (
    src_id      BIGINT NOT NULL REFERENCES docs(id),
    kind        TEXT   NOT NULL,
    dst_id      BIGINT NOT NULL REFERENCES docs(id),
    org_id      BIGINT NOT NULL REFERENCES orgs(id),
    created_at  BIGINT NOT NULL,
    PRIMARY KEY (src_id, kind, dst_id)
);
CREATE INDEX doc_relations_dst ON doc_relations(dst_id, kind);

-- The history: appended, never changed while the document exists. `rev`:
-- the revision the event created (NULL: none).
CREATE TABLE doc_events (
    doc_id      BIGINT NOT NULL REFERENCES docs(id),
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
    rev         BIGINT,
    PRIMARY KEY (doc_id, seq)
);

-- Threads that cite a document (`[d12]` in a subject).
CREATE TABLE doc_threads (
    doc_id      BIGINT NOT NULL REFERENCES docs(id),
    thread_id   BIGINT NOT NULL,
    org_id      BIGINT NOT NULL REFERENCES orgs(id),
    seat_id     BIGINT REFERENCES seats(id),
    created_at  BIGINT NOT NULL,
    PRIMARY KEY (doc_id, thread_id)
);
CREATE INDEX doc_threads_thread ON doc_threads(org_id, thread_id);

-- Tasks that cite a document (`[d12]` in a task title).
CREATE TABLE doc_task_links (
    doc_id      BIGINT NOT NULL REFERENCES docs(id),
    task_id     BIGINT NOT NULL REFERENCES tasks(id),
    org_id      BIGINT NOT NULL REFERENCES orgs(id),
    created_at  BIGINT NOT NULL,
    PRIMARY KEY (doc_id, task_id)
);
CREATE INDEX doc_task_links_task ON doc_task_links(task_id);
