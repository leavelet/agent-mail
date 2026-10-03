-- M2: seat permission mode and structured message fields.
ALTER TABLE seats ADD COLUMN permission_mode TEXT;
-- JSON object of string pairs, or '' when there are none.
ALTER TABLE messages ADD COLUMN meta TEXT NOT NULL DEFAULT '';
