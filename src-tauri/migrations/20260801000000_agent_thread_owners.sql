-- Agent threads are local, but they still belong to the account that created
-- them. Existing rows remain NULL deliberately: they are the signed-out
-- principal's threads and are never visible while an account is signed in.
ALTER TABLE agent_threads ADD COLUMN owner_user_id TEXT;

CREATE INDEX idx_agent_threads_owner_updated
    ON agent_threads(owner_user_id, updated_at DESC);
