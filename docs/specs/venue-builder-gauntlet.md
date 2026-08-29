# Venue builder gauntlet

**Status:** acceptance contract for `docs/design/venue-graph.md` phases 3b–6.
That doc decides; this one says what evidence proves each decision landed, and a
surface disagreeing with it loses. **Meta-goal:** a rig is *built*, never typed.
The human places structure on the stage page and fixes paperwork on the patch
page; the agent calls the same resolver through a Python facade, same
vocabulary. One machine, provably.

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

B1/B2 sit on phase 3b's tables, in parallel; B3 needs both, since one command
places, names, groups and patches; B7 needs B6's `describe()`.

## 3. B1 — addressing

**Scope.** One backend allocator. One universe per run: consecutive addresses in
physical order along the run (`along(t)` ascending, ties by node id), rolling to
the next free universe when the run's footprint would cross 512. Collisions and
any footprint past 512 are **refused** by `patch_fixture`, never truncated.
Patch-page edits override; auto-patch re-derives from placement, dropping
overrides for fixtures it touches and reporting how many moved. *Prior art:*
Vectorworks Spotlight addresses by plot position, not creation order; real rigs
run a universe per truss, bounding a fault to one structure. **Out of scope:**
Art-Net binding (B5), sACN, RDM, any UI.

**Evidence.** `cargo test -p luma-scene` over the allocator;
`harness/goldens/patch-allocation.json` holding, per golden venue, the `(node,
universe, address, footprint)` table; an overflow crossing 512 and a refusal for
a hand-set collision. The critic reads it against run order and greps for stray
first-fit loops. **Acceptance.** Reordering fixtures along a run and re-patching
changes addresses to match, with no literal start address asserted. An
overflowing run lands wholly in the next universe, and says so. A refused patch
leaves the database unchanged and names the conflict. Deleting the three
frontend first-fit allocators is part of the diff. **Applies:** AF1, AF3, AF5,
AF10.

## 4. B2 — grouping

**Scope.** Groups are **derived sets shown as a tree**; a fixture may be in
several. One derivation: role (wash / spot / beam / strobe / blinder / pixel /
fx, from the QLC+ `Type` plus channels, `src-tauri/src/models/fixtures.rs`) →
class (`horizontal`/`vertical` for a run standing on its own, `left wing`/`right
wing` for one bolted to a stage, by the run's *attachment* side against the
stage's resolved surface centre) → row (one per distribution, never merged,
named by the structure piece's label, else — for a pair — by the axis it
measurably separates on, else `row n`) → split (the halves a row's fixtures fall
into where they measurably separate: `top`/`bottom` by height, `left`/`right`
across, `downstage`/`upstage` by depth) → cross-cuts (one split name unioned
across the role).

**Measured, never ranked.** A positional name is claimed only where there is a
gap to claim it with: the widest gap between neighbours must beat the spacing
around it, and never counts below a documented absolute — both sides of that
threshold tested. Two unlabelled rows abreast at one trim are `left` and
`right`, not a "top" and a "bottom"; rows that separate on nothing are numbered.
An evenly spaced run does not split at all. A class holding one unlabelled row
collapses that level, because the class node already *is* that set: its children
are the row's splits. A cross-cut is emitted only where two or more **leaves**
carry the name — a row that split contributes its halves, not itself.

Names are model plus a running number per model (`aura 1`…`aura 8`). Manual
rename/move/merge sit on top as overrides; **a touched node is never
re-derived**, and no edit may mint a selection name another node in the venue
already answers to — derived and authored groups share one namespace. **Out of
scope:** groups in the pattern editor or scores.

**Evidence.** Three canonical venues captured verbatim in
`harness/goldens/venue-groups.json`. The critic rebuilds each mirrored and
confirms the same *sets*, path by path and member by member — equal path lists
prove only that the vocabulary survived. The `describe()` golden lives in B6,
which is where `describe()` is built; asking for it here would pin an interface
this round does not own.

    (a) led bars / horizontal / {top, bottom}
        led bars / vertical   / {left, right}
    (b) spots / left wing  / {top, bottom}
        spots / right wing / {top, bottom}
        spots / top
        spots / bottom
    (c) spots / left wing  / {downstage, upstage} / {top, bottom}
        spots / right wing / {downstage, upstage} / {top, bottom}
        spots / top
        spots / bottom

