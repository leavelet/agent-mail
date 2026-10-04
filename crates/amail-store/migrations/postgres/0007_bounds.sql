-- Bounds for public servers.

-- Browser sessions are looked up by token (at most a fixed number per
-- token; rotation and revocation end them) and pruned by expiry in the
-- background.
CREATE INDEX web_sessions_token ON web_sessions(token_id, created_at);
CREATE INDEX web_sessions_expires ON web_sessions(expires_at);
CREATE INDEX operator_sessions_token ON operator_sessions(token_id, created_at);
CREATE INDEX operator_sessions_expires ON operator_sessions(expires_at);
