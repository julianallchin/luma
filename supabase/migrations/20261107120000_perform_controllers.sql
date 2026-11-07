-- Controllers, and the bindings they hold, sync with the venue.
--
-- `perform_controllers`: a venue has any number. `port` is the MIDI port
-- name the controller matches (empty until heard); `profile_json` says where
-- its pads and faders are and how it shows a light (`inputs::profile` in the
-- backend).
--
-- `control_bindings` was this machine's alone and never on the server; it
-- arrives here belonging to a controller, one binding per controller,
-- control and input. Clients upload the bindings they kept as they open each
-- venue.
--
-- Venue content: the owner and every member may read and write, the same
-- policy the Perform tables have.
--
-- Symmetric with backend/migrations/20261107120000_perform_controllers.sql.
--
-- Order:
--   1. Apply this file.
--   2. Redeploy `deploy/powersync/sync-config.yaml`, which now names both tables.
--   3. Ship the client that syncs them.
--
-- Idempotent.

begin;

create table if not exists public.perform_controllers (
    id text primary key,
    uid uuid not null default auth.uid() references auth.users (id) on delete cascade,
    venue_id text not null references public.venues (id) on delete cascade deferrable initially deferred,
    name text not null default '',
    port text not null default '',
    profile_json text not null default '{}',
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create index if not exists perform_controllers_venue_idx on public.perform_controllers (venue_id);

create table if not exists public.control_bindings (
    id text primary key,
    uid uuid not null default auth.uid() references auth.users (id) on delete cascade,
    venue_id text not null references public.venues (id) on delete cascade deferrable initially deferred,
    controller_id text not null references public.perform_controllers (id) on delete cascade deferrable initially deferred,
    control text not null,
    address_json text not null,
    mode text not null check (mode in ('fader', 'momentary', 'toggle')),
    invert boolean not null default false,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (controller_id, control, address_json)
);

create index if not exists control_bindings_venue_idx on public.control_bindings (venue_id);

alter table public.perform_controllers enable row level security;
revoke all on public.perform_controllers from anon, public;
grant select, insert, update, delete on public.perform_controllers to authenticated;
grant select on public.perform_controllers to powersync_role;

drop policy if exists perform_controllers_select on public.perform_controllers;
drop policy if exists perform_controllers_insert on public.perform_controllers;
drop policy if exists perform_controllers_update on public.perform_controllers;
drop policy if exists perform_controllers_delete on public.perform_controllers;
create policy perform_controllers_select on public.perform_controllers for select to authenticated
    using (public.can_access_venue(venue_id));
create policy perform_controllers_insert on public.perform_controllers for insert to authenticated
    with check (public.can_access_venue(venue_id));
create policy perform_controllers_update on public.perform_controllers for update to authenticated
    using (public.can_access_venue(venue_id))
    with check (public.can_access_venue(venue_id));
create policy perform_controllers_delete on public.perform_controllers for delete to authenticated
    using (public.can_access_venue(venue_id));

-- Authored rows: the server keeps their history.
drop trigger if exists perform_controllers_history on public.perform_controllers;
create trigger perform_controllers_history after insert or update or delete on public.perform_controllers
    for each row execute function public.record_history();

do $$
begin
    if not exists (
        select 1 from pg_publication_tables
        where pubname = 'powersync' and schemaname = 'public' and tablename = 'perform_controllers'
    ) then
        alter publication powersync add table public.perform_controllers;
    end if;
end
$$;

alter table public.control_bindings enable row level security;
revoke all on public.control_bindings from anon, public;
grant select, insert, update, delete on public.control_bindings to authenticated;
grant select on public.control_bindings to powersync_role;

drop policy if exists control_bindings_select on public.control_bindings;
drop policy if exists control_bindings_insert on public.control_bindings;
drop policy if exists control_bindings_update on public.control_bindings;
drop policy if exists control_bindings_delete on public.control_bindings;
create policy control_bindings_select on public.control_bindings for select to authenticated
    using (public.can_access_venue(venue_id));
create policy control_bindings_insert on public.control_bindings for insert to authenticated
    with check (public.can_access_venue(venue_id));
create policy control_bindings_update on public.control_bindings for update to authenticated
    using (public.can_access_venue(venue_id))
    with check (public.can_access_venue(venue_id));
create policy control_bindings_delete on public.control_bindings for delete to authenticated
    using (public.can_access_venue(venue_id));

-- Authored rows: the server keeps their history.
drop trigger if exists control_bindings_history on public.control_bindings;
create trigger control_bindings_history after insert or update or delete on public.control_bindings
    for each row execute function public.record_history();

do $$
begin
    if not exists (
        select 1 from pg_publication_tables
        where pubname = 'powersync' and schemaname = 'public' and tablename = 'control_bindings'
    ) then
        alter publication powersync add table public.control_bindings;
    end if;
end
$$;

commit;

notify pgrst, 'reload schema';
