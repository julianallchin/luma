# Venue workspace

One Venue tab beside chat: live scene above, fixture table and groups below. The fixture column gets most of the width; both columns scroll independently.

## Scene

The centered floating bar contains Add, Objects, the Nucleo eye icon for View, and available transform modes. There is no track clock or separate toolbar on the Venue tab. Selecting an object opens its placement controls over the scene. Patch fields stay in the table.

View contains room mode, House/Sun, haze and fixture-shadow/grid controls. Renderer Lab and its obsolete diagnostic controls are removed. FPS is always visible as a small top-right readout; clicking expands performance details.

New icons come from Nucleo. The permanent local bundle is `~/github/nucleo`; selected SVGs are embedded in `gpui/crates/ui/assets/nucleo` so builds do not depend on that directory.

## Fixtures and groups

The table columns are Fixture, Model, Mode, Universe, Address. Editing group membership keeps the same fonts and column sizes. Numeric addresses are monospace; fixture names and modes are sans.

Groups are saved collections of fixtures, without parents or a distinction between automatic and authored membership. A new venue can Generate from stage once its fixtures are placed. This produces a useful starting vocabulary; subsequent geometry changes never rewrite those memberships. Generation adds missing names and preserves existing collections.

Existing venues convert lazily and transactionally: derive the old effective groups including overrides, preserve canonical selector names and members, save them into `fixture_groups`, and mark `venues.groups_initialized`. Local SQLite and remote Postgres migrations add the same marker. Duplicate IDs produced by identically labelled legacy structures receive distinct saved IDs; selector names and memberships remain unchanged. The old override table remains only for converting older libraries. Tree mutation commands are retired.

The group editor supports naming, fixture membership and confirmed deletion. Deleting a group leaves physical fixtures in place. Whole-fixture membership is editable; existing per-head rows remain until that fixture is removed from the collection. A head-level membership editor remains future work.

## Missing targets

A visible warning identifies selector names used by saved scores but missing from the venue. Group edits open the repair dialog when needed. Audit failures remain visible without preventing the fixture table from loading. The dialog offers:

- Recreate the missing name with the currently selected fixtures (including an explicitly empty collection).
- Replace that name in affected scores with an existing group.

Repair parses selection expressions and replaces exact identifiers, preserving boolean operators and unrelated graph labels. Score edits go through authored history with revision checks. A batch commits one score at a time; a conflict stops the batch and remains visible. Retrying skips already repaired scores. Earlier revisions are retained, not rewritten.

Advanced patch tools remain behind Patch: occupancy, auto patch, channel ranges and output routing.

## Validation

Verification covers flat group generation, retained membership after geometry edits, conversion of legacy overrides, atomic membership edits, missing-selector recreation and replacement through score history, and native table/group/repair/render-settings workflows. Native screenshots use the GPUI app, without in-app model calls.

Verified against the actual EBF library: all 10 group names and their exact fixture memberships survive conversion. A SQLite backup was taken before migration. Both the saved-group marker and the previously pending score-local-pattern migration have been applied to Supabase.

Checks: workspace/all-targets passes; 20 backend group tests, the exact-selector repair test, 11 native patch tests and 15 native scene-builder tests pass. The broader venue test filter also exposes a failure in `venue_launch_picker_create_and_stale_reads_are_correlated`: its delayed catalogue error appears while it expects the picker. That navigation issue remains outside this change.

The real app still reports audio-record write-admission errors in its sync diagnostics (`signed-in write admission is closed or principal-mismatched`). The group data itself is verified remotely, and fresh pull/push passes complete without the schema errors. The audio admission issue has not been changed here.
