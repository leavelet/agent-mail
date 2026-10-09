-- A message the org's user wrote in the name of one of its agent seats
-- (`POST /v1/seats/{name}/messages`): the name of the user seat. NULL for
-- every other message. The sender stays the agent seat; readers are shown
-- both.
ALTER TABLE messages ADD COLUMN written_by TEXT;
