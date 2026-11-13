-- Laser projectors as what they are (docs/design/lasers.md): a box with an
-- aperture and a scanner, set up by its settings, driven through a DAC and
-- not over DMX, drawing in the one window its venue has.
--
--   * A projector's fixture is the built-in `builtin:laser-projector`, not
--     the QLC+ file Luma used to ship, and it has no DMX channels: it takes
--     no address and is not in the patch.
--   * A venue has one laser window, stored as four parameters of its root
--     node (`laser_turn_min`, `laser_turn_max`, `laser_tilt_min`,
--     `laser_tilt_max`). It was a row of `laser_zones`, and a venue could
--     hold several; a laser node named one, and one that named none drew in
--     the venue's first. That first zone becomes the window.
--   * A laser node has no settings: its `zone` goes.
--
-- A projector's settings (`laser_*` parameters of its node) are new; a
-- projector without them reads as the 3 W preset.
--
-- `laser_zones` is left as it is. Nothing reads it from here on, the sync
-- rules stop naming it, and a later migration drops it.
--
-- Graphs live in `clips.graph_json`, `perform_patterns.graph_json` and, one
-- per clip, in a draft's `base_json` and `state_json`.
--
-- Format 3. A build that reads format 2 is refused and told to update, and
-- the build that ships with this clears its copy and downloads again. The
-- marker names the last release that reads format 2, so this runs in the
-- first release after it, which is the first build that reads format 3.
--
-- contract: after 0.8.0

begin;

create or replace function public.data_format() returns integer
language sql
stable
set search_path = ''
as $$ select 3 $$;

-- History names the migration as the actor of the rows it rewrites. The
-- format is this file's: the check that turns older builds away reads it
-- from the same headers.
select set_config(
    'request.headers',
    '{"x-luma-actor": "migration:20261112150000_laser_projectors", "x-luma-format": "3"}',
    true);

-- ---------------------------------------------------------------------------
-- Projectors.
-- ---------------------------------------------------------------------------

update public.fixtures
set fixture_path = 'builtin:laser-projector',
    model = 'Laser Projector',
    num_channels = 0,
    universe = 1,
    address = 1
where fixture_path = 'Generic/Generic-ILDA-Laser-Projector.qxf';

update public.venue_nodes
set catalog_ref = 'builtin:laser-projector'
where catalog_ref = 'Generic/Generic-ILDA-Laser-Projector.qxf';

-- ---------------------------------------------------------------------------
-- The window: the venue's first zone, as a laser node with no zone read it.
-- ---------------------------------------------------------------------------

insert into public.venue_node_params (id, uid, node_id, venue_id, key, value)
select gen_random_uuid()::text, root.uid, root.id, root.venue_id, edge.key, edge.value
from (
    select distinct on (venue_id) *
    from public.laser_zones
    where deleted_at is null
    order by venue_id, lower(name), created_at, id
) zone
join public.venue_nodes root
  on root.venue_id = zone.venue_id and root.kind = 'venue' and root.deleted_at is null
cross join lateral (values
    ('laser_turn_min', zone.turn_min),
    ('laser_turn_max', zone.turn_max),
    ('laser_tilt_min', zone.tilt_min),
    ('laser_tilt_max', zone.tilt_max)
) as edge (key, value)
where not exists (
    select 1 from public.venue_node_params held
    where held.node_id = root.id and held.key = edge.key and held.deleted_at is null);

-- ---------------------------------------------------------------------------
-- Laser nodes lose their zone.
-- ---------------------------------------------------------------------------

create or replace function pg_temp.is_zoned(node jsonb) returns boolean
language sql immutable as $$
    select node ->> 'kind' = 'laser' and coalesce(node -> 'settings' ? 'zone', false)
$$;

create or replace function pg_temp.has_zone(graph jsonb) returns boolean
language sql immutable as $$
    select exists (select 1 from jsonb_each(graph -> 'nodes') where pg_temp.is_zoned(value))
