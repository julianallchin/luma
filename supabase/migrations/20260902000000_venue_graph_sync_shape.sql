-- The shape `20260829000000_venue_graph.sql` needed to actually be registered.
--
-- Deployed 2026-08-30, before the four tables were added to
-- `src-tauri/src/sync/registry.rs`. All four were empty — nothing had ever
-- pushed to them — so this replaces them outright rather than accreting ALTERs
-- around a shape no row was ever written in.
--
-- Two things the original shape got wrong, both found by trying to register it:
--
-- 1. **`uid`.** Only `venue_nodes` carried one. Every other syncable table
--    denormalizes the owner onto the row, and the sync engine is built on that:
--    the dirty sweep scopes by `uid`, `push::mark_synced` refuses a table with
--    no principal column, `TableMeta::payload_principal_matches` rejects a
--    payload without one, and `pull::execute_upsert` decides `origin`
--    ('local' vs 'remote', which is what makes a later local delete push a
--    tombstone) by comparing the row's `uid` to the signed-in user. Deriving
--    the owner through `node_id` instead — which is what the RLS policies do —
--    would mean teaching all four of those to run a join. The column is one
--    canonical way; the join would be a second.
--
-- 2. **`uuid` ids.** A venue's root node id is `'<venue_id>:venue'`
--    (`venue_graph::migrate`), not a UUID, and it cannot be changed: the local
--    admission trigger treats `venue_nodes.id` as immutable, so no migration
--    can rewrite the roots already on disk. Node ids are opaque strings; the
--    column type now says so. `venue_id` stays `uuid` — venue ids really are.
--
-- The RLS policies keep the node-derived reachability check and gain the
-- direct owner check the new column makes possible.

DROP TABLE public.venue_constraints;
DROP TABLE public.venue_node_params;
DROP TABLE public.venue_edges;
DROP TABLE public.venue_nodes;

CREATE TABLE public.venue_nodes (
    id text PRIMARY KEY,
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
    child_id text PRIMARY KEY REFERENCES public.venue_nodes(id) ON DELETE CASCADE,
    uid uuid NOT NULL,
    parent_id text NOT NULL REFERENCES public.venue_nodes(id) ON DELETE CASCADE,
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
    node_id text NOT NULL REFERENCES public.venue_nodes(id) ON DELETE CASCADE,
    uid uuid NOT NULL,
    key text NOT NULL,
    value double precision NOT NULL,
    created_at timestamptz NOT NULL DEFAULT (CURRENT_TIMESTAMP AT TIME ZONE 'UTC'),
    updated_at timestamptz NOT NULL DEFAULT (CURRENT_TIMESTAMP AT TIME ZONE 'UTC'),
    deleted_at timestamptz,
    sync_seq bigint NOT NULL DEFAULT 0,
    PRIMARY KEY (node_id, key)
);

CREATE TABLE public.venue_constraints (
    node_id text NOT NULL REFERENCES public.venue_nodes(id) ON DELETE CASCADE,
    uid uuid NOT NULL,
    my_socket text NOT NULL,
    target_node text NOT NULL REFERENCES public.venue_nodes(id) ON DELETE CASCADE,
    target_socket text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT (CURRENT_TIMESTAMP AT TIME ZONE 'UTC'),
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
-- whoever can reach its venue. The node check is what makes a row pointing into
-- someone else's venue impossible; the `uid` check is what makes the column the
-- push side sends agree with it.
CREATE POLICY venue_nodes_owner ON public.venue_nodes
    USING (uid = auth.uid()) WITH CHECK (uid = auth.uid());

CREATE POLICY venue_edges_owner ON public.venue_edges
    USING (uid = auth.uid()
        AND EXISTS (SELECT 1 FROM public.venue_nodes n WHERE n.id = child_id AND n.uid = auth.uid()))
    WITH CHECK (uid = auth.uid()
        AND EXISTS (SELECT 1 FROM public.venue_nodes n WHERE n.id = child_id AND n.uid = auth.uid()));

CREATE POLICY venue_node_params_owner ON public.venue_node_params
    USING (uid = auth.uid()
        AND EXISTS (SELECT 1 FROM public.venue_nodes n WHERE n.id = node_id AND n.uid = auth.uid()))
    WITH CHECK (uid = auth.uid()
        AND EXISTS (SELECT 1 FROM public.venue_nodes n WHERE n.id = node_id AND n.uid = auth.uid()));

CREATE POLICY venue_constraints_owner ON public.venue_constraints
    USING (uid = auth.uid()
        AND EXISTS (SELECT 1 FROM public.venue_nodes n WHERE n.id = node_id AND n.uid = auth.uid()))
    WITH CHECK (uid = auth.uid()
        AND EXISTS (SELECT 1 FROM public.venue_nodes n WHERE n.id = node_id AND n.uid = auth.uid()));
