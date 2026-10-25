-- Lasers (docs/design/lasers.md): a venue's zones, and each projector's
-- optional limit.
--
-- A zone is a named window of directions in the room that laser clips draw
-- in: `turn` 0° toward the house, positive toward stage right; `tilt` 0°
-- level, positive up. A limit is a polygon in one projector's own view,
-- galvo coordinates −1 to 1, outside which its light is blanked; `id` is the
-- projector fixture's id.
--
-- Venue content: the owner and every member may read and write, the same
-- policy `folders` has.
--
-- Symmetric with backend/migrations/20261024120000_lasers.sql.
--
-- Order:
--   1. Apply this file.
--   2. Redeploy `deploy/powersync/sync-config.yaml`, which now names both tables.
--
-- Idempotent.

begin;

create table if not exists public.laser_zones (
    id text primary key,
    uid uuid not null default auth.uid() references auth.users (id) on delete cascade,
    venue_id text not null references public.venues (id) on delete cascade deferrable initially deferred,
    name text not null,
    turn_min double precision not null,
    turn_max double precision not null,
    tilt_min double precision not null,
    tilt_max double precision not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create index if not exists laser_zones_venue_idx on public.laser_zones (venue_id);

create table if not exists public.laser_limits (
    id text primary key references public.fixtures (id) on delete cascade deferrable initially deferred,
    uid uuid not null default auth.uid() references auth.users (id) on delete cascade,
    venue_id text not null references public.venues (id) on delete cascade deferrable initially deferred,
    points text not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create index if not exists laser_limits_venue_idx on public.laser_limits (venue_id);

do $$
declare
    name text;
begin
    foreach name in array array['laser_zones', 'laser_limits'] loop
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

commit;

notify pgrst, 'reload schema';
