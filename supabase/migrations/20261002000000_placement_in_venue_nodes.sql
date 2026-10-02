-- A node's placement moves onto its row, and `venue_edges` goes.
--
-- A light must never exist without a place in the room. With the placement in
-- a second table, "a node with no edge" was a state the schema allowed. The
-- four placement columns now live on `venue_nodes`: every node but the root has
-- a parent (a CHECK says so), and deleting a parent deletes its subtree.
--
-- Data: every node the root cannot reach is deleted, with its subtree, its
-- params, its constraints and, for a light, its patch row and group
-- memberships. So is every patch row in a venue with a root that has no node
-- at all. The local migration makes the same decision for the same rows.
--
-- Symmetric with backend/migrations/20261002000000_placement_in_venue_nodes.sql.
--
-- Order:
--   1. Apply this file.
--   2. Redeploy `deploy/powersync/sync-config.yaml`, which no longer names `venue_edges`.
--   3. Ship the app build that writes the placement on the node. An older build
--      still uploads `venue_edges` rows, and those uploads are refused.

begin;

alter table public.venue_nodes
    add column if not exists parent_id text
        references public.venue_nodes (id) on delete cascade deferrable initially deferred,
    add column if not exists my_socket text,
    add column if not exists their_socket text,
    add column if not exists roll double precision;

create index if not exists venue_nodes_parent_idx on public.venue_nodes (parent_id);

do $$
begin
    if to_regclass('public.venue_edges') is null then
        return;
    end if;

    create temp table doomed_nodes on commit drop as
    with recursive placed (id) as (
        select id from public.venue_nodes where kind = 'venue'
        union
        select edge.child_id
        from public.venue_edges edge
        join placed on edge.parent_id = placed.id
    )
    select node.id from public.venue_nodes node
    where node.id not in (select id from placed);

    create temp table doomed_fixtures on commit drop as
    select fixture.id from public.fixtures fixture
    where fixture.id in (select id from doomed_nodes)
       or (not exists (select 1 from public.venue_nodes node where node.id = fixture.id)
           and exists (select 1 from public.venue_nodes root
                       where root.venue_id = fixture.venue_id and root.kind = 'venue'));

    delete from public.fixture_group_members
    where fixture_id in (select id from doomed_fixtures);
    delete from public.fixtures where id in (select id from doomed_fixtures);
    delete from public.venue_node_params where node_id in (select id from doomed_nodes);
    delete from public.venue_constraints
    where node_id in (select id from doomed_nodes)
       or target_node in (select id from doomed_nodes);
    delete from public.venue_edges
    where child_id in (select id from doomed_nodes)
       or parent_id in (select id from doomed_nodes);
    delete from public.venue_nodes where id in (select id from doomed_nodes);

    update public.venue_nodes node
       set parent_id = edge.parent_id,
           my_socket = edge.my_socket,
           their_socket = edge.their_socket,
           roll = edge.roll
      from public.venue_edges edge
     where edge.child_id = node.id;

    drop table public.venue_edges;
end
$$;

-- The deletes and the update leave deferred foreign-key checks pending, and
-- ALTER TABLE refuses to run while any are. Run them now.
set constraints all immediate;

alter table public.venue_nodes drop constraint if exists venue_nodes_placed;
alter table public.venue_nodes add constraint venue_nodes_placed check (
    case when kind = 'venue'
         then parent_id is null and my_socket is null
              and their_socket is null and roll is null
         else parent_id is not null and my_socket is not null
              and their_socket is not null and roll is not null
    end
);

alter table public.venue_nodes drop constraint if exists venue_nodes_not_own_parent;
alter table public.venue_nodes add constraint venue_nodes_not_own_parent
    check (parent_id <> id);

commit;

notify pgrst, 'reload schema';
