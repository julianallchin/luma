-- A venue's lighting environment: what kind of room it is, and the one dial
-- that mode has. Venue truth, beside the name — every picture of the room is
-- taken under it.
--
-- The default is the picture the app has always drawn: indoor, house at full.
-- It is a column default rather than a backfill because the auth admission
-- triggers on `venues` abort row writes in migration context; DDL with a
-- default reaches every existing venue without one.
--
-- The JSON spelling is `luma_render::scene_desc::VenueEnvironment`'s own, so
-- the column, the wire and the agent verb are one string rather than three
-- encodings of one idea. Unreadable text reads back as the default (see that
-- type's `From<String>`), so this column can never fail a venue load.
--
-- Local-only: absent from `sync::registry`'s `venues` column list, so it is
-- invisible to push and untouched by pull.
ALTER TABLE venues
  ADD COLUMN environment TEXT NOT NULL DEFAULT '{"mode":"indoor","houseLevel":1.0}';
