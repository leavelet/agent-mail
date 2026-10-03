-- M4: notification webhook, web sessions, full-text search.
ALTER TABLE orgs ADD COLUMN webhook_url TEXT;

-- Browser sessions: the cookie's SHA-256 hash, tied to the user token used
-- to sign in (revoking that token ends its sessions).
CREATE TABLE web_sessions (
    hash        TEXT    PRIMARY KEY,
    token_id    BIGINT  NOT NULL REFERENCES tokens(id),
    created_at  BIGINT  NOT NULL,
    expires_at  BIGINT  NOT NULL
);

CREATE INDEX messages_created ON messages(created_at);

-- Substring search over subjects and bodies (any script, including Chinese):
-- an external-content FTS5 table with the trigram tokenizer. Messages are
-- never updated or deleted, so an insert trigger keeps it current.
CREATE VIRTUAL TABLE messages_fts USING fts5(
    subject, body, content='messages', content_rowid='id', tokenize='trigram'
);
INSERT INTO messages_fts(rowid, subject, body) SELECT id, subject, body FROM messages;
CREATE TRIGGER messages_fts_insert AFTER INSERT ON messages BEGIN
    INSERT INTO messages_fts(rowid, subject, body) VALUES (new.id, new.subject, new.body);
END;