(a) is four evenly spaced runs around a wall: two rows named by the height
between them, two by the width, and no split inside any of them. (b) is the
original rig — one tower a side, spots at two heights: each wing holds one
unlabelled row, so the wing *is* that row and its children are the splits. (c)
is two towers a side, one behind the other and hung at different heights: depth
separates them further than trim does, so depth is what names them, and it is
the halves rather than the rows that the role's cross-cuts gather.

**Acceptance.** Those trees are produced exactly, node for node, name for name,
compared as trees not counts. Moving a truss changes the split with no group
edit. Renaming a derived node then adding fixtures keeps the name and still
files the new ones under it. A name that collides with anything else in the
venue is refused, in both directions. No graph write leaves a stale derived
answer behind, including one read between the write and its commit. One
derivation function, no copy in the page or facade. **Applies:** AF1, AF3, AF10.

## 5. B3 — the distribute command

**Scope.** One command, one transaction: `distribute(feature, fixture, count,
spacing|span)` places, names, groups and patches — the only fixture constructor
besides the patch page's non-placed add. Fit failure reports the *needed* length
("needs 4.0 m, run is 3.0 m — extend?") and places nothing; never silently
drops, clips or overlaps. **Out of scope:** the popup that calls it, the tray.

**Evidence.** `cargo test -p luma-scene`: exact fit, `span` and `spacing ×
count` each longer than the run, count 1, count 0; a golden of the resulting
`venue_nodes/edges/params` rows. The critic checks names, groups and addresses
come from B1/B2. **Acceptance.** The failure report's stated need matches the
length that, once extended, makes the same call succeed — measured by calling
`extend` with that number. A failed distribute leaves zero new rows. Two
distributions on one run interleave in physical, not creation, order.
**Applies:** AF1, AF3, AF5, AF9, AF10.

## 6. B4 — stage page

**Scope.** Palette; placement with ghost and the snap ladder socket → surface →
grid, hysteresis on snap-out; click an open socket → ray → ghost defaulting to
the measured gap with a live measurement gizmo, shorter is a stub, longer is
refused with a red ghost; ⌘D duplicate with every compatible open socket lit as
a snap gizmo, plus flip; trim (lift) on floor and stage placements;
slide-along-face and drag-off-to-detach for a placed fixture; **no transform
gizmo on snapped pieces** — roll freedom only; a truss-face, stage-edge or floor
click opens the distribution popup (fixture, count, spacing or span) calling B3;
a tray of unplaced fixtures. **Out of scope:** addresses, universes, modes.

**Evidence.** `tests/headless/venue_builder.rs` (add its `mod` line to
`tests/headless/main.rs`) drives palette → drag → snap → extend → duplicate →
distribute through the tree and `app.painted()`; the `CAMERA` readout
(`app/src/visualizer.rs:3300`) pins that build gestures never move the camera.
Pixels in `app_pixel/venue_builder_pixels.rs`: ghost accepted, ghost refused
(red), measurement gizmo, duplicate snap gizmos, distribution popup.
**Acceptance.** A snapped piece exposes no translate/rotate gizmo in the node
tree, and a drag on it moves the run — asserted on the resolved parent pose.
Extend past the gap never commits: the refusal shows in `painted()`, no node is
added. Extend equal to the gap creates one edge plus one far-end constraint that
`dangling()` reports satisfied. ⌘D + flip on an asymmetric wing yields a subtree
whose `describe()` matches the hand-built opposite wing. Trim moves the whole
subtree; the tray empties on place, refills on drag-off; reduced motion snaps
ghosts and popups. **Applies:** AF1–AF6, AF8, AF9.

## 7. B5 — patch page

