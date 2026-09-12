-- A venue's cues carry their patterns to its members.
--
-- `cues.pattern_id` is a foreign key on both sides, and a member holding a cue
-- whose pattern they may not read holds a cue that plays nothing — locally the
-- deferred foreign key refuses the whole checkpoint, so nothing at all
-- arrives. Read only: the pattern still belongs to its author, and only the
-- author may write it.
--
-- Idempotent: this one is applied by hand.

create or replace function public.pattern_is_cued (pattern_id text) returns boolean
language sql
stable
security definer
set search_path = public, pg_temp
as $$
    select exists (
        select 1 from public.cues c
        where c.pattern_id = pattern_is_cued.pattern_id
          and public.can_access_venue(c.venue_id)
    );
$$;

create or replace function public.can_read_pattern (pattern_id text) returns boolean
language sql
stable
security definer
set search_path = public, pg_temp
as $$
    select exists (
        select 1 from public.patterns p
        where p.id = pattern_id and (p.uid = auth.uid() or p.is_verified)
    ) or public.pattern_is_cued(pattern_id);
$$;

drop policy if exists patterns_select on public.patterns;
create policy patterns_select on public.patterns for select to authenticated
    using (uid = auth.uid() or is_verified or public.pattern_is_cued(id));
