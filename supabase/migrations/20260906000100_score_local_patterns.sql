ALTER TABLE public.patterns ADD COLUMN score_id uuid REFERENCES public.scores(id);
CREATE INDEX patterns_score_id ON public.patterns(score_id);
CREATE FUNCTION public.check_pattern_score_scope() RETURNS trigger LANGUAGE plpgsql SET search_path = public AS $$
BEGIN
    IF TG_TABLE_NAME = 'patterns' THEN
        IF NEW.score_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM scores WHERE id = NEW.score_id AND uid IS NOT DISTINCT FROM NEW.uid) THEN
            RAISE EXCEPTION 'A local Pattern must have the same owner as its score';
        END IF;
    ELSE
        IF EXISTS (SELECT 1 FROM patterns WHERE id = NEW.pattern_id AND score_id IS NOT NULL AND score_id <> NEW.score_id) THEN
            RAISE EXCEPTION 'This Pattern belongs to another score; copy it or save it to the library';
        END IF;
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER pattern_score_owner BEFORE INSERT OR UPDATE OF score_id, uid ON public.patterns FOR EACH ROW EXECUTE FUNCTION public.check_pattern_score_scope();
CREATE TRIGGER clip_pattern_scope BEFORE INSERT OR UPDATE OF score_id, pattern_id ON public.track_scores FOR EACH ROW EXECUTE FUNCTION public.check_pattern_score_scope();
