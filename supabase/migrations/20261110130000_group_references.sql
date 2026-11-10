-- A selection names its groups by id.
--
-- A clip's selection, a Perform pattern's and a Perform group cell held a
-- group's *name* — `{"expression": "front_wash | spots"}` — so renaming a
-- group, or deleting one, broke every score that used it. They now hold the
-- group's id:
--
--   {"all": true}                      the whole venue
--   {"groups": ["<id>", "<id>"]}       the union of these groups
--
-- A selection was an expression over names, with `&`, `~`, `^`, `>` and
-- parentheses as well as `|`. A union is all the new form says, so anything
-- else an expression said becomes a group of its own, holding the heads the
-- expression matched when this ran, and the selection names that group:
--
--   tower_inner & tower_low     the group  tower_inner_and_tower_low
--   washes | (bars & ~tower)    washes, and the group  bars_and_not_tower
--
-- A name the venue has no group for becomes an empty group of that name, as
-- "Recreate" in the old repair dialog made one, so the score still says what
-- it meant and the group can be filled. `a ^ b` took one side at random and
-- `a > b` the first that lit anything; both become the first side that lights
-- anything now.
--
-- Selections live in `clips.selection_json`, `perform_patterns.selection_json`,
-- each clip of a draft's `base_json` and `state_json`, and, as a bare name, a
-- group cell of `perform_layouts.layout_json`. A draft's clips also lose
-- `selection_seed`, which chose `^`'s side. History keeps what was written
-- then.
--
-- Head counts: an expression is evaluated head by head, and the server does
-- not know how many heads a fixture has. `pg_temp.selection_definitions`
-- lists the count for every definition that had a per-head membership when
-- this was written; any other fixture is taken to have as many heads as its
-- highest per-head membership says, which is exact unless a group holds a
-- whole fixture, another holds only its lower heads, and an expression
-- subtracts the second from the first.
--
-- Server only: the local database is a projection of these rows and syncs
-- the change down.
--
-- Order: apply this before shipping the client that reads `{"groups": …}`; a
-- client from before it cannot read the rewritten selections, and this
-- client cannot read an expression.

begin;

-- History names the migration as the actor of the rows it rewrites.
select set_config('request.headers', '{"x-luma-actor": "migration:20261110130000_group_references"}', true);

create schema if not exists backup;
create table backup.clips_pre_group_references_20261110 as
    select id, selection_json from public.clips;
create table backup.perform_patterns_pre_group_references_20261110 as
    select id, selection_json from public.perform_patterns;
create table backup.perform_layouts_pre_group_references_20261110 as
    select id, layout_json from public.perform_layouts;
create table backup.drafts_pre_group_references_20261110 as
    select id, base_json, state_json from public.drafts;

-- How many heads a definition's mode has (see the header).
create temporary table selection_definitions (
    fixture_path text not null,
    mode_name text not null,
    heads int not null,
    primary key (fixture_path, mode_name)
) on commit drop;

insert into selection_definitions values
    ('lib:oppsk/960led-stage-strobe-light', '39ch', 16),
    ('lib:oppsk/hyt-006-384sl', '64ch', 48),
    ('lib:venue/tetra-bar', '24ch', 4);

-- Every head of every fixture, as `<fixture id>:<head>`: what an expression
-- is evaluated over, as the client evaluated it.
create temporary table selection_heads on commit drop as
select fixture.venue_id, fixture.id as fixture_id, head, fixture.id || ':' || head as key
from public.fixtures fixture
left join selection_definitions known
       on known.fixture_path = fixture.fixture_path and known.mode_name = fixture.mode_name
cross join lateral generate_series(0, coalesce(
    known.heads,
    greatest(1, (select max(member.head_index) + 1 from public.fixture_group_members member
                  where member.fixture_id = fixture.id))) - 1) as head;

create index on selection_heads (venue_id);
create index on selection_heads (fixture_id);

-- The heads of the group called `called` in `venue`. A row for the whole
-- fixture is every head of it; a row for a head past the fixture's last is
-- ignored.
create or replace function pg_temp.selection_group(venue text, called text) returns text[]
language sql stable as $$
    select coalesce(array_agg(distinct head.key), '{}')
    from public.fixture_groups grp
    join public.fixture_group_members member on member.group_id = grp.id
    join selection_heads head
      on head.fixture_id = member.fixture_id
     and (member.head_index < 0 or member.head_index = head.head)
    where grp.venue_id = venue and grp.name = called
