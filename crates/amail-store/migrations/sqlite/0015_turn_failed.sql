-- The seat's last turn failed (`amail_proto::TurnFailure` as JSON), as
-- its node reports it.
ALTER TABLE presence ADD COLUMN turn_failed TEXT;
