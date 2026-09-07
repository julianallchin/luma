ALTER TABLE agent_threads ADD COLUMN engine TEXT NOT NULL DEFAULT 'api'
    CHECK (engine IN ('api', 'codex', 'claude'));
