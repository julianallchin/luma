-- Laser v2 (docs/design/lasers.md, "v2"): a laser node draws `count`
-- elements of a `kind` (beams, line, ring, polygon, grid), each a head with
-- its own x, y, size, rotation and color. The old node's `shape`, `beams`
-- and `join` go, and Color clips no longer reach projectors. Every saved
-- graph moves here, once; nothing converts at runtime.
--
-- Symmetric with backend/migrations/20261025130000_laser_elements.sql.
--
-- A laser node:
--   * fan, join (a number) 0.5 or more  -> kind line, one per projector
--   * circle, join 0.5 or more           -> kind ring, one per projector
--   * grid                               -> kind grid, count = beams
--   * otherwise                          -> kind beams, count = beams
-- `beams` and `join` go; x, y, size, rotation, alpha, heads and zone stay.
-- A fan whose x is set places each beam itself: x becomes x + size times
-- its place across the fan, a curve over `space(split(), kind="order")`
-- (math nodes when x or size is wired). A circle of beams does the same
-- for x and y round the circle. An empty x keeps the fan as the kind's
-- home layout, which is the old fan.
--
-- A Color clip with the same score, start, duration and selection as
-- exactly one laser clip (and the other way round) is folded into it: a
-- flat colour times its brightness and alpha becomes the laser's color;
-- anything wired is copied in, its nodes renamed `c_<id>`, and color is
-- their product. A folded Color clip whose selection is one group of
-- only projectors is deleted.
--
-- Lost: a wired join (it keeps the beams; the nodes only it read go), the rotation of a fan whose x
-- is set or of a circle of beams, a grid's layout where x or y is set, the
-- half-step turn of a circle of beams, and Color clips over projectors
-- that pair with no laser clip one to one (they stay, and light nothing on
-- the projectors). Drafts holding a laser node are deleted.
--
-- Order:
--   1. Apply this file after the build that reads the new node ships.
--
-- Not idempotent: a laser node already in the new form (settings.kind) is
-- left alone, but a folded Color clip is gone.

begin;

create schema if not exists backup;
create table backup.clips_pre_laser_elements_20261025 as
    select * from public.clips
    where score_id in (select score_id from public.clips where graph_json like '%"laser"%');
create table backup.drafts_pre_laser_elements_20261025 as
    select * from public.drafts where base_json like '%"laser"%' or state_json like '%"laser"%';

-- Every `{"node": id}` in `j` becomes `{"node": prefix || id}`.
create function pg_temp.rename_wires(j jsonb, prefix text) returns jsonb
language plpgsql immutable as $$
begin
    if jsonb_typeof(j) = 'object' then
        if (select count(*) from jsonb_object_keys(j)) = 1 and jsonb_typeof(j -> 'node') = 'string' then
            return jsonb_build_object('node', prefix || (j ->> 'node'));
        end if;
        return (select coalesce(jsonb_object_agg(k, pg_temp.rename_wires(v, prefix)), '{}')
                from jsonb_each(j) e(k, v));
    elsif jsonb_typeof(j) = 'array' then
        return (select coalesce(jsonb_agg(pg_temp.rename_wires(v, prefix) order by i), '[]')
                from jsonb_array_elements(j) with ordinality a(v, i));
    end if;
    return j;
end
$$;

-- `nodes` with the nodes that place each element on `axis` at `centre`
-- plus `size` times `reach` times `points` over the element's order on its
-- projector, -1 to 1: {"nodes": ..., "input": the wire the laser's axis
-- reads}. Order puts element i of n at (i + 0.5) / n, so a fan's curve runs
-- ±size·n/(n − 1) to put its end beams at ±size.
create function pg_temp.spread(nodes jsonb, points jsonb, reach numeric, centre jsonb, size jsonb, axis text)
returns jsonb language plpgsql immutable as $$
declare
    name text := 'm_' || axis;
    curve jsonb;
begin
    nodes := nodes || jsonb_build_object(
        'm_split', jsonb_build_object('kind', 'split', 'settings', jsonb_build_object('by', 'fixture')),
        'm_order', jsonb_build_object(
            'kind', 'space',
            'settings', jsonb_build_object('kind', 'order', 'wrap', 'no'),
            'inputs', jsonb_build_object('heads', jsonb_build_object('node', 'm_split'))));
    curve := jsonb_build_object(
        'kind', 'curve',
        'settings', jsonb_build_object('kind', 'number'),
        'inputs', jsonb_build_object(
            'x', jsonb_build_object('node', 'm_order'),
            'shape', jsonb_build_object('points', points)));
    if jsonb_typeof(centre) = 'number' and jsonb_typeof(size) = 'number' then
        nodes := nodes || jsonb_build_object(name, jsonb_set(jsonb_set(curve,
            '{inputs,low}', to_jsonb(centre::text::numeric - size::text::numeric * reach)),
            '{inputs,high}', to_jsonb(centre::text::numeric + size::text::numeric * reach)));
    else
        nodes := nodes || jsonb_build_object(
            name || '_u', jsonb_set(jsonb_set(curve, '{inputs,low}', to_jsonb(-reach)), '{inputs,high}', to_jsonb(reach)),
            name || '_w', jsonb_build_object(
                'kind', 'math', 'settings', jsonb_build_object('op', '*'),
                'inputs', jsonb_build_object('values',
                    jsonb_build_array(jsonb_build_object('node', name || '_u'), size))),
            name, jsonb_build_object(
                'kind', 'math', 'settings', jsonb_build_object('op', '+'),
                'inputs', jsonb_build_object('values',
                    jsonb_build_array(jsonb_build_object('node', name || '_w'), centre))));
    end if;
    return jsonb_build_object('nodes', nodes, 'input', jsonb_build_object('node', name));
