-- Authored graph and score documents are Git state, not independently
-- synchronized relational rows. A venue implementation override selects one
-- of those Git-authored implementations, so synchronizing that pointer without
-- the referenced Git object/ref state can resurrect a stale implementation or
-- select a graph that this device does not have. Generic row sync cannot
-- preserve Git ancestry, CAS, merges, or an absent projection across sign-out,
-- so allowing it to pull/push these blobs or their routing pointer creates a
-- second current-state authority.
--
-- Authenticated Git object/ref transport will be a separate capability. Until
-- then, implementations, track_scores, and venue_implementation_overrides are
-- deliberately absent from the sync registry. Remove every durable route by
-- which an older client could still push them.

DROP TRIGGER IF EXISTS sync_delete_implementations;
DROP TRIGGER IF EXISTS sync_delete_track_scores;
DROP TRIGGER IF EXISTS sync_delete_venue_impl_overrides;

DELETE FROM pending_ops
WHERE table_name IN (
    'implementations',
    'track_scores',
    'venue_implementation_overrides'
);

DELETE FROM sync_state
WHERE table_name IN (
    'implementations',
    'track_scores',
    'venue_implementation_overrides'
);
