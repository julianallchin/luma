-- The stage and group child rows carry their venue.
--
-- A sync rule's `with:` clause is a parameter query, and PowerSync caps one at
-- a thousand rows. `accessible_nodes` listed every `venue_nodes.id` a user can
-- reach — 1154 for one real library — so the whole stream failed with
-- PSYNC_S2305 and nothing synced. The venues a user can reach are a handful;
-- their nodes are not.
--
-- So the child rows say which venue they belong to, the way `fixtures` and
-- `fixture_groups` already do, and the rules filter on `venue_id IN
-- accessible_venues` — a clause that counts venues, not nodes.
--
-- Denormalised on purpose: it is derivable from the parent, and deriving it in
-- a sync rule is exactly what does not scale.

ALTER TABLE venue_edges ADD COLUMN venue_id TEXT;
UPDATE venue_edges
   SET venue_id = (SELECT venue_id FROM venue_nodes WHERE id = venue_edges.child_id);
CREATE INDEX idx_venue_edges_venue ON venue_edges(venue_id);

ALTER TABLE venue_node_params ADD COLUMN venue_id TEXT;
UPDATE venue_node_params
   SET venue_id = (SELECT venue_id FROM venue_nodes WHERE id = venue_node_params.node_id);
CREATE INDEX idx_venue_node_params_venue ON venue_node_params(venue_id);

ALTER TABLE venue_constraints ADD COLUMN venue_id TEXT;
UPDATE venue_constraints
   SET venue_id = (SELECT venue_id FROM venue_nodes WHERE id = venue_constraints.node_id);
CREATE INDEX idx_venue_constraints_venue ON venue_constraints(venue_id);

-- `fixture_group_members` cannot exceed a thousand groups in practice, but it
-- was the last child addressed through its parent's id rather than its venue,
-- and one shape is easier to keep right than two.
ALTER TABLE fixture_group_members ADD COLUMN venue_id TEXT;
UPDATE fixture_group_members
   SET venue_id = (SELECT venue_id FROM fixture_groups WHERE id = fixture_group_members.group_id);
CREATE INDEX idx_fixture_group_members_venue ON fixture_group_members(venue_id);
