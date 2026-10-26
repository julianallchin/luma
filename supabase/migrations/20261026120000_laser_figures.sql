-- Laser figures (docs/design/lasers.md, "Figures"): a laser node draws the
-- figures wired into its `figures`, so one clip mixes beams, lines and
-- rings. Every saved laser node splits in two: a `figure` node takes its
-- `kind` setting and its heads, count, sides, x, y, size, rotation and
-- color, and the laser keeps its zone and alpha and draws that one figure.
-- The figure is `figure1`, or `<laser id>_figure` where `figure1` is taken.
--
-- Graphs live in `clips.graph_json` and, one per clip, in a draft's
-- `base_json` and `state_json`. History keeps what was written then.
--
-- Symmetric with backend/migrations/20261026120000_laser_figures.sql.
--
-- Order: apply this before shipping the client that reads `figure`; a
-- client from before it cannot read the rewritten graphs.
--
-- Idempotent: a laser node that already has `figures` is left alone.

begin;

create schema if not exists backup;
create table if not exists backup.clips_pre_laser_figures_20261026 as
    select * from public.clips where graph_json like '%"laser"%';
create table if not exists backup.drafts_pre_laser_figures_20261026 as
    select * from public.drafts where base_json like '%"laser"%' or state_json like '%"laser"%';

-- History names the migration as the actor of the clips it rewrites.
select set_config('request.headers', '{"x-luma-actor": "migration:20261026120000_laser_figures"}', true);

create function pg_temp.is_old_laser(node jsonb) returns boolean
language sql immutable as $$
    select node ->> 'kind' = 'laser' and not coalesce(node -> 'inputs' ? 'figures', false)
$$;

-- The id a laser node's figure takes in `nodes`.
create function pg_temp.figure_id(nodes jsonb, laser text) returns text
language sql immutable as $$
    select case when nodes ? 'figure1' then laser || '_figure' else 'figure1' end
$$;

create function pg_temp.laser_figures(graph jsonb) returns jsonb
language sql immutable as $$
    select jsonb_set(graph, '{nodes}', (
        select jsonb_object_agg(id, node) from (
            select key as id,
                   case when pg_temp.is_old_laser(value) then jsonb_build_object(
                       'kind', 'laser',
                       'settings', jsonb_build_object(
                           'zone', coalesce(value -> 'settings' ->> 'zone', '')),
                       'inputs', jsonb_build_object('figures', jsonb_build_array(
                               jsonb_build_object('node', pg_temp.figure_id(graph -> 'nodes', key))))
                           || case when value -> 'inputs' ? 'alpha'
                                   then jsonb_build_object('alpha', value -> 'inputs' -> 'alpha')
                                   else '{}' end)
                   else value end as node
            from jsonb_each(graph -> 'nodes')
            union all
            select pg_temp.figure_id(graph -> 'nodes', key),
                   jsonb_build_object(
                       'kind', 'figure',
                       'settings', jsonb_build_object(
                           'kind', coalesce(value -> 'settings' ->> 'kind', 'beams')),
                       'inputs', coalesce(value -> 'inputs', '{}') - 'alpha')
            from jsonb_each(graph -> 'nodes')
            where pg_temp.is_old_laser(value)
        ) nodes))
$$;

create function pg_temp.has_old_laser(graph jsonb) returns boolean
language sql immutable as $$
    select exists (select 1 from jsonb_each(graph -> 'nodes') where pg_temp.is_old_laser(value))
$$;

-- A draft holds a score: each clip's graph.
create function pg_temp.figures_draft(score text) returns text
language sql immutable as $$
    select jsonb_set(score::jsonb, '{clips}', jsonb_object_agg(
        key,
        case when pg_temp.has_old_laser(value -> 'graph')
             then jsonb_set(value, '{graph}', pg_temp.laser_figures(value -> 'graph'))
             else value end))::text
    from jsonb_each(score::jsonb -> 'clips')
$$;

create function pg_temp.draft_has_old_laser(score text) returns boolean
language sql immutable as $$
    select exists (
        select 1 from jsonb_each(score::jsonb -> 'clips')
        where pg_temp.has_old_laser(value -> 'graph'))
$$;

-- Only text that names `laser` is parsed.
update public.clips
set graph_json = pg_temp.laser_figures(graph_json::jsonb)::text
where case when graph_json like '%"laser"%' then pg_temp.has_old_laser(graph_json::jsonb) end;

update public.drafts
set base_json = pg_temp.figures_draft(base_json)
where case when base_json like '%"laser"%' then pg_temp.draft_has_old_laser(base_json) end;

update public.drafts
set state_json = pg_temp.figures_draft(state_json)
where case when state_json like '%"laser"%' then pg_temp.draft_has_old_laser(state_json) end;

commit;
