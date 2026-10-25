-- Retire the runtime venue conversion, server side.
--
-- 1. Delete twelve venues by id. Every child row goes with its venue through
--    the existing ON DELETE CASCADE foreign keys: venue_members, fixtures
--    (and their fixture_group_members), fixture_groups, venue_nodes (and their
--    venue_node_params and venue_constraints), scores (and their clips and
--    drafts), midi_modifiers, midi_bindings, folders (and their folder_tracks).
--    Tracks are not venue rows and stay. agent_threads.venue_id has no foreign
--    key, so the threads that name these venues stay too.
--
--    Dry run on 2026-10-02 (read-only): venues 12, venue_members 0,
--    fixtures 473, fixture_groups 62, fixture_group_members 399,
--    venue_nodes 975, venue_node_params 2799, venue_constraints 0, scores 45,
--    clips 2456, drafts 0, midi_modifiers 0, midi_bindings 3, folders 12,
--    folder_tracks 41. agent_threads naming them, kept: 265. Those counts are
--    three weeks older than this file; repeat the dry run before deploying.
--
--    Gasworks Park (346ce3ae-84e9-4c4f-916b-03bf4d115365) is kept.
--
-- 2. Give every remaining venue its root node. Creating a venue now writes its
--    root in the same transaction, and no client converts a rootless venue any
--    more. On 2026-10-02 every venue kept here already had one, so this
--    inserts nothing; it is here so the invariant holds whatever ran before.
--
-- 3. Drop the columns only the conversion read: venues.groups_initialized and
--    fixtures.pos_*/rot_*. deploy/powersync/sync-config.yaml selects `*`, so
--    the sync rules need no change.
--
-- Deploy order: ship the client first. A client from before this change
-- uploads groups_initialized and pos_*/rot_* in every venue and fixture
-- write, and PostgREST rejects a write naming a column that does not exist.
-- The new client sends neither, and the defaults these columns have cover its
-- inserts until this runs.
--
-- The matching local change is
-- `backend/migrations/20261024000200_drop_venue_conversion.sql`.

begin;

delete from public.venues
where id in (
    'fb8604bd-8cc4-44d0-a54f-f8513164970d', -- Julian's Room
    '220d9dc5-abc6-4537-bfbf-aee5bb0debe4', -- Mausoleum
    'fdcbacdb-fa12-4219-ac5c-cc1cc842981d', -- test
    '1dcad63e-65db-4780-91b6-a1b22dd0652f', -- Bill Graham Civic
    '59531e46-5928-4d11-b1c6-be1763c8a1f3', -- Bill Graham v2
    'ca3c674c-6b2d-4d8f-b905-d4021f08b405', -- Printworks
    'a9f3e9d3-07b5-4fb6-8810-5e078a6d32c0', -- Printworks v2
    '965c2642-3b66-4773-aa6d-43926c076e1e', -- Gasworks v2
    '7fb94fd7-2a5c-4d11-b485-ed1f5c5827df', -- Club
    '5d0c3bab-2690-45fc-be94-f22227082b6d', -- Strave 5/15
    '99a8a8e9-2bc4-4fac-82d3-33b4cb9e6a4f', -- EBF April 4 2026
    '0e696fa5-5222-438d-8790-08376b1c5787'  -- my led
);

insert into public.venue_nodes (id, uid, venue_id, kind)
select venue.id || ':venue', venue.uid, venue.id, 'venue'
from public.venues venue
where not exists (
    select 1 from public.venue_nodes node
    where node.venue_id = venue.id and node.kind = 'venue'
);

alter table public.venues drop column if exists groups_initialized;

alter table public.fixtures drop column if exists pos_x;
alter table public.fixtures drop column if exists pos_y;
alter table public.fixtures drop column if exists pos_z;
alter table public.fixtures drop column if exists rot_x;
alter table public.fixtures drop column if exists rot_y;
alter table public.fixtures drop column if exists rot_z;

commit;

notify pgrst, 'reload schema';
