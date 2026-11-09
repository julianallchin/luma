-- Venue members edit the venue row.
--
-- Members are collaborators: everything in a venue is theirs to edit, and
-- `public.history` records who changed what. The venue's children already
-- allowed that through `can_access_venue`; the venue row itself was the
-- owner's alone, so a member's change to its name, environment or haze was
-- refused on upload.
--
-- What stays the owner's is the door: who owns the venue and the share code
-- that admits people to it. Row-level security cannot tell one column from
-- another, so a trigger refuses a member's change to either. Nobody changes
-- `uid` over a request — before this file the update policy's check already
-- pinned it to the caller, who was the owner. Inserting and deleting a venue
-- stay the owner's, as do memberships (see `venue_members_*` in the row
-- model).
--
-- Policy and trigger only; the local schema does not change. Any time; until
-- it runs, a member's venue-row edit uploads as a PATCH the update policy
-- filters to no rows, which PostgREST answers with success, and the next
-- download puts the old value back on the member's device.
--
-- Idempotent.

begin;

drop policy if exists venues_update on public.venues;
create policy venues_update on public.venues
    for update to authenticated
    using (uid = auth.uid() or public.is_venue_member(id))
    with check (uid = auth.uid() or public.is_venue_member(id));

create or replace function public.guard_venue_door() returns trigger
language plpgsql
security definer
set search_path = public, pg_temp
as $$
begin
    -- Outside a request (the SQL editor, a migration) there is no caller.
    if auth.uid() is null then
        return new;
    end if;
    if new.uid is distinct from old.uid then
        raise exception 'a venue''s owner cannot be changed'
            using errcode = '42501';
    end if;
    if new.share_code is distinct from old.share_code and old.uid is distinct from auth.uid() then
        raise exception 'only the venue''s owner can change its share code'
            using errcode = '42501';
    end if;
    return new;
end;
$$;

revoke all on function public.guard_venue_door() from public, anon, authenticated;

drop trigger if exists venues_guard_door on public.venues;
create trigger venues_guard_door before update on public.venues
    for each row execute function public.guard_venue_door();

commit;

notify pgrst, 'reload schema';
