-- A delivery that was read or acknowledged counts as notified. Before this
-- change, reading a message before a hook or wake had shown it left
-- `notified_at` NULL, so the delivery stayed in the pending counts and was
-- claimed and injected again. The store now sets `notified_at` with
-- `read_at`; this repairs the rows written before. The counter triggers of
-- 0012 update the seats' pending counts.
UPDATE deliveries SET notified_at = read_at, notified_via = 'read'
WHERE notified_at IS NULL AND read_at IS NOT NULL;
