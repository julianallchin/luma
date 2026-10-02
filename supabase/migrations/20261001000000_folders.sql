-- Folders: a venue's named sets of songs (docs/specs/venue-tabs.md, phase 2).
--
-- A folder holds links to tracks, many to many. A song's scores belong to the
-- song (track + venue), not to a folder. Deleting a folder deletes its links
-- and nothing else.
--
-- Venue content: the owner and every member may read and write, the same
-- policy `fixture_groups` has. `folder_tracks` carries `venue_id` so the sync
-- rules and the policy reach it in one hop; the composite foreign key keeps
-- it equal to its folder's.
--
-- Symmetric with backend/migrations/20261001000000_folders.sql.
--
-- Order:
--   1. Apply this file.
--   2. Redeploy `deploy/powersync/sync-config.yaml`, which now names both tables.
--
-- Idempotent.

begin;

create table if not exists public.folders (
    id text primary key,
    uid uuid not null default auth.uid() references auth.users (id) on delete cascade,
    venue_id text not null references public.venues (id) on delete cascade deferrable initially deferred,
    name text not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (id, venue_id)
);

create index if not exists folders_venue_idx on public.folders (venue_id);

-- `id` is the client's `folder_id || ':' || track_id`, as every natural-key
-- table spells it.
create table if not exists public.folder_tracks (
    id text primary key,
    uid uuid not null default auth.uid() references auth.users (id) on delete cascade,
    venue_id text not null,
    folder_id text not null,
    track_id text not null references public.tracks (id) on delete cascade deferrable initially deferred,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (folder_id, track_id),
    check (id = folder_id || ':' || track_id),
    foreign key (folder_id, venue_id) references public.folders (id, venue_id)
        on delete cascade deferrable initially deferred
);

create index if not exists folder_tracks_venue_idx on public.folder_tracks (venue_id);
create index if not exists folder_tracks_track_idx on public.folder_tracks (track_id);

do $$
declare
    name text;
begin
    foreach name in array array['folders', 'folder_tracks'] loop
        execute format('alter table public.%I enable row level security', name);
        execute format('revoke all on public.%I from anon, public', name);
        execute format('grant select, insert, update, delete on public.%I to authenticated', name);
        execute format('grant select on public.%I to powersync_role', name);

        execute format('drop policy if exists %I on public.%I', name || '_select', name);
        execute format('drop policy if exists %I on public.%I', name || '_insert', name);
        execute format('drop policy if exists %I on public.%I', name || '_update', name);
        execute format('drop policy if exists %I on public.%I', name || '_delete', name);
        execute format('create policy %I on public.%I for select to authenticated
                        using (public.can_access_venue(venue_id))',
                       name || '_select', name);
        execute format('create policy %I on public.%I for insert to authenticated
                        with check (public.can_access_venue(venue_id))',
                       name || '_insert', name);
        execute format('create policy %I on public.%I for update to authenticated
                        using (public.can_access_venue(venue_id))
                        with check (public.can_access_venue(venue_id))',
                       name || '_update', name);
        execute format('create policy %I on public.%I for delete to authenticated
                        using (public.can_access_venue(venue_id))',
                       name || '_delete', name);

        -- Authored rows: the server keeps their history.
        execute format('drop trigger if exists %I on public.%I', name || '_history', name);
        execute format(
            'create trigger %I after insert or update or delete on public.%I
             for each row execute function public.record_history()',
            name || '_history', name);

        if not exists (
            select 1 from pg_publication_tables
            where pubname = 'powersync' and schemaname = 'public' and tablename = name
        ) then
            execute format('alter publication powersync add table public.%I', name);
        end if;
    end loop;
end
$$;

-- Every venue starts with one folder, "Testing", holding every song it has.
-- The id is derived from the venue, and the local migration derives the same
-- one, so a device's copy and this one are one row.
insert into public.folders (id, uid, venue_id, name)
select v.id || ':testing', v.uid, v.id, 'Testing'
from public.venues v
on conflict (id) do nothing;

insert into public.folder_tracks (id, uid, venue_id, folder_id, track_id)
select distinct v.id || ':testing:' || s.track_id, v.uid, v.id, v.id || ':testing', s.track_id
from public.scores s
join public.venues v on v.id = s.venue_id
on conflict (id) do nothing;

commit;

notify pgrst, 'reload schema';
