-- Storage reads of track audio and stems broke on the row model: every read
-- went out as "API error 500 ... code: 42883".
--
-- `storage.objects` has one read policy, `can_read_track_media`, which calls
-- `can_read_venue_track(track.id, track.uid)`. The row model retyped every id
-- from uuid to text, so that call became (text, uuid) and matched no function
-- -- 42883 is "undefined function", not a permissions failure, which is why it
-- surfaced as a 500 rather than a 403.
--
-- Three more references in the same pair had gone stale behind it, each of
-- which would have thrown 42703 in turn once the signature was fixed:
-- `tracks.deleted_at`, `scores.deleted_at` and `venues.deleted_at` no longer
-- exist (deletes are rows now, not flags), `track_stems.deleted_at` likewise,
-- and `venue_members.user_id` is `venue_members.uid`.

DROP FUNCTION IF EXISTS public.can_read_venue_track(uuid, uuid, boolean);

CREATE FUNCTION public.can_read_venue_track(
    track_id text,
    metadata_owner uuid,
    include_members boolean DEFAULT true
)
RETURNS boolean
LANGUAGE sql
STABLE SECURITY DEFINER
SET search_path TO ''
AS $function$
    SELECT auth.uid() IS NOT NULL AND EXISTS (
        SELECT 1 FROM public.tracks track
        JOIN public.scores score ON score.track_id = track.id AND score.uid = track.uid
        JOIN public.venues venue ON venue.id = score.venue_id
        WHERE track.id = $1 AND track.uid = $2
          AND (venue.uid = auth.uid() OR ($3 AND EXISTS (
              SELECT 1 FROM public.venue_members member
              WHERE member.venue_id = venue.id AND member.uid = auth.uid())))
    );
$function$;

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
              AND ((bucket = 'track-audio' AND track.storage_path = object_name)
                OR (bucket = 'track-stems' AND EXISTS (
                    SELECT 1 FROM public.track_stems stem
                    WHERE stem.track_id = track.id AND stem.uid = track.uid
                      AND stem.storage_path = object_name)))
        )
    );
$function$;
