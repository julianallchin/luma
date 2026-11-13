-- The score a duplicate was made from, and a person's verdicts on what an
-- agent made: one per turn (`message_id`) or per clip (`clip_id`).
--
-- "One per turn" and "one per clip" hold on the server, among live rows, and
-- not here: a checkpoint applies one row's removal and another's put in no
-- promised order, so a local unique index would refuse it
-- (20261112121000_clip_labels.sql). `services/feedback.rs` writes the one row.
--
-- Symmetric with supabase/migrations/20261113120000_provenance.sql.
ALTER TABLE scores ADD COLUMN copied_from_score_id TEXT;

CREATE TABLE feedback (
    id TEXT PRIMARY KEY NOT NULL,
    uid TEXT,
    score_id TEXT,
    thread_id TEXT,
    message_id TEXT,
    clip_id TEXT,
    rating INTEGER NOT NULL CHECK (rating IN (-1, 1)),
    note TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    CHECK (message_id IS NOT NULL OR clip_id IS NOT NULL),
    FOREIGN KEY (score_id) REFERENCES scores(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED
);
CREATE INDEX idx_feedback_score ON feedback(score_id);
CREATE INDEX idx_feedback_message ON feedback(message_id);
CREATE INDEX idx_feedback_clip ON feedback(clip_id);
CREATE TRIGGER feedback_updated_at AFTER UPDATE ON feedback FOR EACH ROW
BEGIN UPDATE feedback SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = OLD.id; END;
