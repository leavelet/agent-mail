-- Search and the archive start from an org's own messages, so the trigram
-- index over all messages is no longer used; it only slowed down sending.
DROP INDEX IF EXISTS messages_search;
-- A thread's and a seat's messages in id order, so that the newest come
-- first without sorting all of them.
CREATE INDEX messages_thread_id ON messages(thread_id, id);
DROP INDEX messages_thread;
CREATE INDEX messages_sender_id ON messages(sender_seat_id, id);
DROP INDEX messages_sender;
