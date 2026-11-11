-- Venue members could never read a shared track's cover art: Storage
-- answered "Object not found" (400, NoSuchKey) for covers that were there.
--
-- A cover is stored beside its audio, `track-audio/<uid>/<hash>/cover.<ext>`,
-- and named by `tracks.album_art_storage_path`. The member branch admitted an
-- object in `track-audio` only when it was the track's audio, so the cover
-- matched no row and only the owner's own-prefix branch ever admitted a read.

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
              AND ((bucket = 'track-audio'
                    AND bucket || '/' || object_name
                        IN (track.storage_path, track.album_art_storage_path))
                OR (bucket = 'track-stems' AND EXISTS (
                    SELECT 1 FROM public.track_stems stem
                    WHERE stem.track_id = track.id AND stem.uid = track.uid
                      AND stem.storage_path = bucket || '/' || object_name)))
        )
    );
$function$;
