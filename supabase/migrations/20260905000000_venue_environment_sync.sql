-- Deploy before clients that include environment in their venue payload.
-- Text is the canonical VenueEnvironment JSON, matching SQLite and the wire.
ALTER TABLE public.venues ADD COLUMN environment text NOT NULL
    DEFAULT '{"mode":"indoor","houseLevel":1.0}';
NOTIFY pgrst, 'reload schema';
