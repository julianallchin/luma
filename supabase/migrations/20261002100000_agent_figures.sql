-- Python figures move out of `agent_thread_messages.parts_json` into Storage.
-- 485 base64 PNGs were 167 MB of synced rows; a stored figure now names its
-- object by `path` (`agent-figures/<uid>/<sha256>.png`) instead.
--
-- Private, and per owner: the first path segment is the uid, and only that
-- user can write or read under it. Objects are content-addressed, so the
-- client always uploads with `x-upsert`; an upsert over an existing object
-- needs UPDATE beside INSERT and SELECT, and rewrites the same bytes.

INSERT INTO storage.buckets (id, name, public)
VALUES ('agent-figures', 'agent-figures', false)
ON CONFLICT (id) DO NOTHING;

CREATE POLICY agent_figures_insert ON storage.objects
    FOR INSERT TO authenticated
    WITH CHECK (bucket_id = 'agent-figures'
        AND split_part(name, '/', 1) = (SELECT auth.uid())::text);

CREATE POLICY agent_figures_select ON storage.objects
    FOR SELECT TO authenticated
    USING (bucket_id = 'agent-figures'
        AND split_part(name, '/', 1) = (SELECT auth.uid())::text);

CREATE POLICY agent_figures_update ON storage.objects
    FOR UPDATE TO authenticated
    USING (bucket_id = 'agent-figures'
        AND split_part(name, '/', 1) = (SELECT auth.uid())::text)
    WITH CHECK (bucket_id = 'agent-figures'
        AND split_part(name, '/', 1) = (SELECT auth.uid())::text);
