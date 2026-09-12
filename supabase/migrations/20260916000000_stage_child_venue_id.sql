-- The stage and group child rows carry their venue.
--
-- A sync rule's `with:` clause is a parameter query, and PowerSync caps one at
-- a thousand rows. `accessible_nodes` listed every `venue_nodes.id` a user can
-- reach — 1154 for one real library — so the stream failed with PSYNC_S2305
-- and nothing synced at all. The venues a user can reach are a handful; their
-- nodes are not.
--
-- So the child rows say which venue they belong to, the way `fixtures` and
-- `fixture_groups` already do, the sync rules filter on `venue_id IN
-- accessible_venues`, and the row-level security says the same thing in one
-- hop instead of two.
--
-- Idempotent: this one is applied by hand.

alter table public.venue_edges add column if not exists venue_id text;
update public.venue_edges e
   set venue_id = n.venue_id
  from public.venue_nodes n
 where n.id = e.child_id and e.venue_id is distinct from n.venue_id;
create index if not exists venue_edges_venue_idx on public.venue_edges (venue_id);

alter table public.venue_node_params add column if not exists venue_id text;
update public.venue_node_params p
   set venue_id = n.venue_id
  from public.venue_nodes n
 where n.id = p.node_id and p.venue_id is distinct from n.venue_id;
create index if not exists venue_node_params_venue_idx
    on public.venue_node_params (venue_id);

alter table public.venue_constraints add column if not exists venue_id text;
update public.venue_constraints c
   set venue_id = n.venue_id
  from public.venue_nodes n
 where n.id = c.node_id and c.venue_id is distinct from n.venue_id;
create index if not exists venue_constraints_venue_idx
    on public.venue_constraints (venue_id);

alter table public.fixture_group_members add column if not exists venue_id text;
update public.fixture_group_members m
   set venue_id = g.venue_id
  from public.fixture_groups g
 where g.id = m.group_id and m.venue_id is distinct from g.venue_id;
create index if not exists fixture_group_members_venue_idx
    on public.fixture_group_members (venue_id);

-- One hop instead of two. `can_access_venue_node` and
-- `can_access_fixture_group` have no other caller once these four are on
-- `venue_id`, but they are left in place: dropping a function a policy might
-- still reference by hand is not worth the tidiness.
do $$
declare
    entry record;
begin
    for entry in
        select * from (values
            ('fixture_group_members'),
            ('venue_edges'),
            ('venue_node_params'),
            ('venue_constraints')
        ) as t(name)
    loop
        execute format('drop policy if exists %I on public.%I', entry.name || '_select', entry.name);
        execute format('drop policy if exists %I on public.%I', entry.name || '_insert', entry.name);
        execute format('drop policy if exists %I on public.%I', entry.name || '_update', entry.name);
        execute format('drop policy if exists %I on public.%I', entry.name || '_delete', entry.name);
        execute format('create policy %I on public.%I for select to authenticated
                        using (public.can_access_venue(venue_id))',
                       entry.name || '_select', entry.name);
        execute format('create policy %I on public.%I for insert to authenticated
                        with check (public.can_access_venue(venue_id))',
                       entry.name || '_insert', entry.name);
        execute format('create policy %I on public.%I for update to authenticated
                        using (public.can_access_venue(venue_id))
                        with check (public.can_access_venue(venue_id))',
                       entry.name || '_update', entry.name);
        execute format('create policy %I on public.%I for delete to authenticated
                        using (public.can_access_venue(venue_id))',
                       entry.name || '_delete', entry.name);
    end loop;
end
$$;
