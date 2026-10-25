-- Random replaces Noise (docs/design/lasers.md, v2). Every saved graph's
-- `noise` node becomes a `random` node that plays the same: `period` is
-- `every`, `glide` 1 drifts the whole turn as Noise did, and `scale` and
-- `contrast` keep their meaning. Node ids stay, so each node keeps its draw.
--
-- Graphs live in `clips.graph_json` and, one per clip, in a draft's
-- `base_json` and `state_json`. History keeps what was written then.
--
-- Symmetric with backend/migrations/20261025120000_random_node.sql.
--
-- Order: apply this before shipping the client that reads `random`; a
-- client from before it cannot read the rewritten graphs.

begin;

-- History names the migration as the actor of the clips it rewrites.
select set_config('request.headers', '{"x-luma-actor": "migration:20261025120000_random_node"}', true);

create function pg_temp.random_nodes(graph jsonb) returns jsonb
language sql immutable as $$
    select jsonb_set(graph, '{nodes}', jsonb_object_agg(
        key,
        case when value ->> 'kind' = 'noise' then
            jsonb_set(value, '{kind}', '"random"')
            || jsonb_build_object('inputs',
                   coalesce(value -> 'inputs', '{}') - 'period'
                   || jsonb_build_object('glide', 1.0)
                   || case when value -> 'inputs' ? 'period'
                           then jsonb_build_object('every', value -> 'inputs' -> 'period')
                           else '{}' end)
        else value end))
    from jsonb_each(graph -> 'nodes')
$$;

create function pg_temp.has_noise(graph jsonb) returns boolean
language sql immutable as $$
    select exists (select 1 from jsonb_each(graph -> 'nodes') where value ->> 'kind' = 'noise')
$$;

-- A draft holds a score: each clip's graph.
create function pg_temp.random_draft(score text) returns text
language sql immutable as $$
    select jsonb_set(score::jsonb, '{clips}', jsonb_object_agg(
        key,
        case when pg_temp.has_noise(value -> 'graph')
             then jsonb_set(value, '{graph}', pg_temp.random_nodes(value -> 'graph'))
             else value end))::text
    from jsonb_each(score::jsonb -> 'clips')
$$;

create function pg_temp.draft_has_noise(score text) returns boolean
language sql immutable as $$
    select exists (
        select 1 from jsonb_each(score::jsonb -> 'clips')
        where pg_temp.has_noise(value -> 'graph'))
$$;

-- Only text that names `noise` is parsed.
update public.clips
set graph_json = pg_temp.random_nodes(graph_json::jsonb)::text
where case when graph_json like '%"noise"%' then pg_temp.has_noise(graph_json::jsonb) end;

update public.drafts
set base_json = pg_temp.random_draft(base_json)
where case when base_json like '%"noise"%' then pg_temp.draft_has_noise(base_json) end;

update public.drafts
set state_json = pg_temp.random_draft(state_json)
where case when state_json like '%"noise"%' then pg_temp.draft_has_noise(state_json) end;

commit;
