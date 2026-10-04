-- Search and the archive start from an org's own messages, so the full-text
-- index over all messages is no longer used; it only slowed down sending.
-- (SQLite indexes already end with the rowid, so `messages_thread` and
-- `messages_sender` give a thread's and a seat's messages in id order.)
DROP TRIGGER IF EXISTS messages_fts_insert;
DROP TRIGGER IF EXISTS messages_fts_delete;
DROP TRIGGER IF EXISTS messages_fts_update;
DROP TABLE IF EXISTS messages_fts;
