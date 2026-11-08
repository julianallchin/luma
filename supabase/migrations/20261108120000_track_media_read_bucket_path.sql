-- Venue members could never read a shared track's audio or stems: Storage
-- answered "Object not found" (400, NoSuchKey) for objects that were there.
--
-- `tracks.storage_path` and `track_stems.storage_path` name the object with
-- its bucket, `track-audio/<uid>/<hash>/audio.ogg`, as the client records the
-- upload. `storage.objects.name` has no bucket, `<uid>/<hash>/audio.ogg`. The
-- policy compared the two directly, so the member branch matched no row and
-- only the owner's own-prefix branch ever admitted a read. Storage reports a
-- row hidden by RLS as a missing object.

CREATE OR REPLACE FUNCTION public.can_read_track_media(bucket text, object_name text)
RETURNS boolean
LANGUAGE sql
STABLE SECURITY DEFINER
SET search_path TO ''
AS $function$
    SELECT auth.uid() IS NOT NULL AND bucket IN ('track-audio', 'track-stems') AND (
        split_part(object_name, '/', 1) = auth.uid()::text
        OR EXISTS (
            SELECT 1 FROM public.tracks track
            WHERE split_part(object_name, '/', 1) = track.uid::text
              AND public.can_read_venue_track(track.id, track.uid)
              AND ((bucket = 'track-audio' AND track.storage_path = bucket || '/' || object_name)
                OR (bucket = 'track-stems' AND EXISTS (
                    SELECT 1 FROM public.track_stems stem
                    WHERE stem.track_id = track.id AND stem.uid = track.uid
                      AND stem.storage_path = bucket || '/' || object_name)))
        )
    );
$function$;