end
$$;

-- `graph` with its old laser node in the new form.
create function pg_temp.laser_elements(graph jsonb) returns jsonb
language plpgsql immutable as $$
declare
    nodes jsonb := graph -> 'nodes';
    lid text;
    node jsonb;
    inputs jsonb;
    shape text;
    n integer;
    kind text;
    placed jsonb;
    reached text[];
    sine constant jsonb := '[[0, 0.5, "sine-out"], [0.25, 1, "sine-in"], [0.5, 0.5, "sine-out"], [0.75, 0, "sine-in"], [1, 0.5]]';
    cosine constant jsonb := '[[0, 1, "sine-in"], [0.25, 0.5, "sine-out"], [0.5, 0, "sine-in"], [0.75, 0.5, "sine-out"], [1, 1]]';
begin
    select key, value into lid, node from jsonb_each(nodes) where value ->> 'kind' = 'laser' limit 1;
    if lid is null or (node -> 'settings') ? 'kind' then
        return graph;
    end if;
    inputs := coalesce(node -> 'inputs', '{}');
    shape := coalesce(node -> 'settings' ->> 'shape', 'fan');
    n := case when jsonb_typeof(inputs -> 'beams') = 'number' then (inputs ->> 'beams')::numeric::integer else 8 end;
    kind := case
        when shape = 'grid' then 'grid'
        when jsonb_typeof(inputs -> 'join') = 'number' and (inputs ->> 'join')::numeric >= 0.5 then
            case when shape = 'circle' then 'ring' else 'line' end
        else 'beams'
    end;
    inputs := inputs - 'beams' - 'join';
    if kind in ('beams', 'grid') then
        inputs := inputs || jsonb_build_object('count', n);
    end if;
    if kind = 'beams' and shape = 'fan' and inputs ? 'x' and n > 1 then
        placed := pg_temp.spread(nodes, '[[0, 0], [1, 1]]', n::numeric / (n - 1), inputs -> 'x', coalesce(inputs -> 'size', '0.5'), 'x');
        nodes := placed -> 'nodes';
        inputs := inputs || jsonb_build_object('x', placed -> 'input');
    elsif kind = 'beams' and shape = 'circle' then
        placed := pg_temp.spread(nodes, sine, 1, coalesce(inputs -> 'x', '0'), coalesce(inputs -> 'size', '0.5'), 'x');
        nodes := placed -> 'nodes';
        inputs := inputs || jsonb_build_object('x', placed -> 'input');
        placed := pg_temp.spread(nodes, cosine, 1, coalesce(inputs -> 'y', '0'), coalesce(inputs -> 'size', '0.5'), 'y');
        nodes := placed -> 'nodes';
        inputs := inputs || jsonb_build_object('y', placed -> 'input');
    end if;
    nodes := nodes || jsonb_build_object(lid, jsonb_build_object(
        'kind', 'laser',
        'settings', jsonb_build_object('kind', kind, 'zone', coalesce(node -> 'settings' ->> 'zone', '')),
        'inputs', inputs));
    -- A wired join's nodes, read by nothing now.
    if jsonb_typeof(node -> 'inputs' -> 'join') = 'object' then
        with recursive reach (key) as (
            select lid
            union
            select w #>> '{}'
            from reach r, jsonb_path_query(nodes -> r.key, '$.**.node') w
            where jsonb_typeof(w) = 'string'
        )
        select array_agg(key) into reached from reach;
        nodes := (select jsonb_object_agg(k, v) from jsonb_each(nodes) e(k, v) where k = any(reached));
    end if;
    return jsonb_set(graph, '{nodes}', nodes);
end
$$;

-- `laser` with the colour of Color graph `color` as its own.
create function pg_temp.fold_color(laser jsonb, color jsonb) returns jsonb
language plpgsql immutable as $$
declare
    lid text := (select key from jsonb_each(laser -> 'nodes') where value ->> 'kind' = 'laser' limit 1);
    cid text := (select key from jsonb_each(color -> 'nodes') where value ->> 'kind' = 'color' limit 1);
    renamed jsonb;
    inputs jsonb;
    shade jsonb;
    items jsonb;
    f numeric;
