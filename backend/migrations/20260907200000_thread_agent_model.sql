ALTER TABLE agent_threads ADD COLUMN model TEXT;
ALTER TABLE agent_threads ADD COLUMN provider TEXT
    CHECK (provider IN ('openrouter', 'vercel-ai-gateway', 'anthropic'));
UPDATE agent_threads SET
    model = CASE engine
        WHEN 'api' THEN COALESCE((SELECT value FROM settings WHERE key = 'agent_model'), 'claude-opus-5')
        WHEN 'codex' THEN NULLIF((SELECT value FROM settings WHERE key = 'agent_codex_model'), '')
        WHEN 'claude' THEN NULLIF((SELECT value FROM settings WHERE key = 'agent_claude_model'), '')
    END,
    provider = CASE WHEN engine = 'api' THEN
        COALESCE((SELECT value FROM settings WHERE key = 'agent_provider'), 'vercel-ai-gateway') END,
    synced_at = NULL;