$$;

create or replace function pg_temp.selection_all(venue text) returns text[]
language sql stable as $$
    select coalesce(array_agg(key), '{}') from selection_heads where venue_id = venue
$$;

-- An expression's tokens: names, lowercased as the client's lexer did, and
-- the signs. Anything else in it is an error, as it was to the client.
create or replace function pg_temp.selection_tokens(expression text) returns text[]
language plpgsql immutable as $$
declare
    tokens text[];
begin
    if expression ~ '[^A-Za-z0-9_|&^~>()[:space:]]' then
        raise exception 'unexpected character in selection %', expression;
    end if;
    select coalesce(array_agg(lower(match[1])), '{}') into tokens
    from regexp_matches(expression, '[A-Za-z0-9_]+|[|&^~>()]', 'g') as match;
    return tokens;
end
$$;

-- The heads `tokens` match in `venue`: the client's grammar, tightest first
-- `~`, `&`, `^`, `|`, `>`, evaluated on two stacks.
create or replace function pg_temp.selection_eval(venue text, tokens text[]) returns text[]
language plpgsql stable as $$
declare
    token text;
    -- A set of heads is a text[]; a stack of them is jsonb, since an array
    -- of arrays must be rectangular.
    sets jsonb := '[]';
    signs text[] := '{}';
    -- Whether the next token must start an operand.
    operand boolean := true;

    rank constant jsonb := '{">": 1, "|": 2, "^": 3, "&": 4, "~": 5}';
    top text;
begin
    foreach token in array tokens || array[')'] loop
        if token ~ '^[a-z0-9_]+$' then
            if not operand then
                raise exception 'two names in a row';
            end if;
            sets := sets || jsonb_build_array(to_jsonb(case
                when token = 'all' then pg_temp.selection_all(venue)
                else pg_temp.selection_group(venue, token) end));
            operand := false;
        elsif token = '(' or token = '~' then
            if not operand then
                raise exception 'a name is missing its operator';
            end if;
            signs := signs || token;
        else
            if operand then
                raise exception 'an operator is missing its name';
            end if;
            -- Apply what binds at least as tightly as this sign; a closing
            -- parenthesis applies everything back to its opening one.
            loop
                top := signs[cardinality(signs)];
                exit when top is null or top = '('
                    or (token <> ')' and (rank ->> top)::int < (rank ->> token)::int);
                signs := signs[1:cardinality(signs) - 1];
                sets := pg_temp.selection_apply(venue, top, sets);
            end loop;
            if token = ')' then
                -- The closing one appended to the tokens meets no opening.
                if top = '(' then
                    signs := signs[1:cardinality(signs) - 1];
                end if;
            else
                signs := signs || token;
                operand := true;
            end if;
        end if;
    end loop;
    if cardinality(signs) > 0 or jsonb_array_length(sets) <> 1 then
        raise exception 'unbalanced selection';
    end if;
    return array(select jsonb_array_elements_text(sets -> 0));
end
$$;

-- `sets` with `sign` applied to its top one (`~`) or two.
create or replace function pg_temp.selection_apply(venue text, sign text, sets jsonb) returns jsonb
language plpgsql stable as $$
declare
    depth int := jsonb_array_length(sets);
    b text[] := array(select jsonb_array_elements_text(sets -> (depth - 1)));
    a text[];
    result text[];
begin
    if sign = '~' then
        result := array(select unnest(pg_temp.selection_all(venue)) except select unnest(b));
        return (sets - (depth - 1)) || jsonb_build_array(to_jsonb(result));
    end if;
    if depth < 2 then
        raise exception 'an operator is missing its name';
    end if;
    a := array(select jsonb_array_elements_text(sets -> (depth - 2)));
    result := case sign
        when '&' then array(select unnest(a) intersect select unnest(b))
        when '|' then array(select unnest(a) union select unnest(b))
        -- `^` and `>`: the first side that lights anything.
        else case when cardinality(a) > 0 then a else b end
    end;
    return (sets - (depth - 1) - (depth - 2)) || jsonb_build_array(to_jsonb(result));
end
$$;

-- `tokens` without the parentheses that wrap all of it.
create or replace function pg_temp.selection_unwrapped(tokens text[]) returns text[]
language plpgsql immutable as $$
declare
    depth int;
    closes int;
