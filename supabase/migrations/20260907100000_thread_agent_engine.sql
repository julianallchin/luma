ALTER TABLE public.agent_threads ADD COLUMN engine text NOT NULL DEFAULT 'api'
    CHECK (engine IN ('api', 'codex', 'claude'));
