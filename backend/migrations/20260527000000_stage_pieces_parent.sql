-- Sticky attachment between stage pieces. When `parent_piece_id` is set,
-- the row's pos_x/y/z and rot_x/y/z are interpreted in the parent's local
-- space (not world). Moving the parent moves the children. Detaching is a
-- runtime decision: write a NULL parent_piece_id to drop the relationship.

ALTER TABLE stage_pieces ADD COLUMN parent_piece_id TEXT REFERENCES stage_pieces(id) ON DELETE CASCADE;

CREATE INDEX idx_stage_pieces_parent ON stage_pieces(parent_piece_id);