$$;

-- A node with no settings is written without the key.
create or replace function pg_temp.unzoned(graph jsonb) returns jsonb
language sql immutable as $$
    select jsonb_set(graph, '{nodes}', (
        select jsonb_object_agg(key, case
            when not pg_temp.is_zoned(value) then value
            when (value -> 'settings') - 'zone' = '{}'::jsonb then value - 'settings'
            else jsonb_set(value, '{settings}', (value -> 'settings') - 'zone') end)
        from jsonb_each(graph -> 'nodes')))
$$;

-- A draft holds a score: each clip's graph.
create or replace function pg_temp.unzoned_draft(score text) returns text
language sql immutable as $$
    select jsonb_set(score::jsonb, '{clips}', jsonb_object_agg(
        key,
        case when pg_temp.has_zone(value -> 'graph')
             then jsonb_set(value, '{graph}', pg_temp.unzoned(value -> 'graph'))
             else value end))::text
    from jsonb_each(score::jsonb -> 'clips')
$$;

create or replace function pg_temp.draft_has_zone(score text) returns boolean
language sql immutable as $$
    select exists (
        select 1 from jsonb_each(score::jsonb -> 'clips')
        where pg_temp.has_zone(value -> 'graph'))
$$;

-- Only text that names a zone is parsed.
update public.clips
set graph_json = pg_temp.unzoned(graph_json::jsonb)::text
where case when graph_json like '%"zone"%' then pg_temp.has_zone(graph_json::jsonb) end;

update public.perform_patterns
set graph_json = pg_temp.unzoned(graph_json::jsonb)::text
where case when graph_json like '%"zone"%' then pg_temp.has_zone(graph_json::jsonb) end;

update public.drafts
set base_json = pg_temp.unzoned_draft(base_json)
where case when base_json like '%"zone"%' then pg_temp.draft_has_zone(base_json) end;

update public.drafts
set state_json = pg_temp.unzoned_draft(state_json)
where case when state_json like '%"zone"%' then pg_temp.draft_has_zone(state_json) end;

-- ---------------------------------------------------------------------------
-- A row as format 2 wrote it, as format 3 reads it. A `laser_zones` row has
-- no format 3 reading: it comes back as it was.
-- ---------------------------------------------------------------------------

create or replace function public.row_from_format_2(table_name text, old_row jsonb)
returns jsonb
language sql
stable
set search_path = ''
as $$
    select case
        when table_name = 'fixtures'
             and old_row ->> 'fixture_path' = 'Generic/Generic-ILDA-Laser-Projector.qxf'
        then old_row || jsonb_build_object(
            'fixture_path', 'builtin:laser-projector',
            'model', 'Laser Projector',
            'num_channels', 0,
            'universe', 1,
            'address', 1)
        when table_name = 'venue_nodes'
             and old_row ->> 'catalog_ref' = 'Generic/Generic-ILDA-Laser-Projector.qxf'
        then old_row || jsonb_build_object('catalog_ref', 'builtin:laser-projector')
        when table_name in ('clips', 'perform_patterns')
             and old_row ->> 'graph_json' like '%"zone"%'
        then old_row || jsonb_build_object('graph_json', (
            select jsonb_set(graph, '{nodes}', jsonb_object_agg(key, case
                when value ->> 'kind' <> 'laser' or not coalesce(value -> 'settings' ? 'zone', false)
                    then value
                when (value -> 'settings') - 'zone' = '{}'::jsonb then value - 'settings'
                else jsonb_set(value, '{settings}', (value -> 'settings') - 'zone') end))::text
            from (select (old_row ->> 'graph_json')::jsonb as graph) held,
                 jsonb_each(held.graph -> 'nodes')
            group by graph))
        else old_row
    end;
$$;
revoke all on function public.row_from_format_2(text, jsonb) from public, anon, authenticated;

notify pgrst, 'reload schema';

commit;