begin
    inputs := pg_temp.rename_wires(coalesce(color -> 'nodes' -> cid -> 'inputs', '{}'), 'c_');
    if jsonb_typeof(coalesce(inputs -> 'color', '[1, 1, 1]')) = 'array'
        and coalesce(jsonb_typeof(inputs -> 'brightness'), 'number') = 'number'
        and coalesce(jsonb_typeof(inputs -> 'alpha'), 'number') = 'number' then
        f := coalesce((inputs ->> 'brightness')::numeric, 1) * coalesce((inputs ->> 'alpha')::numeric, 1);
        shade := (select jsonb_agg(least(1, greatest(0, c::text::numeric * f)) order by i)
                  from jsonb_array_elements(coalesce(inputs -> 'color', '[1, 1, 1]')) with ordinality a(c, i));
        return jsonb_set(laser, array['nodes', lid, 'inputs', 'color'], shade);
    end if;
    renamed := (select coalesce(jsonb_object_agg('c_' || k, pg_temp.rename_wires(v, 'c_')), '{}')
                from jsonb_each(color -> 'nodes') e(k, v) where k <> cid);
    if jsonb_typeof(coalesce(inputs -> 'color', '[1, 1, 1]')) = 'array' then
        renamed := renamed || jsonb_build_object(
            'c_once', jsonb_build_object('kind', 'time'),
            'c_flat', jsonb_build_object(
                'kind', 'curve',
                'settings', jsonb_build_object('kind', 'color'),
                'inputs', jsonb_build_object(
                    'x', jsonb_build_object('node', 'c_once'),
                    'gradient', jsonb_build_object('stops', jsonb_build_array(jsonb_build_object(
                        't', 0, 'color', coalesce(inputs -> 'color', '[1, 1, 1]')))))));
        items := jsonb_build_array(jsonb_build_object('node', 'c_flat'));
    else
        items := jsonb_build_array(inputs -> 'color');
    end if;
    if inputs ? 'brightness' then
        items := items || jsonb_build_array(inputs -> 'brightness');
    end if;
    if inputs ? 'alpha' then
        items := items || jsonb_build_array(inputs -> 'alpha');
    end if;
    if jsonb_array_length(items) > 1 then
        renamed := renamed || jsonb_build_object('c_mix', jsonb_build_object(
            'kind', 'math', 'settings', jsonb_build_object('op', '*'),
            'inputs', jsonb_build_object('values', items)));
        items := jsonb_build_array(jsonb_build_object('node', 'c_mix'));
    end if;
    laser := jsonb_set(laser, '{nodes}', (laser -> 'nodes') || renamed);
    return jsonb_set(laser, array['nodes', lid, 'inputs', 'color'], items -> 0);
end
$$;

update public.clips
set graph_json = pg_temp.laser_elements(graph_json::jsonb)::text
where graph_json like '%"laser"%';

-- Laser and Color clips that pair one to one.
create temporary table laser_color_pairs on commit drop as
with lasers as (
    select * from public.clips
    where graph_json::jsonb -> 'nodes' @? '$.* ? (@.kind == "laser")'
), colors as (
    select * from public.clips
    where graph_json::jsonb -> 'nodes' @? '$.* ? (@.kind == "color")'
), pairs as (
    select l.id as laser_id, c.id as color_id
    from lasers l
    join colors c on c.score_id = l.score_id
        and abs(c.start - l.start) < 0.01
        and abs(c.duration - l.duration) < 0.01
        and c.selection_json = l.selection_json
)
select * from pairs p
where (select count(*) from pairs q where q.laser_id = p.laser_id) = 1
  and (select count(*) from pairs q where q.color_id = p.color_id) = 1;

update public.clips l
set graph_json = pg_temp.fold_color(l.graph_json::jsonb, c.graph_json::jsonb)::text
from laser_color_pairs p
join public.clips c on c.id = p.color_id
where l.id = p.laser_id;

-- A folded Color clip over one group of only projectors lit nothing else.
delete from public.clips c
using laser_color_pairs p, public.scores s, public.fixture_groups g
where c.id = p.color_id
  and s.id = c.score_id
  and g.venue_id = s.venue_id
  and g.name = c.selection_json::jsonb ->> 'expression'
  and exists (select 1 from public.fixture_group_members m where m.group_id = g.id)
  and not exists (
      select 1 from public.fixture_group_members m
      join public.fixtures f on f.id = m.fixture_id
      where m.group_id = g.id and f.fixture_path <> 'Generic/Generic-ILDA-Laser-Projector.qxf');

delete from public.drafts where base_json like '%"laser"%' or state_json like '%"laser"%';

commit;
