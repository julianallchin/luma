-- A clip's label is unique among its score's live clips on the server, and
-- there only. A checkpoint applies one row's removal and another's put in no
-- promised order, so a label that moved from one clip to another is on two
-- local rows for a moment, and a local unique index would refuse the
-- checkpoint. A document with a label on two clips is refused before it is
-- written (scores/rows.rs), and nothing reads a clip by its label in SQL.
DROP INDEX idx_clips_label;