**Scope.** Editable inventory table (fixture, mode, universe, address, label,
group, output); a per-universe footprint strip with collisions in red; Auto
Patch calling B1; outputs as a **table** binding discovered Art-Net nodes to
universes, replacing the `(net<<8)|(sub<<4)|(u&0xF)` arithmetic; an add-N dialog
morphing from picker to count/mode in the shared dialog container. **Out of
scope:** anything positional — no u/v/yaw/trim column, no "move to truss", no
drag between structures.

**Evidence.** `tests/headless/venue_patch.rs`; pixel captures of the footprint
strip with and without a collision and of both add-N dialog states; an
`app.painted()` sweep proving the picker→count morph never flashes an empty
table. **Acceptance.** A colliding address edit is refused, shows red, and
leaves the stored value unchanged. Auto Patch after a stage-page move re-derives
that address and changes nothing about placement. Universes 17 and 1 resolve to
different output rows — asserted on the resolution, not the formula. Adding N
unplaced fixtures puts N rows in the tray and no nodes in the graph. No path
here writes `venue_edges` or a placement param. **Applies:** AF1, AF3, AF5, AF8,
AF9, AF10.

## 8. B6 — Python facade

**Scope.** `attach`, `extend`, `array`, `aim`, `duplicate(+flip)`, `trim`,
`describe`, `dangling`, `Placement` — the six verbs plus the two verification
channels, in stage vocabulary, no coordinates, no `set_position`. Same resolver,
allocator and grouping as the pages. **Out of scope:** new verbs; a convenience
that is a second spelling of an existing verb is AF6.

**Evidence.** One test builds a rig twice — by script through the facade, and by
the harness driving B4/B5 — then diffs `venue_nodes`, `venue_edges`,
`venue_node_params` and `describe()`. The rig holds a wing, a duplicate+flip, a
distribution, an extend-to-gap and a trim. **Acceptance.** The two rigs produce
**identical rows** (node ids normalised by tree position) and **identical
`describe()`**. Every mutating call returns a `Placement`; the only hard errors
are a socket-type mismatch and an extend past the measured gap — everything else
warns and places, each warning path reachable in tests. `dangling()` names the
unsatisfied far-end constraint the test leaves open. **Applies:** AF1, AF3, AF5,
AF6, AF8, AF9, AF10.

## 9. B7 — tile map and POV render

**Scope.** Top-down quantized tile map ("Gauntlet view") and
`render(view=fixture.pov)`, in that order of preference; front render last, per
the design's ranking. **Evidence.** Golden tile maps per canonical venue in
`harness/goldens/venue-tiles.json` (text, diffable); POV captures under
`harness/goldens/scenes-wgpu/` via `render-goldens`, each with its descriptor.
**Acceptance.** The tile map tells the two canonical venues apart at a glance;
its diff localises a one-piece move to one tile row. A fixture POV shows what
its mount normal points at — changing `aim` changes the frame, compared as
frames not matrices. No new per-frame cost above the existing visualizer
budgets. **Applies:** AF1, AF2, AF7.

## 10. Verification matrix

Every accepted round runs its narrow tests plus:

```sh
cd gpui
cargo fmt --all -- --check
cargo check -p gpui-agent --all-targets        # headless tree; no --features
cargo clippy --workspace --all-targets
cargo test -p luma-scene
cargo test -p luma-render
cargo test --test headless                     # append a name to filter a file
cargo run -p luma-render --release --bin render-goldens -- --check

cargo test --manifest-path ../src-tauri/Cargo.toml

# Pixel rounds get the second tree — never flip features in the headless one.
# LUMA_SHOTS=dir … --release -- --ignored --nocapture keeps the shots.
export PIXEL_TARGET="$(git rev-parse --show-toplevel)/gpui/target-pixel"
CARGO_TARGET_DIR="$PIXEL_TARGET" cargo test -p gpui-agent --features pixel \
  --test app_pixel venue_builder
```

`headless` is intermittently red for reasons unrelated to any diff; run a red
test alone before believing it (`gpui/BUILD.md`). `render-goldens --check`
writes nothing and exits non-zero on drift; capture is the same binary without
the flag. The harness is the authority for every UI claim; extend it with
observable roles/actions, never a parallel driver.