begin
    while cardinality(tokens) >= 2 and tokens[1] = '(' loop
        -- The first parenthesis wraps everything only if it closes at the end.
        depth := 0;
        closes := null;
        for at in 1 .. cardinality(tokens) loop
            depth := depth + case tokens[at] when '(' then 1 when ')' then -1 else 0 end;
            if depth = 0 then
                closes := at;
                exit;
            end if;
        end loop;
        exit when closes is distinct from cardinality(tokens);
        tokens := tokens[2:cardinality(tokens) - 1];
    end loop;
    return tokens;
end
$$;

-- The terms `tokens` is a union of, each as its tokens joined by spaces. An
-- expression that is not a union at its top is its own one term.
create or replace function pg_temp.selection_terms(tokens text[]) returns text[]
language plpgsql immutable as $$
declare
    depth int := 0;
    start int := 1;
    terms text[] := '{}';
begin
    tokens := pg_temp.selection_unwrapped(tokens);
    -- `>` binds loosest, so one outside every parenthesis is the whole thing.
    for at in 1 .. cardinality(tokens) loop
        depth := depth + case tokens[at] when '(' then 1 when ')' then -1 else 0 end;
        if depth = 0 and tokens[at] = '>' then
            return array[array_to_string(tokens, ' ')];
        end if;
    end loop;
    for at in 1 .. cardinality(tokens) + 1 loop
        if at > cardinality(tokens) or (depth = 0 and tokens[at] = '|') then
            if start = 1 and at > cardinality(tokens) then
                return array[array_to_string(tokens, ' ')];
            end if;
            terms := terms || pg_temp.selection_terms(tokens[start:at - 1]);
            start := at + 1;
        else
            depth := depth + case tokens[at] when '(' then 1 when ')' then -1 else 0 end;
        end if;
    end loop;
    return terms;
end
$$;

-- The id of the group called `called` in `venue`, made empty if the venue has
-- none. `null` for what is no group name.
create or replace function pg_temp.selection_group_id(venue text, called text) returns text
language plpgsql as $$
declare
    found text;
begin
    select grp.id into found from public.fixture_groups grp
     where grp.venue_id = venue and grp.name = called;
    if found is not null or called !~ '^[a-z][a-z0-9_]*$' or called = 'all' then
        return found;
    end if;
    found := gen_random_uuid()::text;
    insert into public.fixture_groups (id, uid, venue_id, name, display_order)
    select found, room.uid, room.id, called,
           coalesce((select max(grp.display_order) from public.fixture_groups grp
                      where grp.venue_id = venue), 0) + 1
    from public.venues room where room.id = venue;
    return found;
end
$$;

-- The id of the group that holds what the term `tokens` matches in `venue`,
-- made if the venue has none of its name.
create or replace function pg_temp.selection_term_id(venue text, tokens text[]) returns text
language plpgsql as $$
declare
    called text;
    found text;
    heads text[];
begin
    if cardinality(tokens) = 1 then
        return pg_temp.selection_group_id(venue, tokens[1]);
    end if;
    -- `a & ~b` is `a_and_not_b`.
    select string_agg(case token
               when '&' then 'and' when '|' then 'or' when '~' then 'not'
               when '^' then 'xor' when '>' then 'else' else token end, '_' order by at)
      into called
      from unnest(tokens) with ordinality as part (token, at)
     where token not in ('(', ')');
    if called !~ '^[a-z]' then
        called := 'g_' || called;
    end if;
    if length(called) > 60 then
        called := left(called, 51) || '_' || left(md5(called), 8);
    end if;
    select grp.id into found from public.fixture_groups grp
     where grp.venue_id = venue and grp.name = called;
    if found is not null then
        return found;
    end if;
    heads := pg_temp.selection_eval(venue, tokens);
    found := pg_temp.selection_group_id(venue, called);
    -- A fixture all of whose heads matched is a member whole; any other is a
    -- member by each head that did.
    insert into public.fixture_group_members (id, uid, fixture_id, group_id, head_index, display_order, venue_id)
    with matched as (
        select head.fixture_id,
               count(*) filter (where head.key = any(heads)) as hit,
               count(*) as total
        from selection_heads head
        where head.venue_id = venue
        group by head.fixture_id
    ), members as (
        select fixture_id, -1 as head_index from matched where hit = total
        union all
        select head.fixture_id, head.head
        from selection_heads head
        join matched on matched.fixture_id = head.fixture_id
        where matched.hit < matched.total and head.key = any(heads)
    )
    select gen_random_uuid()::text, grp.uid, members.fixture_id, found, members.head_index,
           row_number() over (order by members.fixture_id, members.head_index), venue
    from members, public.fixture_groups grp
    where grp.id = found;
    return found;
