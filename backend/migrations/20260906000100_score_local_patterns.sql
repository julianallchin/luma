ALTER TABLE patterns ADD COLUMN score_id TEXT REFERENCES scores(id);
CREATE INDEX patterns_score_id ON patterns(score_id);
CREATE TRIGGER pattern_score_owner_insert BEFORE INSERT ON patterns
WHEN NEW.score_id IS NOT NULL AND NOT EXISTS (
    SELECT 1 FROM scores WHERE id = NEW.score_id AND uid IS NEW.uid
)
BEGIN SELECT RAISE(ABORT, 'A local Pattern must have the same owner as its score'); END;
CREATE TRIGGER pattern_score_owner_update BEFORE UPDATE OF score_id, uid ON patterns
WHEN NEW.score_id IS NOT NULL AND NOT EXISTS (
    SELECT 1 FROM scores WHERE id = NEW.score_id AND uid IS NEW.uid
)
BEGIN SELECT RAISE(ABORT, 'A local Pattern must have the same owner as its score'); END;
CREATE TRIGGER clip_pattern_scope_insert BEFORE INSERT ON track_scores
WHEN EXISTS (SELECT 1 FROM patterns WHERE id = NEW.pattern_id AND score_id IS NOT NULL AND score_id != NEW.score_id)
BEGIN SELECT RAISE(ABORT, 'This Pattern belongs to another score; copy it or save it to the library'); END;
CREATE TRIGGER clip_pattern_scope_update BEFORE UPDATE OF score_id, pattern_id ON track_scores
WHEN EXISTS (SELECT 1 FROM patterns WHERE id = NEW.pattern_id AND score_id IS NOT NULL AND score_id != NEW.score_id)
BEGIN SELECT RAISE(ABORT, 'This Pattern belongs to another score; copy it or save it to the library'); END;
