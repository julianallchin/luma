-- Generated groups become saved collections once, preserving their existing names.
ALTER TABLE public.venues ADD COLUMN groups_initialized integer NOT NULL DEFAULT 0 CHECK (groups_initialized IN (0, 1));
