-- Execution claims never expire into permission to take over another machine.
-- Only the originating device can recover its claim after a local process dies.
CREATE TABLE public.agent_thread_runs (
    owner_user_id text NOT NULL,
    thread_id text NOT NULL,
    device_id text NOT NULL,
    run_id text NOT NULL,
    PRIMARY KEY (owner_user_id, thread_id)
);
ALTER TABLE public.agent_thread_runs ENABLE ROW LEVEL SECURITY;
CREATE POLICY agent_thread_runs_owner_select ON public.agent_thread_runs
    FOR SELECT TO authenticated USING (owner_user_id = auth.uid()::text);
GRANT SELECT ON public.agent_thread_runs TO authenticated;

CREATE FUNCTION public.claim_agent_thread_run(p_thread_id text, p_device_id text, p_run_id text)
RETURNS boolean LANGUAGE plpgsql SECURITY DEFINER SET search_path = '' AS $$
DECLARE claimed boolean;
BEGIN
    IF auth.uid() IS NULL THEN RAISE EXCEPTION 'authentication required'; END IF;
    INSERT INTO public.agent_thread_runs (owner_user_id, thread_id, device_id, run_id)
    VALUES (auth.uid()::text, p_thread_id, p_device_id, p_run_id)
    ON CONFLICT (owner_user_id, thread_id) DO UPDATE SET run_id = EXCLUDED.run_id
    WHERE agent_thread_runs.device_id = EXCLUDED.device_id
    RETURNING true INTO claimed;
    RETURN COALESCE(claimed, false);
END;
$$;

CREATE FUNCTION public.release_agent_thread_run(p_thread_id text, p_device_id text, p_run_id text)
RETURNS void LANGUAGE sql SECURITY DEFINER SET search_path = '' AS $$
    DELETE FROM public.agent_thread_runs
    WHERE owner_user_id = auth.uid()::text AND thread_id = p_thread_id
        AND device_id = p_device_id AND run_id = p_run_id;
$$;
REVOKE ALL ON FUNCTION public.claim_agent_thread_run(text,text,text) FROM PUBLIC;
REVOKE ALL ON FUNCTION public.release_agent_thread_run(text,text,text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION public.claim_agent_thread_run(text,text,text) TO authenticated;
GRANT EXECUTE ON FUNCTION public.release_agent_thread_run(text,text,text) TO authenticated;
