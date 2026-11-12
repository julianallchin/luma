-- A venue's members can download the audio of every song a score in the
-- venue uses.
--
-- The rule was narrower than the one for the song's row: it asked for a
-- score by the song's *owner* in the venue. A member who scored another
-- member's song then saw the song and its beat grid (the sync rules send a
-- track any score in the venue plays) and could not fetch its audio, on a new
-- device or after a resync. The audio now follows the score, as the row does.

create or replace function public.can_read_venue_track(
    track_id text,
    metadata_owner uuid,
    include_members boolean default true
)
returns boolean
language sql
stable security definer
set search_path to ''
as $function$
    select auth.uid() is not null and exists (
        select 1 from public.tracks_live track
        join public.scores_live score on score.track_id = track.id
        join public.venues_live venue on venue.id = score.venue_id
        where track.id = $1 and track.uid = $2
          and (venue.uid = auth.uid() or ($3 and exists (
              select 1 from public.venue_members_live member
              where member.venue_id = venue.id and member.uid = auth.uid())))
    );
$function$;
