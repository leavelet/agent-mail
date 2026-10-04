-- How many registrations with this invite were refused because the org
-- name was taken. At `amail_proto::limits::MAX_INVITE_NAME_CONFLICTS` the
-- invite can no longer be used, so that one invite cannot test many names.
ALTER TABLE invites ADD COLUMN name_conflicts BIGINT NOT NULL DEFAULT 0;

-- Link requests to org names that do not exist are no longer removed when
-- an org of that name is created; they expire like requests that the other
-- org has not accepted (`amail_proto::limits::LINK_REQUEST_DAYS`).
CREATE INDEX link_requests_created ON link_requests (created_at);
