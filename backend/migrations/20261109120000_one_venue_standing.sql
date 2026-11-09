-- Who the admitted principal is to each venue they can open, and what they may
-- do with it: the one definition. `VenueAccess` admits by this view, a venue
-- read carries its answer, and every listing and trigger that asks "can this
-- principal reach that venue" joins it.
--
-- No permission model inside a venue: whoever can open it can edit all of it,
-- other people's scores included. Its door — the share code and memberships —
-- is its owner's. Every write needs a signed-in principal, because every row
-- it writes needs an owner: signed out, the guest namespace's venues (those
-- nobody owns) open for reading only, and have no door — nobody can join one.
--
-- Replaces the baseline's `auth_venue_access`, whose `owner_access` column
-- nothing reads any more (the triggers on it went with `stage_pieces` and
-- `fixture_group_overrides`) and whose guest branch also required the legacy
-- `venues.role` column not to say 'member', which `VenueAccess` never asked.
-- `auth_visible_tracks` reads this view by name and is unchanged.

DROP VIEW auth_venue_access;

CREATE VIEW auth_venue_access AS
SELECT
    venue_id,
    principal,
    owner,
    principal IS NOT NULL AS edit,
    owner AND principal IS NOT NULL AS manage
FROM (
    SELECT
        venue.id AS venue_id,
        admission.active_uid AS principal,
        -- A venue nobody owns is the guest namespace's, and signed out that
        -- is whose namespace this is.
        CASE
            WHEN admission.active_uid IS NULL THEN venue.uid IS NULL
            ELSE venue.uid IS admission.active_uid
        END AS owner,
        admission.active_uid IS NOT NULL AND EXISTS (
            SELECT 1
            FROM venue_members AS membership
            WHERE membership.venue_id = venue.id
              AND membership.uid = admission.active_uid
        ) AS member
    FROM venues AS venue
    CROSS JOIN auth_write_admission AS admission
    WHERE admission.singleton = 1
      AND admission.armed = 1
      AND admission.accepting = 1
      AND admission.maintenance = 0
      AND admission.remote_writes = 0
)
WHERE owner OR member;