end
$$;

-- The selection `expression` was in `venue`, by id.
create or replace function pg_temp.selection_of(venue text, expression text) returns jsonb
language plpgsql as $$
declare
    term text;
    part text[];
    id text;
    ids jsonb := '[]';
begin
    if btrim(expression) = '' or lower(btrim(expression)) = 'all' then
        return '{"all": true}';
    end if;
    foreach term in array pg_temp.selection_terms(pg_temp.selection_tokens(expression)) loop
        part := string_to_array(term, ' ');
        if part = array['all'] then
            -- The whole venue, whatever else is joined to it.
            return '{"all": true}';
        end if;
        id := pg_temp.selection_term_id(venue, part);
        if id is not null and not ids ? id then
            ids := ids || to_jsonb(id);
        end if;
    end loop;
    return jsonb_build_object('groups', ids);
exception when raise_exception then
    -- An expression the client could not read lit nothing, and still does.
    raise warning 'selection % of venue % is not an expression: %', expression, venue, sqlerrm;
    return '{"groups": []}';
end
$$;

-- A stored selection, converted if it is still an expression.
create or replace function pg_temp.selection_json(venue text, selection jsonb) returns jsonb
language sql as $$
    select case when selection ? 'expression'
                then pg_temp.selection_of(venue, selection ->> 'expression')
                else selection end
$$;

-- A draft holds a score: each clip's selection, and no `selection_seed`.
create or replace function pg_temp.selection_draft(venue text, score text) returns text
language sql as $$
    select jsonb_set(score::jsonb, '{clips}', coalesce(jsonb_object_agg(
        key,
        case when value ? 'selection'
             then jsonb_set(value - 'selection_seed', '{selection}',
                            pg_temp.selection_json(venue, value -> 'selection'))
             else value - 'selection_seed' end), '{}'))::text
    from jsonb_each(score::jsonb -> 'clips')
$$;

-- A layout's group cells hold a group's id, or `all`.
create or replace function pg_temp.selection_layout(venue text, layout text) returns text
language sql as $$
    select jsonb_set(layout::jsonb, '{pages}', coalesce((
        select jsonb_agg(jsonb_set(page, '{cells}', coalesce((
            select jsonb_agg(
                case when cell -> 'content' ->> 'kind' = 'group'
                          and cell -> 'content' ->> 'group' <> 'all'
                     then jsonb_set(cell, '{content,group}', to_jsonb(coalesce(
                              pg_temp.selection_group_id(venue, lower(cell -> 'content' ->> 'group')),
                              cell -> 'content' ->> 'group')))
                     else cell end
                order by cell_at)
            from jsonb_array_elements(page -> 'cells') with ordinality as cells (cell, cell_at)
        ), '[]')) order by page_at)
        from jsonb_array_elements(layout::jsonb -> 'pages') with ordinality as pages (page, page_at)
    ), '[]'))::text
$$;

update public.clips clip
set selection_json = pg_temp.selection_json(score.venue_id, clip.selection_json::jsonb)::text
from public.scores score
where score.id = clip.score_id and clip.selection_json like '%"expression"%';

update public.perform_patterns
set selection_json = pg_temp.selection_json(venue_id, selection_json::jsonb)::text
where selection_json like '%"expression"%';

update public.drafts draft
set base_json = pg_temp.selection_draft(score.venue_id, draft.base_json),
    state_json = pg_temp.selection_draft(score.venue_id, draft.state_json)
from public.scores score
where score.id = draft.score_id
  and (draft.base_json like '%"expression"%' or draft.state_json like '%"expression"%'
       or draft.base_json like '%"selection_seed"%' or draft.state_json like '%"selection_seed"%');

update public.perform_layouts
set layout_json = pg_temp.selection_layout(venue_id, layout_json)
where layout_json like '%"kind":"group"%' or layout_json like '%"kind": "group"%';

commit;
