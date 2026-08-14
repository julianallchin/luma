-- Per-track, per-bar Discogs style activations (Discogs-EffNet).
-- Mirrors track_bar_classifications exactly: shape, RLS, triggers.
-- Applied to production 2026-08-14 via MCP (create_track_genres).

CREATE TABLE public.track_genres (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    uid uuid REFERENCES auth.users (id),
    track_id uuid UNIQUE REFERENCES public.tracks (id),
    genres_json text,
    labels_json text,
    processor_version integer DEFAULT 1,
    created_at timestamptz DEFAULT (CURRENT_TIMESTAMP AT TIME ZONE 'UTC'::text),
    updated_at timestamptz DEFAULT (CURRENT_TIMESTAMP AT TIME ZONE 'UTC'::text),
    deleted_at timestamptz,
    sync_seq bigint DEFAULT 0
);

ALTER TABLE public.track_genres ENABLE ROW LEVEL SECURITY;

CREATE POLICY "Users can manage own track_genres"
    ON public.track_genres FOR ALL
    USING (uid = auth.uid())
    WITH CHECK (uid = auth.uid());

CREATE POLICY "Members can read venue track_genres"
    ON public.track_genres FOR SELECT
    USING (EXISTS (
        SELECT 1 FROM scores
        WHERE scores.track_id = track_genres.track_id
          AND is_venue_member(scores.venue_id)
    ));

CREATE POLICY "Owners can read venue track_genres"
    ON public.track_genres FOR SELECT
    USING (EXISTS (
        SELECT 1 FROM scores
        WHERE scores.track_id = track_genres.track_id
          AND is_venue_owner(scores.venue_id)
    ));

CREATE TRIGGER sync_seq_bump
    BEFORE INSERT OR UPDATE ON public.track_genres
    FOR EACH ROW EXECUTE FUNCTION private.bump_sync_seq();

CREATE TRIGGER update_track_genres_updated_at
    BEFORE UPDATE ON public.track_genres
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();
