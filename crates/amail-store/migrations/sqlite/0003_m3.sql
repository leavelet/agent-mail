-- M3: the team group of a seat (display only) and lists maintained by `amail up`.
ALTER TABLE seats ADD COLUMN group_name TEXT;
ALTER TABLE lists ADD COLUMN managed BOOLEAN NOT NULL DEFAULT FALSE;
