CREATE TABLE public.track_beat_validations (
    track_id uuid PRIMARY KEY REFERENCES public.tracks(id) ON DELETE CASCADE,
    uid uuid NOT NULL REFERENCES auth.users(id),
    track_hash text NOT NULL,
    grid_json text NOT NULL,
    processor_version integer NOT NULL,
    approved integer NOT NULL CHECK (approved IN (0, 1)),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    deleted_at timestamptz,
    sync_seq bigint NOT NULL DEFAULT 0
);
ALTER TABLE public.track_beat_validations ENABLE ROW LEVEL SECURITY;
CREATE POLICY "Owners manage beat validations" ON public.track_beat_validations
FOR ALL TO authenticated USING (uid = auth.uid())
WITH CHECK (uid = auth.uid() AND EXISTS (
    SELECT 1 FROM public.tracks t WHERE t.id = track_id AND t.uid = auth.uid()
));
GRANT SELECT, INSERT, UPDATE, DELETE ON public.track_beat_validations TO authenticated;
CREATE TRIGGER sync_seq_bump BEFORE INSERT OR UPDATE ON public.track_beat_validations
FOR EACH ROW EXECUTE FUNCTION private.bump_sync_seq();
CREATE TRIGGER update_track_beat_validations_updated_at
BEFORE UPDATE ON public.track_beat_validations
FOR EACH ROW EXECUTE FUNCTION public.update_updated_at_column();
