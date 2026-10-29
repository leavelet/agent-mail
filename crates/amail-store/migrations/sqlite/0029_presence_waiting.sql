-- The request (`r<num>`) a seat waits for after asking the person, as
-- its node reports it (M10).
ALTER TABLE presence ADD COLUMN waiting_for BIGINT;
