ALTER TABLE public.track_beat_validations
ADD COLUMN verdict text NOT NULL DEFAULT 'unreviewed',
ADD COLUMN reason text;
UPDATE public.track_beat_validations SET verdict = CASE approved WHEN 1 THEN 'correct' ELSE 'unreviewed' END;
ALTER TABLE public.track_beat_validations
ADD CONSTRAINT beat_validation_verdict CHECK (verdict IN ('unreviewed', 'correct', 'incorrect')),
ADD CONSTRAINT beat_validation_reason CHECK (reason IS NULL OR (verdict = 'incorrect' AND reason IN ('tempo', 'offset', 'drift', 'bar_phase'))),
DROP COLUMN approved;
