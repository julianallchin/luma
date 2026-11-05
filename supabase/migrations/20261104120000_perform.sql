-- The Perform set: the patterns a venue plays live, and how its Perform tab
-- lays out the pads and faders that play them.
--
-- `perform_patterns` holds the same pattern a score holds (a graph, a target,
-- a blend mode, a seed) without a place on a timeline: each one plays from
-- the moment it is turned on, at its own level. `z_index` is its place in the
-- set: where two patterns write the same attribute, the higher one wins.
-- `pad_json` is how its pad plays it: toggle or flash, and its envelope.
--
-- `perform_layouts` is one row a venue, its `id` the venue's: the Perform
-- tab's named pages of cells, as JSON.
--
-- Venue content: the owner and every member may read and write, the same
-- policy `folders` and the laser tables have.
--
-- Symmetric with backend/migrations/20261104120000_perform.sql.
--
-- Order:
--   1. Apply this file.
--   2. Redeploy `deploy/powersync/sync-config.yaml`, which now names both tables.
--
-- Idempotent.

begin;

create table if not exists public.perform_patterns (
    id text primary key,
    uid uuid not null default auth.uid() references auth.users (id) on delete cascade,
    venue_id text not null references public.venues (id) on delete cascade deferrable initially deferred,
    name text not null default '',
    selection_json text not null,
    z_index integer not null default 0,
    blend_mode text not null,
    seed text not null,
    graph_json text not null,
    pad_json text not null default '{}',
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create index if not exists perform_patterns_venue_idx on public.perform_patterns (venue_id);

create table if not exists public.perform_layouts (
    id text primary key,
    uid uuid not null default auth.uid() references auth.users (id) on delete cascade,
    venue_id text not null references public.venues (id) on delete cascade deferrable initially deferred,
    layout_json text not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create index if not exists perform_layouts_venue_idx on public.perform_layouts (venue_id);

alter table public.perform_patterns enable row level security;
revoke all on public.perform_patterns from anon, public;
grant select, insert, update, delete on public.perform_patterns to authenticated;
grant select on public.perform_patterns to powersync_role;

drop policy if exists perform_patterns_select on public.perform_patterns;
drop policy if exists perform_patterns_insert on public.perform_patterns;
drop policy if exists perform_patterns_update on public.perform_patterns;
drop policy if exists perform_patterns_delete on public.perform_patterns;
create policy perform_patterns_select on public.perform_patterns for select to authenticated
    using (public.can_access_venue(venue_id));
create policy perform_patterns_insert on public.perform_patterns for insert to authenticated
    with check (public.can_access_venue(venue_id));
create policy perform_patterns_update on public.perform_patterns for update to authenticated
    using (public.can_access_venue(venue_id))
    with check (public.can_access_venue(venue_id));
create policy perform_patterns_delete on public.perform_patterns for delete to authenticated
    using (public.can_access_venue(venue_id));

-- Authored rows: the server keeps their history.
drop trigger if exists perform_patterns_history on public.perform_patterns;
create trigger perform_patterns_history after insert or update or delete on public.perform_patterns
    for each row execute function public.record_history();

do $$
begin
    if not exists (
        select 1 from pg_publication_tables
        where pubname = 'powersync' and schemaname = 'public' and tablename = 'perform_patterns'
    ) then
        alter publication powersync add table public.perform_patterns;
    end if;
end
$$;

alter table public.perform_layouts enable row level security;
revoke all on public.perform_layouts from anon, public;
grant select, insert, update, delete on public.perform_layouts to authenticated;
grant select on public.perform_layouts to powersync_role;

drop policy if exists perform_layouts_select on public.perform_layouts;
drop policy if exists perform_layouts_insert on public.perform_layouts;
drop policy if exists perform_layouts_update on public.perform_layouts;
drop policy if exists perform_layouts_delete on public.perform_layouts;
create policy perform_layouts_select on public.perform_layouts for select to authenticated
    using (public.can_access_venue(venue_id));
create policy perform_layouts_insert on public.perform_layouts for insert to authenticated
    with check (public.can_access_venue(venue_id));
create policy perform_layouts_update on public.perform_layouts for update to authenticated
    using (public.can_access_venue(venue_id))
    with check (public.can_access_venue(venue_id));
create policy perform_layouts_delete on public.perform_layouts for delete to authenticated
    using (public.can_access_venue(venue_id));

-- Authored rows: the server keeps their history.
drop trigger if exists perform_layouts_history on public.perform_layouts;
create trigger perform_layouts_history after insert or update or delete on public.perform_layouts
    for each row execute function public.record_history();

do $$
begin
    if not exists (
        select 1 from pg_publication_tables
        where pubname = 'powersync' and schemaname = 'public' and tablename = 'perform_layouts'
    ) then
        alter publication powersync add table public.perform_layouts;
    end if;
end
$$;

commit;

notify pgrst, 'reload schema';
