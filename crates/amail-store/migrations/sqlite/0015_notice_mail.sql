-- Notice mail of the mail system (`POST /v1/node/notices`). It is charged
-- to nobody's storage quota; instead each org may receive a bounded amount
-- per day (`amail_proto::limits::NOTICE_MAILS_PER_DAY` and
-- `NOTICE_BYTES_PER_DAY`, counted from these rows), and it is deleted after
-- `amail_proto::limits::NOTICE_RETENTION_DAYS`. Notices sent before this
-- migration were charged and stay ordinary mail.
ALTER TABLE messages ADD COLUMN notice BOOLEAN NOT NULL DEFAULT FALSE;
CREATE INDEX messages_notice ON messages (org_id, created_at) WHERE notice;
