-- A venue's Perform layout was keyed by its venue's id. Under data format 2
-- (docs/design/data-layer.md) its id is its own, and "the layout of this
-- venue" is found by `venue_id`, which is therefore unique. No row is
-- rewritten: PROJECTION_VERSION 2 clears the table and downloads it again.

DROP INDEX idx_perform_layouts_venue;
CREATE UNIQUE INDEX idx_perform_layouts_venue ON perform_layouts(venue_id);
