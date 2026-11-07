-- Random takes a clock as Shuffle does (docs/specs/clip-graphs.md, "Clocks
-- of presses, one live event, and a random's clock"). Every saved graph's
-- `random` node loses its `every` and reads a new time node with that
-- every through its `time` input: `random(every=4)` becomes
-- `random(time=time(every=4))`, which plays the same. A random with no
-- every took 4 beats. The time node is the lowest free `time<n>`, one per
-- random, in id order, as the local migration names them. Node ids stay, so
-- each random keeps its draw.
--
-- Graphs live in `clips.graph_json`, `perform_patterns.graph_json` and, one
-- per clip, in a draft's `base_json` and `state_json`. History keeps what
-- was written then.
--
-- Symmetric with backend/migrations/20261107130000_random_time.sql.
--
-- Order: apply this before shipping the client that reads `random.time`; a
-- client from before it cannot read the rewritten graphs, and this client
-- cannot read a random with an `every`.

begin;

-- History names the migration as the actor of the clips it rewrites.
select set_config('request.headers', '{"x-luma-actor": "migration:20261107130000_random_time"}', true);

create function pg_temp.random_clocks(graph jsonb) returns jsonb
language plpgsql immutable as $$
declare
    random record;
    nodes jsonb := graph -> 'nodes';
    n int;
    clock text;
begin
    for random in
        select key, value from jsonb_each(graph -> 'nodes')
        where value ->> 'kind' = 'random'
        order by key collate "C"
    loop
        n := 1;
        while nodes ? ('time' || n) loop
            n := n + 1;
        end loop;
        clock := 'time' || n;
        nodes := nodes
            || jsonb_build_object(clock, jsonb_build_object(
                   'kind', 'time',
                   'inputs', jsonb_build_object(
                       'every', coalesce(random.value -> 'inputs' -> 'every', '4.0'::jsonb))))
            || jsonb_build_object(random.key, jsonb_set(
                   random.value, '{inputs}',
                   (coalesce(random.value -> 'inputs', '{}') - 'every')
                   || jsonb_build_object('time', jsonb_build_object('node', clock))));
    end loop;
    return jsonb_set(graph, '{nodes}', nodes);
end
$$;

create function pg_temp.has_random(graph jsonb) returns boolean
language sql immutable as $$
    select exists (select 1 from jsonb_each(graph -> 'nodes') where value ->> 'kind' = 'random')
$$;

-- A draft holds a score: each clip's graph.
create function pg_temp.random_draft(score text) returns text
language sql immutable as $$
    select jsonb_set(score::jsonb, '{clips}', jsonb_object_agg(
        key,
        case when pg_temp.has_random(value -> 'graph')
             then jsonb_set(value, '{graph}', pg_temp.random_clocks(value -> 'graph'))
             else value end))::text
    from jsonb_each(score::jsonb -> 'clips')
$$;

create function pg_temp.draft_has_random(score text) returns boolean
language sql immutable as $$
    select exists (
        select 1 from jsonb_each(score::jsonb -> 'clips')
        where pg_temp.has_random(value -> 'graph'))
$$;

-- Only text that names `random` is parsed.
update public.clips
set graph_json = pg_temp.random_clocks(graph_json::jsonb)::text
where case when graph_json like '%"random"%' then pg_temp.has_random(graph_json::jsonb) end;

update public.perform_patterns
set graph_json = pg_temp.random_clocks(graph_json::jsonb)::text
where case when graph_json like '%"random"%' then pg_temp.has_random(graph_json::jsonb) end;

update public.drafts
set base_json = pg_temp.random_draft(base_json)
where case when base_json like '%"random"%' then pg_temp.draft_has_random(base_json) end;

update public.drafts
set state_json = pg_temp.random_draft(state_json)
where case when state_json like '%"random"%' then pg_temp.draft_has_random(state_json) end;

commit;
