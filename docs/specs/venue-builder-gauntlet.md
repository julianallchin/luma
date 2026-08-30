# Venue builder gauntlet

**Status:** acceptance contract for `docs/design/venue-graph.md` phases 3b–6.
That doc decides; this one says what evidence proves each decision landed.
**Meta-goal:** a rig is *built*, never typed — the human places structure on the
stage page and fixes paperwork on the patch page, the agent calls the same
resolver through a Python facade, same vocabulary. One machine, provably.

## 1. Gauntlet protocol

1. An **owner** gets one bounded surface, this spec, the design doc, the
   dirty-tree rules, and the commands in §10.
2. It implements and produces fresh evidence but does not judge it.
3. A separate **critic** inspects code, tests and pixels from scratch and
   returns only `SHIP IT` or `FAIL`, with ranked, reproducible defects.
4. A failed round returns to the same owner; evidence is regenerated and the
   critic re-judges the surface, not just the patch.
5. Accepted surfaces commit independently and integrate in dependency order; no
   owner commits its own unaccepted round.

Automatic failure, cited by tag per surface:

- **AF1** an assertion restates a constant instead of measuring the behaviour;
- **AF2** a visual change has no fresh capture, or none was inspected;
- **AF3** a state transition has no outside-in automation test;
- **AF4** reduced motion is ignored;
- **AF5** a loading, empty, error, refusal or cancellation state is unreachable;
- **AF6** shared primitives are bypassed by a second local implementation;
- **AF7** renderer output misses its declared quality or frame-time budget;
- **AF8** a position is editable anywhere but the stage page — any path writing
  `u/v/yaw/trim` or an edge from a table cell, a field, or a
  `set_position`-shaped API;
- **AF9** a fixture exists that no distribution and no patch-page non-placed add
  created — no origin pile, no bare insert, no self-authored pose;
- **AF10** a second copy of the allocator, grouping, or naming rule exists.

## 2. Surfaces and dependency order

    3b ─┬─▶ B1 addressing ─┬─▶ B3 distribute ─┬─▶ B4 stage page ─┬─▶ B6 facade ─▶ B7 views
        └─▶ B2 grouping ───┘                  └─▶ B5 patch page ─┘

## 3. B1 — addressing

**Scope.** One backend allocator. One universe per run: consecutive addresses in
physical order along it (`along(t)` ascending, ties by node id), rolling to the
next free universe when the footprint would cross 512, as real rigs do to keep a
fault inside one structure. Collisions and overflow are **refused**, never
truncated. Patch-page edits override; auto-patch re-derives from placement,
dropping overrides for fixtures it touches and reporting how many moved. **Out
of scope:** Art-Net binding (B5), sACN, RDM, UI.

**Evidence and acceptance.** `cargo test -p luma-scene`, plus the `(node,
universe, address, footprint)` table per golden venue in
`harness/goldens/patch-allocation.json`. Reordering fixtures along a run and
re-patching changes addresses to match, no literal start address asserted. An
overflowing run lands wholly in the next universe, and says so. A refused
collision leaves the database unchanged and names the conflict. The three
frontend first-fit allocators die in this diff; the critic greps. **Applies:**
AF1, AF3, AF5, AF10.

## 4. B2 — grouping

**Scope.** Groups are **derived sets shown as a tree**; a fixture may be in
several. One derivation: role (wash / spot / beam / strobe / blinder / pixel /
fx, from the QLC+ `Type` and channels, `src-tauri/src/models/fixtures.rs`) →
class (`horizontal`/`vertical` for a run standing alone, `left wing`/`right
wing` for one bolted to a stage, by the run's *attachment* side against the
stage's resolved surface centre) → row (one per distribution, never merged,
named by the piece's label, else — for a pair — by the axis it measurably
separates on, else `row n`) → split (the halves a row's fixtures fall into where
they separate: `top`/`bottom` by height, `left`/`right` across,
`downstage`/`upstage` by depth) → cross-cuts (one split name unioned across the
role). Names are model plus a running number (`aura 1`…); rename/move/merge sit
on top as overrides, and **a touched node is never re-derived**. **Out of
scope:** groups in the pattern editor or scores.

**Measured, never ranked.** A positional name is claimed only where there is a
gap to claim it with: the widest gap between neighbours must beat the spacing
around it, and never counts below a documented absolute — both sides of that
threshold tested. Rows separating on nothing are numbered; an evenly spaced run
does not split at all; a class holding one unlabelled row collapses that level,
the class node already *being* that set. A cross-cut is emitted only where two
or more **leaves** carry the name: a split row contributes its halves.

