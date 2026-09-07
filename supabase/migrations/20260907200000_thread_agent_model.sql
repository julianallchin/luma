ALTER TABLE public.agent_threads ADD COLUMN model text;
ALTER TABLE public.agent_threads ADD COLUMN provider text
    CHECK (provider IN ('openrouter', 'vercel-ai-gateway', 'anthropic'));
CREATE OR REPLACE FUNCTION private.guard_agent_thread_update()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog
AS $$
BEGIN
    IF (to_jsonb(NEW) - ARRAY['sync_seq', 'title', 'engine', 'model', 'provider', 'lifecycle_state', 'updated_at'])
        IS DISTINCT FROM (to_jsonb(OLD) - ARRAY['sync_seq', 'title', 'engine', 'model', 'provider', 'lifecycle_state', 'updated_at'])
    THEN
        RAISE EXCEPTION 'agent thread identity collision' USING ERRCODE = '23505';
    END IF;
    IF OLD.lifecycle_state = 'deleting' THEN
        NEW.lifecycle_state := 'deleting';
    ELSIF NEW.lifecycle_state = 'deleting' AND NOT EXISTS (
        SELECT 1
          FROM public.agent_thread_deletions deletion
         WHERE deletion.thread_id = NEW.id
           AND deletion.owner_user_id = NEW.owner_user_id
    ) THEN
        RAISE EXCEPTION 'agent thread deletion requires its immutable deletion fact'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END
$$;

UPDATE public.agent_threads SET model = 'claude-opus-5', provider = 'vercel-ai-gateway'
WHERE engine = 'api';
