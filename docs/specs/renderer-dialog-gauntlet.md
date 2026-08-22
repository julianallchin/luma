# Renderer + interaction gauntlet

**Status:** active implementation contract  
**Branch:** `codex/renderer-dialog-gauntlet`  
**Workspace:** `/tmp/luma-gauntlet.7qvhKU`  
**Reference UI:** `zeronsh/comet` (current checkout in `/tmp/comet-reference.z41Mgy`)  
**Visual reference:** Julian's Codex-derived shell direction already landed in `04cf321`

## 0. Meta-goal

Luma should feel like one native, authored instrument rather than a collection
of ports. The renderer must reach game-engine quality while remaining a
deterministic lighting tool. Venue selection, track acquisition and workspace
tabs must use one reusable glass-dialog and motion language. Every visible
claim is accepted by an independent critic against captures and executable
contracts, not by the agent that authored it.

This document includes every item from Julian's latest direction:

- investigate why Bevy was rejected and re-evaluate that decision;
- a full renderer update with a controllable directional sun (including off),
  texture inspection, PBR, elegant light transport, fast beautiful volumetric
  fixture light, and a renderer-specific test harness;
- replace the sidebar venue dropdown with a full-window glass dialog;
- build a general child-sized dialog whose container morphs width and height
  as its route changes, with extensible content transitions;
- add-track search across all Luma tracks, source import through Engine DJ or
  Rekordbox, shared playlist/crate/library browsing, background analysis,
  newest-first appearance, and explicit add-to-venue;
- restore the last-open venue on launch; no venue is an onboarding/create state;
- Chrome-like tab close motion and an add-tab menu containing Universe setup,
  Pattern editor, Track editor and Visualizer.

## 1. Gauntlet protocol

Each surface runs the same loop:

1. An **owner** receives one bounded surface, this spec, its reference assets,
   the dirty-tree rules, and exact build/capture commands.
2. The owner implements and produces fresh evidence but does not judge it.
3. A separate **critic** inspects code, tests and pixels from scratch. It returns
   only `SHIP IT` or `FAIL`, with ranked, reproducible defects.
4. A failed round returns to the same owner with the complete verdict. Evidence
   is regenerated. The critic re-judges the whole surface, not just the patch.
5. Accepted surfaces are committed independently, then integrated in dependency
   order. No owner commits its own unaccepted round.

Automatic failure:

- an assertion restates an implementation constant instead of measuring the
  promised behavior;
- a visual change has no fresh capture or a capture was not inspected;
- a state transition has no outside-in automation test;
- reduced motion is ignored;
- a loading, empty, error or cancellation state is unreachable in tests;
- shared primitives are bypassed by a second local implementation;
- renderer output misses its declared quality or frame-time budget.

## 2. Work tracks and dependency order

### R — renderer / game-engine state

R0 records the Bevy decision from the original Claude session and current code.
R1 builds a renderer lab before changing production output. R2 establishes
lighting and material controls. R3 upgrades volumetrics and light transport.
R4 integrates editor controls and the GPUI viewport.

### D — dialog system

D0 adopts or ports the required GPUI backdrop-blur seam. D1 implements modal
layering and focus/input isolation. D2 implements measured container morphing.
D3 implements route transition vocabulary and accessibility behavior.

### V — venue and launch state

V1 persists and restores the last valid venue. V2 provides onboarding when no
venue exists or the stored venue disappeared. V3 opens the venue selector from
the sidebar into the shared dialog.

### T — track acquisition

T1 gives venue membership an explicit durable meaning. T2 adds the Luma-wide
track browser route. T3 adds import source selection. T4 unifies Engine DJ and
Rekordbox behind one browser model. T5 imports/analyzes in the background and
reconciles the newly added row. T6 explicitly adds the chosen track to venue.

### W — workspace tabs

W1 adds the plus menu. W2 gives close/reflow Chrome-like motion. W3 gates menu
availability by current venue/track state and proves idempotent target opens.

D1–D3 precede V3/T2–T6/W1. T1 precedes the add-track UI. R runs independently
until R4 touches the visualizer tab.

## 3. Renderer acceptance

The renderer exposes a closed, serializable scene/environment contract:

- directional sun: `enabled`, direction, illuminance/color, shadow toggle;
- environment/ambient contribution independently controllable;
- debug views for base color, normals, metallic, roughness, depth, shadow and
  volumetric accumulation;
- glTF base-color textures visible with correct sRGB handling;
- metallic-roughness GGX PBR with stable tonemapping;
- fixture beams remain physically tied to the fixture state and lens geometry;
- volumetrics are depth-aware, temporally stable, bounded by fixture cone/range,
  visually smooth at performance resolution, and deterministic in capture mode;
