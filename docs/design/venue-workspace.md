# Venue workspace

The venue tab (`Target::Patch`) sits beside chat. It has the live scene above,
and the fixture table and groups below. The fixture column gets most of the
width. Both columns scroll independently.

## Scene

The centred floating bar contains Add, Objects, the View eye icon and the
available transform modes. The venue tab has no track clock and no separate
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

An older venue converts once, on first read (`venue_graph::ensure_migrated`).
The conversion derives the effective groups, keeps the selector names and
members, saves them into `fixture_groups`, and sets `venues.groups_initialized`.
The local-only `fixture_group_overrides` table is read only by this conversion.

The group editor supports naming, fixture membership and confirmed deletion.
Deleting a group leaves the physical fixtures in place. Whole-fixture
membership is editable. Existing per-head rows stay until that fixture leaves
the collection. A head-level membership editor is not built.

## Missing targets

A warning names selector names that saved scores use but the venue does not
have. A group edit opens the repair dialog when needed. An audit failure stays
visible and does not block the fixture table. The dialog offers:

- Recreate the missing name with the selected fixtures. An empty collection is
  allowed.
- Replace that name in affected scores with an existing group.

Replacement is `resolve_venue_group`
(`backend/src/dispatch/handlers/group_references.rs`). It parses selection
expressions and replaces exact identifiers only. Boolean operators and graph
labels stay unchanged. Each score is rewritten in its own transaction through
the score rows. The `changes` log records every row it changes (see
[sync.md](sync.md)). A failure stops the batch. Scores already repaired stay
repaired. A retry skips scores that no longer use the missing name.

Advanced patch tools stay behind Patch: occupancy, auto patch, channel ranges
and output routing.
