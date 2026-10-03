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
-- a trigram GIN index used by ILIKE. pg_trgm is a trusted extension, so the
-- database owner can create it.
CREATE EXTENSION IF NOT EXISTS pg_trgm;
CREATE INDEX messages_search ON messages USING gin ((subject || ' ' || body) gin_trgm_ops);
