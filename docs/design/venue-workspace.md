# Venue workspace

The venue page is Rig mode's body (`Target::Venue`, `gpui/crates/app/src/patch.rs`),
beside the agent panel. It has the live scene above, and the fixture table and
groups below. The fixture column gets most of the
width. Both columns scroll independently.

## Scene

Nothing for adding or listing sits on the scene. A centred bar appears only
while a selection has a gizmo, with its Translate and Rotate modes. Rig has no
track clock and no separate toolbar. Patch fields stay in the table.

The column beside the scene is an inspector (`environment_panel`,
`gpui/crates/app/src/visualizer/settings.rs`). With a piece selected it shows
that piece's placement controls, Duplicate and Remove. With nothing selected
it shows the environment. Nothing floats over the scene for a selection.

View holds the environment (Indoor or Outdoor), house lights, time of day,
haze (density, cloudiness, turbulence), fixture shadows, grid and gizmos. Haze
and environment are venue data and sync with the venue. Grid, gizmos and
render scale are settings of this device. FPS is a small top-right readout.
A click on it expands the performance details.

New icons come from Nucleo. The local bundle is `~/github/nucleo`. Selected
SVGs are embedded in `gpui/crates/ui/assets/nucleo`, so builds do not depend on
that directory.

## Fixtures and groups

There is one fixture table. Its columns are Fixture, Model, Mode, Universe,
Address, Channels and Groups. Numeric cells are monospace. Fixture names and
modes are sans. Below its minimum width the table scrolls sideways. A row
names up to three of its groups and counts the rest.

The band over the page switches it between **Fixtures** (the table and the
groups), **Stage**, **Patch** and **Output**, and holds Add and Auto patch.
Nothing for adding or listing sits on the scene: Add opens the shared
palette, and Stage (`stage::elements`) is a table of every placed piece that
is not a fixture and what it is fixed to. A row selects its piece. Patch
(`gpui/crates/app/src/patch/footprint.rs`) draws every universe in use as a
grid of 512 square cells as wide as the page. The band's zoom sets how many
cells make a line: 128, 64 or 32. A fixture is a run of cells with its name
on the first, and a dot before the name when its address was set by hand. Dragging a run
re-addresses it through `set_fixture_address`, onto another universe's grid
too, and a refused drop says why under that universe. Collisions are drawn
from `universe_occupancy`, not recomputed. Output
(`gpui/crates/app/src/patch/outputs.rs`) is a table of the universes: how
many channels each uses, where it is sent, and Add output. Beside it are the
devices the drivers see and a button to the DMX Output settings.

Auto patch packs (`luma_scene::patch`): each run is a block in physical
order, blocks follow one another, and a block that will not fit rolls whole
to the next universe.

The stage, the table, the footprint and the groups panel share one selection
(`Patch::selected`). A click on a row selects it, command-click toggles a row
and shift-click takes the run from the last row clicked. A click on the name
of the one selected row edits it. Editing a cell of an unselected row selects
that row. A tag in the Groups column selects that group's fixtures.

A group is a saved collection of fixtures. Groups have no parents. There is
no split between automatic and authored membership. A new venue can use
**Generate from stage** after its fixtures are placed. Generation adds missing
names and keeps existing collections. Later geometry changes never rewrite
memberships.

Creating a venue writes its root node in the same transaction. A venue
without a root is one whose rows are still arriving from sync, and every reader
shows it empty. No client converts an older venue; format changes are server
migrations.

The groups panel has no editing mode, so a fixture row never changes what a
click on it means. A group's row selects its fixtures and shows how many it
holds and how many times it is used. While fixtures are selected each group
has a box: ticked when it holds all of them, a dash when it holds some. A
click puts the selected fixtures in, or takes them out when it held them all,
and writes at once. **New group** opens a name field and the group takes the
selected fixtures. A right-click on a group offers Rename and Delete group.
Deleting a group leaves the physical fixtures in place. Whole-fixture
membership is editable. Existing per-head rows stay until that fixture leaves
the collection. A head-level membership editor is not built.

## What references a group

A clip's selection, a Perform pattern's selection and a Perform group cell
hold a group's id, not its name (`luma_patterns::Selection`,
`backend/crates/patterns/src/selection.rs`). Renaming a group changes no
score.

A deleted group stays listed wherever it was selected and selects nothing
there. The delete confirmation says how many times the group is used across
how many scores, by how many Perform patterns and on how many Perform group
cells, and that deleting will break them. The counts come from
`venue_group_usage` (`backend/src/dispatch/handlers/group_references.rs`),
which counts ids and reads no graph. If the count fails, the confirmation
says the usage could not be checked.
