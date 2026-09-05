# GPUI Pattern integration

The regular desktop app now offers a searchable score insertion menu with
Chase, Dissolve Flash, score-local Patterns, and library Patterns. Lighting nodes
create a local authored graph and then use the normal clip insertion/undo path.
Clip inputs include musical durations, spatial proportions, mapping and boundary
choices, selection, direct RGB color, and brightness. Save copy to library makes
an independent library graph; it does not redirect existing clips.

The typed engine runs inside the existing score renderer, including stage
playback and clip previews. Dissolve operates on independently addressable heads.
A connected node input no longer displays a second, ignored literal control.

Typed stage mappings use U+ toward stage right, V+ toward downstage, and Z+
upward, normalized over the selection. They do not use the legacy rig UV
projection (whose V was height). Major Axis remains the fitted oblique mapping.
The clip inspector groups shape, space, timing, and appearance, with width and
the Shape envelope first. The native Envelope editor supports presets and
adding, dragging, or removing custom knots. Direction independence, asymmetric
wrapped profiles, and native per-clip curve persistence have regression coverage.

## Validation

- Native UI: search, keyboard insertion, clip inspector, persisted score-local
  graph and clip, and independent library copy pass.
- Existing native clip-sheet editing/batch-edit/retargeting test passes.
- Backend: typed graph file round-trip and per-head playback pass.
- Backend: idempotent creation, cross-score placement refusal, and score deletion
  preserving local graph history pass.
- Catalogue enum decoding, IPC manifest, and native JSON golden checks pass.
- The full backend run passed 1,099 tests initially. Its native integration
  failures were corrected and rerun. Two pre-existing rig construction tests
  still fail: `a_draft_previews_a_component_and_stamps_copies_of_it` and
  `a_python_program_builds_a_rig_from_an_empty_venue`.

The GPUI workspace enables JSON insertion order, unlike the standalone backend
workspace. Three semantic golden tests compared serialized key order, causing
false failures. Those comparisons now compare JSON values. The canonical venue
revision test still checks exact bytes.

## Migration and remaining work

SQLite and Supabase migrations add Pattern score scope and ownership/placement
constraints. Graph file v3 adds Envelope inputs; frozen v1/v2 codecs migrate prior files.
Old softness connections become explicit Soft Edges → Shape connections,
preserving their exposed inputs and clip overrides. Local migrations have been tested on disposable libraries only; no live
library or Supabase migration was applied for this preview.

This is the native **score workflow**, not completion of the entire graph
redesign. The GPUI canvas still needs node insertion, port dragging, and input
exposure/default editing. Immutable library
references and mapping independently over multiple selected groups also remain.
Selection currently scopes the compiled graph. Typed components cannot yet mix
with legacy signal nodes.

The React/Tauri interface, alternate test interface, frontend tooling and Tauri
window/adapter entry points are removed. Some shared backend utilities still
have Tauri-shaped signatures and retain the base dependency. The old publishing
workflow is retired; the native workflow produces build artifacts, with installer
packaging/signing still required. GPUI startup also does not yet use the old
scheduler recovery helper; that existing host gap was not changed here.

Envelope follow-up validation: native preset selection and a custom-point drag
persist only the clip override; the Pattern default remains unchanged. The full
backend run passed 1,106 tests, with the same two rig construction failures and
10 ignored tests. The v2-to-v3 migration is idempotent and retains old softness
controls through the composable Soft Edges node. No live database was upgraded.
