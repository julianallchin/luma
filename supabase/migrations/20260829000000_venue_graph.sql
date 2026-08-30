-- Remote counterpart of `src-tauri/migrations/20260829000000_venue_graph.sql`.
--
-- Deployed 2026-08-30. The four tables are local-only in
-- phase 3 — they are absent from `src-tauri/src/sync/registry.rs`, so nothing
-- pushes to them and nothing pulls from them. The filename says so on purpose:
-- rename it to `20260829000000_venue_graph.sql` at the same time as the two
-- follow-ups below, never before.
--
-- Why it is here at all, un-deployed: `stage_pieces` never synced (it is not in
-- the registry and there is no remote table), but `fixtures.pos_*`/`rot_*`
-- *did*. Phase 3 moves fixture placement into `venue_edges`, so until these
-- tables are deployed and registered, a fixture's placement stops travelling
-- between a user's machines. Stage layout is no worse off than it has ever
-- been; fixture placement is. That is the one regression phase 3 carries, and
-- this file plus the checklist below is how it is paid off.
--
-- To land it (in this order — the app must never register a table the remote
-- does not have):
--   1. Deploy this migration.
--   2. Add four `TableMeta` entries to `src-tauri/src/sync/registry.rs`:
--      `venue_nodes`   parents = ["venues"], conflict_key "id",
--                      columns id/uid/venue_id/kind/catalog_ref/label/created_at/updated_at
--      `venue_edges`   parents = ["venue_nodes"], conflict_key "child_id"
--      `venue_node_params` and `venue_constraints` — composite keys, so they
--                      need the registry's conflict_key to grow past one
--                      column first, or a surrogate `id`. That is the real work
--                      in step 2 and is why it is not done here.
--   3. Ship. Only then may a later local migration drop `stage_pieces` and the
--      `fixtures.pos_*`/`rot_*` columns, which phase 3 leaves in place and
--      unread.

CREATE TABLE public.venue_nodes (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    uid uuid NOT NULL,
    venue_id uuid NOT NULL REFERENCES public.venues(id) ON DELETE CASCADE,
    kind text NOT NULL CHECK (kind IN ('venue', 'stage', 'run', 'tower', 'piece', 'fixture', 'array')),
    catalog_ref text,
    label text,
    created_at timestamptz NOT NULL DEFAULT (CURRENT_TIMESTAMP AT TIME ZONE 'UTC'),
    updated_at timestamptz NOT NULL DEFAULT (CURRENT_TIMESTAMP AT TIME ZONE 'UTC'),
    deleted_at timestamptz,
    sync_seq bigint NOT NULL DEFAULT 0
);

CREATE INDEX idx_venue_nodes_venue ON public.venue_nodes(venue_id);
CREATE UNIQUE INDEX idx_venue_nodes_root ON public.venue_nodes(venue_id) WHERE kind = 'venue';

-- Keyed by the child, so "exactly one parent" is a primary key on both sides.
CREATE TABLE public.venue_edges (
    child_id uuid PRIMARY KEY REFERENCES public.venue_nodes(id) ON DELETE CASCADE,
    parent_id uuid NOT NULL REFERENCES public.venue_nodes(id) ON DELETE CASCADE,
    my_socket text NOT NULL,
    their_socket text NOT NULL,
    roll double precision NOT NULL DEFAULT 0.0,
    created_at timestamptz NOT NULL DEFAULT (CURRENT_TIMESTAMP AT TIME ZONE 'UTC'),
    updated_at timestamptz NOT NULL DEFAULT (CURRENT_TIMESTAMP AT TIME ZONE 'UTC'),
    deleted_at timestamptz,
    sync_seq bigint NOT NULL DEFAULT 0,
    CHECK (child_id <> parent_id)
);

CREATE INDEX idx_venue_edges_parent ON public.venue_edges(parent_id);

CREATE TABLE public.venue_node_params (
    node_id uuid NOT NULL REFERENCES public.venue_nodes(id) ON DELETE CASCADE,
    key text NOT NULL,
    value double precision NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT (CURRENT_TIMESTAMP AT TIME ZONE 'UTC'),
    deleted_at timestamptz,
    sync_seq bigint NOT NULL DEFAULT 0,
    PRIMARY KEY (node_id, key)
);

CREATE TABLE public.venue_constraints (
    node_id uuid NOT NULL REFERENCES public.venue_nodes(id) ON DELETE CASCADE,
    my_socket text NOT NULL,
    target_node uuid NOT NULL REFERENCES public.venue_nodes(id) ON DELETE CASCADE,
    target_socket text NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT (CURRENT_TIMESTAMP AT TIME ZONE 'UTC'),
    deleted_at timestamptz,
    sync_seq bigint NOT NULL DEFAULT 0,
    PRIMARY KEY (node_id, my_socket)
);

CREATE INDEX idx_venue_constraints_target ON public.venue_constraints(target_node);

-- The commit-ordered cursor every synced table carries
-- (`20260802000000_authored_revision_sync.sql`).
DO $$
DECLARE
    relation text;
BEGIN
    FOREACH relation IN ARRAY ARRAY[
        'venue_nodes', 'venue_edges', 'venue_node_params', 'venue_constraints'
    ] LOOP
        EXECUTE format(
            'CREATE UNIQUE INDEX %I ON public.%I(sync_seq)',
            'idx_' || relation || '_sync_seq', relation
        );
        EXECUTE format(
            'CREATE TRIGGER sync_seq_bump BEFORE INSERT OR UPDATE ON public.%I FOR EACH ROW EXECUTE FUNCTION private.bump_sync_seq()',
            relation
        );
        EXECUTE format('ALTER TABLE public.%I ENABLE ROW LEVEL SECURITY', relation);
    END LOOP;
END
$$;

-- Venue data is one authorization aggregate remotely too: a row is reachable by
-- whoever can reach its venue. `venue_nodes` carries the venue id; the other
-- three reach it through the node they belong to, which is also what makes a
-- row pointing into someone else's venue impossible.
CREATE POLICY venue_nodes_owner ON public.venue_nodes
    USING (uid = auth.uid()) WITH CHECK (uid = auth.uid());

CREATE POLICY venue_edges_owner ON public.venue_edges
    USING (EXISTS (SELECT 1 FROM public.venue_nodes n WHERE n.id = child_id AND n.uid = auth.uid()))
    WITH CHECK (EXISTS (SELECT 1 FROM public.venue_nodes n WHERE n.id = child_id AND n.uid = auth.uid()));

CREATE POLICY venue_node_params_owner ON public.venue_node_params
    USING (EXISTS (SELECT 1 FROM public.venue_nodes n WHERE n.id = node_id AND n.uid = auth.uid()))
    WITH CHECK (EXISTS (SELECT 1 FROM public.venue_nodes n WHERE n.id = node_id AND n.uid = auth.uid()));

CREATE POLICY venue_constraints_owner ON public.venue_constraints
    USING (EXISTS (SELECT 1 FROM public.venue_nodes n WHERE n.id = node_id AND n.uid = auth.uid()))
    WITH CHECK (EXISTS (SELECT 1 FROM public.venue_nodes n WHERE n.id = node_id AND n.uid = auth.uid()));
