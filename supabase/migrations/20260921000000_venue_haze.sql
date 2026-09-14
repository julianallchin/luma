-- Deploy before clients that include haze in their venue payload.
-- Text is the canonical VenueHaze JSON, matching SQLite and the wire.
ALTER TABLE public.venues ADD COLUMN haze text NOT NULL
    DEFAULT '{"enabled":true,"density":0.24,"appearance":{"cloudiness":0.35,"cloudSize":4.0,"turbulence":0.3,"windSpeed":0.15,"windDirection":30.0}}';
NOTIFY pgrst, 'reload schema';
