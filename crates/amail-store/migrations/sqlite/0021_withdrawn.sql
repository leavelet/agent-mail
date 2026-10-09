-- A delivery the sender (or the org's user) withdrew before the recipient
-- was shown, read or acknowledged it: when. Withdrawing also sets the
-- delivery's mode to `archive`, so that the counter triggers of 0012 drop
-- it from the seat's pending and unread counts, claims skip it and the
-- attention queue and acknowledgement reminders leave it out. The row
-- stays: the message is still readable by the recipient and part of its
-- threads, and its storage is still counted.
ALTER TABLE deliveries ADD COLUMN withdrawn_at INTEGER;
