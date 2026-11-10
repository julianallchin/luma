# Venue workspace

The venue page is Rig mode's body (`Target::Venue`, `gpui/crates/app/src/patch.rs`),
beside the agent panel. It has the live scene above, and the fixture table and
groups below. The fixture column gets most of the
width. Both columns scroll independently.

## Scene

The centred floating bar contains Add, Objects, the View eye icon and the
available transform modes. Rig has no track clock and no separate
toolbar. Selecting an object opens its placement controls over the scene.
Patch fields stay in the table.

View holds the environment (Indoor or Outdoor), house lights, time of day,
haze (density, cloudiness, turbulence), fixture shadows, grid and gizmos. Haze
and environment are venue data and sync with the venue. Grid, gizmos and
render scale are settings of this device. FPS is a small top-right readout.
A click on it expands the performance details.

New icons come from Nucleo. The local bundle is `~/github/nucleo`. Selected
SVGs are embedded in `gpui/crates/ui/assets/nucleo`, so builds do not depend on
that directory.

## Fixtures and groups

The table columns are Fixture, Model, Mode, Universe and Address. Editing
group membership keeps the same fonts and column sizes. Numeric addresses are
monospace. Fixture names and modes are sans.

A group is a saved collection of fixtures. Groups have no parents. There is
no split between automatic and authored membership. A new venue can use
**Generate from stage** after its fixtures are placed. Generation adds missing
names and keeps existing collections. Later geometry changes never rewrite
memberships.

Creating a venue writes its root node in the same transaction. A venue
without a root is one whose rows are still arriving from sync, and every reader
shows it empty. No client converts an older venue; format changes are server
migrations.

The group editor supports naming, fixture membership and confirmed deletion.
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

Advanced patch tools stay behind Patch: occupancy, auto patch, channel ranges
and output routing.