Three canonical venues, verbatim in `harness/goldens/venue-groups.json`:

    (a) led bars / horizontal / {top, bottom} + vertical / {left, right}
    (b) spots / {left wing, right wing} / {top, bottom} + spots / {top, bottom}
    (c) spots / {left, right} wing / {downstage, upstage} / {top, bottom}
        + spots / {top, bottom}

**Acceptance.** Those trees are produced exactly, node for node, name for name,
compared as trees not counts; the critic rebuilds each mirrored and checks the
same *sets*, member by member — path lists alone prove only the vocabulary.
Moving a truss changes the split with no group edit. Renaming a derived node
then adding fixtures keeps the name and files the new ones under it. A name
colliding with anything else in the venue is refused, both directions — derived
and authored groups share one namespace. No write leaves a stale derived answer
behind, including a read between write and commit. **Applies:** AF1, AF3, AF10.

## 5. B3 — the distribute command

**Scope.** One command, one transaction: `distribute(feature, fixture, count,
spacing|span)` places, names, groups and patches — the only fixture constructor
besides the patch page's non-placed add. Fit failure reports the *needed* length
("needs 4.0 m, run is 3.0 m — extend?") and places nothing; never silently
drops, clips or overlaps. **Out of scope:** the popover calling it.

**Evidence and acceptance.** `cargo test -p luma-scene`: exact fit, `span` and
`spacing × count` longer than the run, count 1, count 0; a golden of the
resulting `venue_nodes/edges/params` rows, whose names, groups and addresses
come from B1/B2. The failure report's stated need matches the length that, once
extended, makes the same call succeed — measured by calling `extend` with it. A
failed distribute leaves zero new rows; two distributions on one run interleave
in physical, not creation, order. **Applies:** AF1, AF3, AF5, AF9.

## 6. B4 — stage page

**Scope.** The design doc's "stage page is the picture": an empty venue draws its grid and is
buildable, no toolbar; `+` → add-element dialog in the existing gpui picker pattern (preview
left, searchable list right, unplaced fixtures a section) → **place mode**, ghost down the snap
ladder socket → surface → grid with hysteresis, click places, configure inline, the mode
persists with params inherited, Esc out one level; the face popover (count, layout, fixture) as
`distribute`'s only caller (B3); open-socket extend at the ray-measured gap, longer refused; a
dismissible inspector sheet on selection, Duplicate / Flip / Detach in its context menu; **no
gizmo on snapped pieces**; controls may live in the 3D picture (`scene_desc::Editor`,
`render/src/overlay.rs` extended). **Out of scope:** addresses, universes.

**Evidence and acceptance.** `tests/headless/venue_builder.rs` (`mod` into `main.rs`) drives `+`
→ place → snap → extend → duplicate → distribute through the tree and `app.painted()`, `CAMERA`
pinning that build gestures never move the camera; `app_pixel/venue_builder_pixels.rs` covers
the ghosts, gizmos and popover. An empty venue paints the grid and the first piece lands from
`+` alone. Two placements happen without reopening the dialog, the second's params equalling the
first's untouched ones. The popover anchors within **32 px** of the projected face point; a
value change moves its ghosts in `painted()` before Apply; Esc leaves zero new nodes. No painted
node is a state label ("Hand:", socket lists, "distribute onto"), swept while a piece is held. A
snapped piece exposes no gizmo; a drag moves the run (resolved parent pose). Extend past the gap
refuses in `painted()` and adds no node; equal to it makes one edge plus a far-end constraint
`dangling()` reports satisfied. Duplicate + flip on an asymmetric wing `describe()`s identically
to the hand-built opposite. Placing empties the fixture from the list, detaching returns it,
trim moves the subtree, reduced motion snaps ghosts. **Applies:** AF1–AF6, AF8, AF9.

## 7. B5 — patch page

**Scope.** Editable inventory table (fixture, mode, universe, address, label,
group, output); a per-universe footprint strip, collisions in red; Auto Patch
calling B1; outputs as a **table** binding discovered Art-Net nodes to
universes, replacing the `(net<<8)|(sub<<4)|(u&0xF)` arithmetic; an add-N dialog
morphing from picker to count/mode in the shared dialog container. **Out of
scope:** anything positional — no u/v/yaw/trim column, no "move to truss".

**Evidence and acceptance.** `tests/headless/venue_patch.rs`; pixels of the
strip with and without a collision and of both add-N dialog states; an
`app.painted()` sweep proving the picker→count morph never flashes an empty
table. A colliding address edit is refused, shows red, and leaves the stored
value unchanged. Auto Patch after a stage-page move re-derives that address and
changes nothing about placement. Universes 17 and 1 resolve to different output
rows — asserted on the resolution, not the formula. Adding N unplaced fixtures
adds N unplaced fixtures and no nodes. No path here writes `venue_edges` or a param.
**Applies:** AF1, AF3, AF5, AF8, AF9, AF10.