- the editor keeps camera/orbit, picking, gizmos and overlays after tonemapping;
- sun off + fixtures off is genuinely dark except for the configured ambient;
- no live render path performs a synchronous GPU readback on the UI thread.

Renderer lab controls must change every parameter above at runtime and show
frame GPU/CPU timing. Golden scenes cover: textured PBR asset, metal/roughness
sweep, sun direction/off, hard/soft shadow, one beam, overlapping beams,
occluded beam, gobo seam, and 32/128/512 fixture stress scenes. Goldens store
the full input descriptor beside the image. Numeric probes cover NaN/Inf,
camera matrices, energy monotonicity and deterministic hashes. Performance
budgets are declared per target machine before R3 is accepted. On the initial
Apple-silicon target at 1920×1080 with 256 active fixture cones, the provisional
budgets are GPU p95 <= 6.5 ms and CPU scene-update/encode p95 <= 1.5 ms; the
volume pass owns at most 3.0 ms. A 64-cone tier targets <= 8.0 ms total GPU for
120 Hz. Baseline JSON must name adapter, OS, build profile and display scale.

The production path is staged: independent environment/sun/haze first;
resident geometry and asynchronous triple-buffered presentation second; full
glTF material maps, tangent generation, IBL, stable cascaded sun shadows and
consistent reverse-Z third; clustered surface spotlights fourth; tiled cone
lists plus blue-noise/temporal volumetrics fifth. The steady-state UI thread
must never call a blocking GPU poll or upload unchanged geometry/textures.

The engine decision is not assumed. Bevy is adopted only if it removes more
renderer/editor infrastructure than it adds and can render into Luma's GPUI
surface without a competing window/input/world lifetime. A hybrid must name the
exact Bevy crates used; “use Bevy” is not an architecture.

### 3.1 Bevy decision record

The earlier renderer work did not run a Bevy comparison. Bevy was rejected in
conversation because it was assumed to own the event loop/window, spread ECS
through the application, add a large and fast-moving dependency, and duplicate
Luma's custom shader work. The subsequent renderer-design task was explicitly
constrained to a purpose-built non-ECS renderer, so its successful custom-wgpu
implementation cannot be read as evidence from a Bevy spike.

Two conclusions survive that archaeology. GPUI remains the sole window, input,
and app-state owner, and Luma's analytic fixture haze remains differentiating
render code. The event-loop objection does not survive: current Bevy supports
an externally driven headless renderer with `WinitPlugin` disabled. ECS
contamination is avoidable if the render world is private and receives a
one-way snapshot from Luma's evaluator/scene model.

R1 therefore builds `luma-render-bevy` beside, not over, `luma-render`. It must:

- be explicitly updated by the host, create no window, and return BGRA through
  the existing presentation boundary;
- render a material lab with base-color, metallic-roughness, normal, occlusion,
  and emissive textures;
- render controllable sun, sun-off, shadows, ambient/environment light, and a
  120-fixture scene using Luma's analytic haze;
- keep fixed time, seed, camera, exposure, and an asset-ready capture barrier;
- separately time evaluator, scene sync, extraction/prepare, GPU, readback, and
  GPUI copy, comparing p50/p95 with the current 8.9/12.0 ms release baseline;
- preserve deterministic renderer descriptors and existing fixture-shape
  goldens while adding new intended-look goldens for upgraded PBR.

Only a passing spike may replace the production backend. Failure leaves the
current renderer in place and turns the spike's measured gaps into bounded
custom-wgpu work; there is no indefinite dual-backend architecture.

## 4. Dialog primitive acceptance

The full-window modal plane:

- occludes pointer, scroll and keyboard interaction with the shell beneath;
- traps focus and restores it to the opener on dismissal;
- paints a dimmed/blurred background and a separately frosted foreground card;
- keeps traffic-light window controls usable without exposing shell controls;
- dismisses by Escape and optional scrim click according to route policy;
- snaps under reduced motion.

Real blur is a capability gate. Luma's current upstream GPUI revision exposes
window blur but not Comet's per-element `paint_backdrop_blur`; an alpha scrim is
not accepted as blur. D0 must either pin a compatible GPUI implementation with
backdrop and filtered-subtree painting, or add the smallest equivalent API and
pixel-test it. Background blur is region-aware (sidebar stronger than center),
while the foreground card remains a separately frosted readable layer. If a
platform cannot composite blur, it uses an explicit opaque readable fallback.

