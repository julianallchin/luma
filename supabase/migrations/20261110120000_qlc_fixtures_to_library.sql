-- Patched QLC+ fixtures become their Luma fixture library entries.
--
-- Each row below names a QLC+ definition and mode that the library
-- (`resources/fixture-library`) carries with the same channels in the same
-- order, so the patch, its addresses and every score that drives it are
-- unchanged; only the definition the fixture is drawn and driven from moves.
--
--   Showtec/Showtec-Sunstrip-Active.qxf  10 Channels Mode  lib:showtec/sunstrip-active  10ch
--   Clay_Paky/Clay-Paky-Sharpy.qxf       Standard          lib:claypaky/sharpy          16ch
--   Martin/Martin-Atomic-3000-LED.qxf    3-Channel         lib:martin/atomic-3000-led   3ch
--   Venue/Venue-Tetra-Bar.qxf            24-CH             lib:venue/tetra-bar          24ch
--
-- A library bar numbers its cells from the other end to the QLC+ mesh, so a
-- migrated bar turns half a turn about its mount: cell 1 stays where the
-- show put it. The generic ILDA projector stays a built-in.
--
-- A client before the library (0.6.2 and older) cannot read a `lib:`
-- fixture: it draws and drives these again once it updates.
--
-- Server only: the local database is a projection of these rows and syncs
-- the change down.

begin;

create temporary table qlc_to_library (
    qlc_path text primary key,
    qlc_mode text not null,
    lib_path text not null,
    lib_mode text not null,
    turns boolean not null
) on commit drop;

insert into qlc_to_library values
    ('Showtec/Showtec-Sunstrip-Active.qxf', '10 Channels Mode', 'lib:showtec/sunstrip-active', '10ch', true),
    ('Clay_Paky/Clay-Paky-Sharpy.qxf', 'Standard', 'lib:claypaky/sharpy', '16ch', false),
    ('Martin/Martin-Atomic-3000-LED.qxf', '3-Channel', 'lib:martin/atomic-3000-led', '3ch', false),
    ('Venue/Venue-Tetra-Bar.qxf', '24-CH', 'lib:venue/tetra-bar', '24ch', true);

-- A fixture node's id is its patch row's id.
update public.venue_nodes node
set catalog_ref = map.lib_path,
    roll = case
        when not map.turns then node.roll
        when coalesce(node.roll, 0) > 0 then node.roll - pi()
        else coalesce(node.roll, 0) + pi()
    end,
    updated_at = now()
from public.fixtures fixture
join qlc_to_library map
  on map.qlc_path = fixture.fixture_path and map.qlc_mode = fixture.mode_name
where node.id = fixture.id and node.kind = 'fixture';

update public.fixtures fixture
set fixture_path = map.lib_path,
    mode_name = map.lib_mode,
    updated_at = now()
from qlc_to_library map
where map.qlc_path = fixture.fixture_path and map.qlc_mode = fixture.mode_name;

commit;