## 8. B6 — Python facade

**Scope.** The facade sits in the same `luma.*` namespace as `luma.track`:
`luma.venue.attach`, `.extend`, `.array`, `.aim`, `.duplicate(+flip)`, `.trim`,
`.describe`, `.dangling`, `.unplaced` — verbs and verification channels in stage
vocabulary, no coordinates, no `set_position`. Each returns the `Placement` or
report type **the resolver already emits**; a facade-local third shape is AF6
and AF10. Same resolver, allocator and grouping as the pages, plus the thread
scope that lets an agent build a rig with no track in the library. **Out of
scope:** new verbs; a second spelling of one is AF6.

**Venue-scoped threads.** A thread scope kind `Venue { venue_id }`, carrying no
track and no score. Its cells bind `luma.venue` only: `luma.track` is *absent*
from the namespace, not an erroring stub — the error is defined out of
existence. The in-app chat opens on this scope from the patch and stage pages.
`luma-mcp` gains `open(venue=…)`, which pins such a thread and **mints no
score** — a score is a track's membership in a room, and there is no track to
have one; `reset` rebinds the same venue thread.

**Usability run.** The gauntlet critic, aimed at the tool surface. A headless
script dispatches an agent holding *only* the facade plus `describe` and
`render`, gives it a task in stage words ("10 m portal downstage, 8 movers on
the crossbar facing the house, two towers with 4 washes each"), and collects
four artefacts, shipped with the round: the tool-call transcript, the final
`describe()`, front and top renders, and the agent's own friction notes. A
critic judges the rig against the task and files what it finds in
`docs/AGENT_FRICTION.md` under a new **Tool friction** subheading — product
friction still goes in the task report.

**Evidence and acceptance.** One test builds a rig twice — by script through the
facade, and by the harness driving B4/B5 — then diffs `venue_nodes`,
`venue_edges`, `venue_node_params` and `describe()`; the rig holds a wing, a
duplicate+flip, a distribution, an extend-to-gap and a trim, and both runs must
produce **identical rows** (ids normalised by tree position) and `describe()`.
Every mutating call returns a `Placement`; the only hard errors are a
socket-type mismatch and an extend past the measured gap — everything else warns
and places, each warning path reachable in tests, and `dangling()` names the
far-end constraint the test leaves open. A second headless run opens a venue
thread against a library with **no tracks** and builds a rig through the facade:
no `score_id`, none created, and the app's stage-page chat shows that thread.
The usability run completes with the agent never needing a coordinate and every
refusal carrying a suggested fix. **Applies:** AF1, AF3, AF5, AF6, AF8, AF9,
AF10.

## 9. B7 — tile map and POV render

**Scope.** Top-down quantized tile map ("Gauntlet view") and
`render(view=fixture.pov)`, in that order of preference, front render last.
**Evidence.** Golden tile maps per canonical venue in
`harness/goldens/venue-tiles.json` (text, diffable); POV captures under
`harness/goldens/scenes-wgpu/` via `render-goldens`, each with its descriptor.
**Acceptance.** The tile map tells the two canonical venues apart at a glance,
and its diff localises a one-piece move to one tile row. A fixture POV shows
what its mount normal points at — changing `aim` changes the frame, compared as
frames not matrices. No per-frame cost above budget. **Applies:** AF1, AF2, AF7.

## 10. Verification matrix

Every accepted round runs its narrow tests plus:

```sh
cd gpui
cargo fmt --all -- --check
cargo check -p gpui-agent --all-targets        # headless tree; no --features
cargo clippy --workspace --all-targets
cargo test -p luma-scene -p luma-render
cargo test -p gpui-agent --test headless       # append a name to filter a file
cargo run -p luma-render --release --bin render-goldens -- --check
cargo test --manifest-path ../src-tauri/Cargo.toml
# Pixel rounds get the second tree — never flip features in the headless one.
export PIXEL_TARGET="$(git rev-parse --show-toplevel)/gpui/target-pixel"
CARGO_TARGET_DIR="$PIXEL_TARGET" cargo test -p gpui-agent --features pixel \
  --test app_pixel venue_builder
```

`headless` is intermittently red for unrelated reasons; run a red test alone
before believing it (`gpui/BUILD.md`); `render-goldens --check` writes nothing
and exits non-zero on drift. The harness is the authority for every UI claim:
extend it with observable roles/actions, never a parallel driver.