The child owns its intended size through a route descriptor. The dialog owns
one live container rect and morphs from the currently painted rect to the new
route's width/height. A mid-flight route reversal starts at the visible rect,
never at the stale source or destination. Unknown intrinsic height is measured
before the morph commits; there is no one-frame size snap.

Initial transition `Right`:

- outgoing A begins at rest and travels a short distance right while fading
  out and increasing blur;
- incoming B starts the same short distance left, travels to rest, fades in and
  unblurs;
- container width and height move concurrently on the shared root curve;
- travel is deliberately small so the result reads as a morph, not navigation.

The transition API separates choreography from route content. It must admit
future `Scale`, `CrossFade` and custom evaluators without changing the dialog
container or route reducer. Tests pin start/mid/end samples, reversal, route
replacement, intrinsic-size handoff and reduced motion.

## 5. Venue and launch acceptance

- The sidebar venue trigger opens the dialog; it never expands an inline menu.
- The dialog venue route searches/selects all venues and exposes create venue.
- Selecting a venue closes the dialog, refreshes the sidebar, and persists that
  venue as last open.
- Launch restores the last venue when it still exists.
- Missing/deleted stored venues fall back cleanly.
- One or more venues with no stored choice opens venue selection.
- Zero venues is onboarding: a focused create-venue state, not an empty picker.
- Loading is visually distinct from empty and error. No empty-state flash occurs
  while the venue query is pending.

## 6. Add-track acceptance

The sidebar plus opens the shared dialog on `TrackBrowser`:

1. Search covers every track already in Luma, not only the current venue.
2. Default ordering is newest added first with deterministic tie-breaking.
3. A persistent bottom import button opens `ImportSource`.
4. If Luma has no tracks, the import button is the centered empty state.
5. `ImportSource` offers Engine DJ and Rekordbox.
6. Either source morphs into one shared `SourceLibrary` route: playlist/crate
   navigation, search, selection, loading/error/empty, and one import action.
7. Import starts the existing background analysis pipeline and shows progress;
   closing the dialog does not cancel analysis.
8. Completed imports reconcile into `TrackBrowser` as newest additions without
   restarting the app.
9. Clicking an existing/imported track explicitly adds it to the selected
   venue and closes (or advances to a success state if needed).
10. A track is “in venue” because of durable venue membership, not because it
    happens to have at least one clip. An empty newly created score must remain
    visible in the venue.

Engine DJ and Rekordbox adapters return one UI-owned source model. Their path,
playlist and import peculiarities stay below the Library facade. The GPUI app
does not call Tauri-only command bodies or parse progress prose.

The backend foundation ports import ownership from `AppHandle` to the existing
`StorageRoot + Events` services, then registers those write commands in the
dispatcher. Import returns after durable insertion; background analysis owns
its own lifetime and publishes structured progress. The membership query reads
score existence, is idempotent under repeat add, and is tested with a score
that has zero clips/annotations.

## 7. Workspace-tab acceptance

- `+` sits with the tab strip and opens a glass menu listing Universe setup,
  Pattern editor, Track editor and Visualizer.
- Disabled choices explain which missing venue/track/pattern prerequisite they
  need.
- Opening is target-idempotent and selects an existing matching tab.
- Closing mirrors Chrome: the closing chip contracts/fades, neighboring chips
  glide into the released slot, the pointer can close consecutive tabs without
  chasing a moving close target, and the selected neighbor follows existing
  `Tabs::close` semantics.
- Middle click and the close affordance use the same close transition.
- Keyboard close remains immediate in intent but uses the same visual state.
- Reduced motion snaps to the final tab strip.

## 8. Verification matrix

Each accepted round runs its narrow tests plus:

```sh
cd /tmp/luma-gauntlet.7qvhKU/gpui
cargo fmt --all -- --check
cargo build --workspace --all-features
cargo clippy --workspace --all-targets
cargo test -p gpui-agent --all-features
cargo test -p luma-render
cargo test -p luma-scene

cd /tmp/luma-gauntlet.7qvhKU
cargo test --manifest-path src-tauri/Cargo.toml
```

Visual rounds produce before/after/current-reference captures at the same pixel
size and state. Render rounds additionally record timings and golden deltas.

The existing `gpui-agent` harness is the integration authority for every UI
claim. Semantic mode drives actions, focus, clicks, drags, typing, snapshots,
and deterministic frame advancement; pixel mode supplies real GPUI rendering,
cropped screenshots, and draw timing. New behavior extends that harness with
observable roles/actions or narrow geometry/effect hooks instead of creating a
parallel app driver. Renderer unit/golden tests judge pixels below GPUI;
`gpui-agent` judges the same renderer controls and state in the assembled app.
