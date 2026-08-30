# Venue graph

A venue stops being a bag of world poses and becomes a **tree of relations**. Every
node stores `(parent, my_socket, their_socket, params)` and *no* world pose. Poses
are derived by walking the tree through the one snap resolver, on load and after
every edit. Moving a truss moves what is bolted to it because there is nothing else
it could do.

Today `stage_pieces` stores `pos_*`/`rot_*` in parent-local space and the relation
that produced the pose — which socket met which — is discarded the moment the drag
ends. That is why the agent can only be handed flattened metres, and why "put a light
on the downstage truss" is not expressible.

## Decisions

- **Live graph, no baked poses, no hybrid.** `solve_snap` is the only thing that
  produces a world transform — builder, renderer, eval, agent. `stage_pieces.pos_*`/
  `rot_*` **go away**, not into a cache: a cache is a second source of truth with no
  way to tell it is stale, and the solve is microseconds (see Performance). The one
  exception is the root's own placement, which is the venue frame, not a pose.
- **The vocabulary is exactly six verbs: attach, extend, array, aim, duplicate
  (+flip), trim.** No `mirror` — not a node kind, not an op. No `join`. No roof or
  ceiling. Symmetry is the builder's job: call the same function twice, or duplicate
  a wing and flip it.
- **A free piece sits on a surface at `(u, v, yaw, trim)`.** `parent = venue.floor`
  or a `stage`, same resolver, same invariants — there is no "unparented" branch to
  test. `(u, v)` is continuous over the surface, `yaw` is its spin, and **`trim` is
  how high it flies**: `trim = 0` sits on the deck, `trim = 6.0` hangs 6 m up.
  Flown is a parameter, not a structure. "Lift" edits `trim`; the whole subtree
  comes along. This `(u, v)` is the **only position-like number in the system**.
  - **The root has two synthesized surfaces, not one.** `floor` is the up-facing
    plane at the venue origin; **`rig` is the same plane facing *down***. Beam is
    the mount normal, so a fixture flown off an up-facing floor would point at the
    ceiling; `rig` is the host `trim` was written for. Same origin, same `(u, v)`,
    same `trim` — only the normal differs. A `stage` deck's own top surface is its
    **`top`** socket, so "on the deck at (u, v)" and "on the floor at (u, v)" are
    the same call with a different host.
  - **`trim` runs along world up, not along the host's normal.** A light on the
    grid rises the same 6 m a light on the deck does, and nobody has to remember
    which way a host faces. `(u, v)` stay in the surface's own plane, which is what
    makes them continuous over it. A perfectly vertical surface has no "higher", so
    there `trim` is inert — the same thing as saying a bolted joint has no lift.
  - **`yaw` is stored as the edge's `roll`.** A socket's freedom *is* a roll about
    the shared normal, and on a surface placement that roll is what the stage
    vocabulary calls yaw. One number, one home: `venue_edges.roll`, clamped at solve
    by the host socket's roll freedom. It is not a param.
- **Origin = mount point** — bottom-centre for anything sitting or hanging, the
  joining socket for anything snapped. Trim and yaw act about it.
- **Beam = mount normal.** A fixture's rest direction is the outward normal of its
  mount socket — floor → up, under-truss → down, truss downstage face → toward the
  house. No per-fixture-type rest axis; pan/tilt are relative to the mount frame.
  `fixture-kinematics` is the single implementation and takes the mount frame as an
  input; renderer, eval (`head_world_position`), and agent bindings all call it.
- **Node kinds are a small closed alphabet.** `venue` (root: floor plus audience
  direction, which is what defines downstage/upstage/stage-left/right), `stage`,
  `run`, `tower`, `piece`, `fixture`, `array`. Nothing else. A new set object is a
  `piece` with sockets, never a new kind.
  - `run` — truss segments with **one `along(t)` over the run's total length**, so
    `run.along(0.25)` means the same thing for one 3 m segment or four. `tower` is
    the same generator, height in 0.5 m steps.
  - `array` — `count`, `span` along a parent feature; children are **derived**, not
    stored rows, and expand at solve time. No "re-solve" step exists, because solving
    is the only way poses exist at all.
  - Params: `u, v, trim` on a surface placement (`yaw` is the edge's `roll`, not a
    param); `length` on an extend; `count, span` on an array; `angle` on a hinge;
    `pan, tilt` on a fixture.
  - An **array node is placed at its anchor** — the seat its `span` is centred on,
    where a single member would sit — and its members are derived from it and name
    it as their parent. The anchor is a pose so the members have a frame to derive
    from and so `array(...)` reports the row the caller placed; it is not a mount.
    **An array is never a parent** (invariant 5): the members hold no rows, so an
    edge could only name the anchor, which would seat one child on every copy
    through the same socket, at a seat with no geometry.
- **Truss is fully procedural — one generator, one family, everything mates.**
  Straight (any span in 0.5 m steps — landed in `gpui/crates/render/src/truss.rs`),
  corner box (2/3/4/5/6-way), hinge (two half-boxes on a pin, 0–180°, where the
  angle is the socket's roll freedom and is draggable), arc later. Ripped truss GLBs
  get deleted; ripped **product** GLBs — decks, CDJs, speakers — stay, they are real
  objects, not parametric structure. Measured: decks are 1×1 m (0.3 m tall) and
  2×1 m (1.0 m tall); the existing truss GLBs are imperial products (1.22 m / 1.83 m
  spans, Q30 at 254 mm, an F34-ish Q40) — which is why the catalog goes metric.
- **Extend casts a ray.** Clicking an open socket — or `extend(socket)` with no
  length — fires a ray along the socket normal.
  - Hits another open compatible socket → the ghost truss defaults to that gap and a
    measurement gizmo runs its length showing the distance. Steps are 0.5 m; feet are
    display-only.
  - Length **equal to the gap** → the piece bridges both sockets. Parent is the
    origin side; the far end is a **constraint** on the other socket, reported
    satisfied / violated / dangling. No node ever gets a second parent.
  - **Less** than the gap → a stub parented to the origin side. **Greater** →
    **refused**, ghost red; that is what stops structure intersecting itself.
  - Ray hits nothing → ghost at 0.5 m, type a length.
- **Snapped pieces have no transform gizmo.** A snapped piece moves only in its
  socket's roll freedom: truss end, none — dragging it drags the whole run; clamp,
  yaw; hinge, its angle. Detaching is explicit — drag past the snap-out radius, or
  use the action. Only free pieces, sitting on the floor or a stage, get the gizmo.
- **Agent API is the same resolver behind a Python facade, in stage vocabulary,
  with no coordinates.** `attach(piece, to=host.feature(...), socket=...)`,
  `extend(socket, length=None)`, `place_on(surface, u, v, yaw=0, trim=0)`,
  `array(...)`, `aim(fixture, at=point|"audience"|"stage_center")`,
  `duplicate(node, to=socket, flip=False)`, `trim(node, height)`. Features are
  named: `stage.corner("downstage_left")`, `truss.face("downstage")`,
  `run.along(0.25)`. Fractions and counts for positions, never metres; metres are
  fine for *lengths* and for `trim`. There is no `set_position`.
- **Every mutating call returns a `Placement` report** — `ok`, `parent`, `warnings`,
  dangling sockets, unsatisfied far-end constraints. Reports **suggest fixes, they do
  not refuse**. Only two hard errors: a type-level socket mismatch, and an extend
  longer than a ray-measured gap.
  - `collisions` (OBB against neighbours) and `span_exceeds` (past the run's end,
    carrying a suggested `extend`) are **deferred, not dropped**. Both need a piece's
    *bounds*, which the socket supply does not carry; shipping them as fields that
    are always empty would be a promise the type does not keep. They arrive with the
    builder that can draw them (phase 4).
  - **A node with no edge is reported, never dropped.** `resolve` returns
    `unplaced`: the root of every branch the walk could not reach, with its
    kind, label and descendant count. A patched, unplaced fixture is the
    ordinary case; `detach` is the other. Silence was the bug — a detached wing
    had no pose, no warning and no mention, so "unplaced" and "deleted" looked
    identical.
  - An **array's open ends are its members'**, not its anchor's. Three trusses
    have three pairs of ends standing in the room; the anchor is a seat with no
    geometry, and reporting it once under-counts by `count - 1`.
  - A **`dangling` socket is one no relation accounts for**: neither half of a joint
    is dangling, and neither is an end a **resolved** far-end constraint checks — a
    constraint is exactly what the builder writes down instead of a second parent.
    Satisfied and violated both account for their ends; a violated end has been
    *measured*, and the gap is reported as itself. A constraint reported `dangling`
    accounts for nothing — its target is unplaced or gone, so it describes no end
    that is in the room, and closing a socket on it would hide the open end behind
    the paperwork meant to explain it. Only self-mating (`Neutral`) joints are
    counted at all; an empty deck top is not an open end.
- **Verification channels, ranked:** `describe(node)` (the tree in stage words) →
  `dangling()` and the `Placement` reports → a top-down quantized tile map ("Gauntlet
  view") → `render(view=fixture.pov)` → `render(view="front")` last. A front render is
  the *worst* signal per token and the most tempting. Humans get the same tools.
- **The stage page is the picture.** An empty venue draws the grid and is already
  buildable — there is no empty-state wall to click through, and no persistent
  toolbar. One `+` opens the add-element dialog in the gpui picker pattern
  (`gpui/crates/app/src/fixture_picker.rs`): element preview on the left, a
  searchable, keyboard-navigable list on the right, catalog pieces and
  patched-but-unplaced fixtures as sections of the one list. Enter enters **place
  mode**.
- **Place mode is the whole builder.** A ghost follows the cursor down the snap
  ladder — socket → surface → grid, hysteresis (snap-in radius strictly smaller than
  snap-out) so a held piece does not chatter. Click places; configure it inline
  where it stands; place mode *stays*, and the next ghost inherits the last piece's
  params, so a row of the same thing is a row of clicks. Esc backs out one level —
  inline edit, then place mode, then nothing held. **No empty game objects** — the
  parent piece *is* the group.
- **Nothing on screen is a label for state.** No "Hand:", no socket list, no
  "distribute onto …". The host is whatever the cursor is over; beads, the
  measurement gizmo and the ghost are the indicators, and they are drawn *in the
  picture*. Controls may live in the 3D view — `scene_desc::Editor` and
  `render/src/overlay.rs` are extended to carry them rather than a chrome panel
  growing a mirror of the scene. The standard is clean, elegant, minimal: if the
  picture can show it, no label also says it, and a control that does not apply to
  the current state is not drawn at all.
- **Holding a fixture, click a truss face, deck edge or floor** and a popover
  anchors in the viewport at that face: count, layout, fixture. Ghost fixtures
  re-solve live as the values change, so the answer is visible before it is
  committed. Apply places them — that is the `distribute` verb (B3); Esc cancels and
  leaves nothing. A fit failure states the needed length inline, with the extend
  action next to it.
- **Selection opens a dismissible inspector sheet** — nudges within the socket's
  roll freedom, trim, warnings and dangling ends. Duplicate, Flip and Detach are
  context-menu actions on the selection, not buttons parked on screen waiting for
  one.
- **Duplicate is how symmetry happens.** Select a wing's root — the piece attaching
  to the stage; the subtree comes along — press ⌘D, or Duplicate from its context
  menu. That puts you in place mode holding the copy: every compatible open socket
  lights up as a snap gizmo, click one and the copy is placed and selected.
  **Flip** inverts the subtree's handedness about its root socket, which is what
  an asymmetric wing needs. Agent: `duplicate(node, to=socket, flip=bool)`.
- **Sockets keep what is golden-tested and gain polarity.** Bbox-anchor authoring
  stays, as do both orientation guards in `snap.ts` (the edge-mode opposing-`outward`
  test, and the parallel-normal self-mating side test that otherwise ties an
  upside-down pose with the correct one). The directed `COMPATIBLE` table is
  **replaced by polarity** — `Male` / `Female` / `Neutral` — plus a roll freedom per
  socket; a thirteen-entry hand-maintained adjacency list is a lookup table
  pretending to be a rule. The catalog moves to Rust as the single copy with a
  generated TS binding: it exists twice today (`src/features/stage/lib/sockets.ts`,
  `gpui/crates/scene/src/sockets.rs`) and the goldens exist because it drifted.

## Two pages: patch and stage

The "Universe" tab is replaced by two pages, split by the question each answers:
what exists, and where it is.

- **Patch page = inventory** — the rental sheet: fixture, mode, universe, address,
  label, N of a fixture added at once. **One allocator, in the backend, per
  universe.** Three frontend first-fit allocators exist today and none of them
  looks at the universe (`add-fixture-dialog.tsx:22-38`,
  `use-fixture-store.ts:625-655`, `:768-790`); they are deleted. Collisions and
  addresses past 512 render red and are **refused by `patch_fixture`** — today
  nothing validates and `engine.rs:429` silently truncates. Address and mode are
  editable; Auto Patch becomes real.
- **Outputs live on the patch page.** Discovered Art-Net nodes bind to universes as
  a **table**, not the `(net<<8)|(sub<<4)|(u&0xF)` arithmetic in `artnet.rs:218`
  that aliases universe 17 onto 1. sACN later.
- **Stage page = the builder** — the venue graph above. A fixture may be patched but
  unplaced; those fixtures are a section of the add-element list, and **that is the
  only place an unplaced fixture is allowed to be** — no more fixtures piled at the
  origin. Placing one snaps it and defaults its label from the location ("Truss L ·
  mover 3"); N of them onto a run is `distribute`.
- **The fixture row splits in two: patch and placement.** Patch is
  `(universe, address, mode, definition)`; placement is a `venue_edges` row like any
  other node. Re-addressing never touches placement, moving never touches the
  address.
- **Groups default from structure** — same truss face is a group — with manual
  groups as an override. The "must group everything before leaving" gate
  (`App.tsx:328`) goes.
- **Build order:** patch page lands with **phase 3** (it needs the split row), stage
  page with **phase 4**. Today `gpui/crates/app/src/universe.rs` is a read-only list
  and the React page cannot edit an address at all — `move_patched_fixture` has zero
  callers.

## Data model

`stage_pieces` is replaced, not extended. New tables (append-only migrations; the old
table's rows are converted by one solve-and-invert pass at migration time, then it is
dropped):

    venue_nodes(id, venue_id, kind, catalog_ref, label, created_at, …)
    venue_edges(child_id PK, parent_id, my_socket, their_socket, roll)  -- roll = yaw
    venue_node_params(node_id, key, value)   -- u, v, yaw, trim, length, count, span, angle
    venue_constraints(node_id, my_socket, target_node, target_socket)

`venue_edges` is keyed by `child_id`, making "exactly one parent" a primary key rather
than a check. `venue_constraints` is a *separate* table on purpose: a far end is a
check, never an edge. `fixtures.pos_*`/`rot_*` follow the same fate — a fixture is a
node with an edge like anything else.

**Invariants**, each enforced where it lives:

1. **Acyclic** — a child's parent must already be reachable from the root; enforced
   in the resolver's insertion order, which is the only writer.
2. **Exactly one parent per node.** A bridging piece has one parent and one far-end
   constraint; **far-end constraints are checks, not edges** — evaluated after the
   solve, reported satisfied / violated / dangling, never participating in it.
3. **Every non-root node has a resolvable socket pair** — polarity-compatible, both
   present on their catalog entries. Checked at edge insert, not at solve.
4. **Deterministic solve order** — depth-first over children sorted by node id, so
   two solves of the same graph produce byte-identical poses. Golden capture depends
   on this.
5. **An array is not a host** — its members are derived at solve time and hold no
   rows, so nothing can be bolted to one. Refused at edge insert, with the anchor
   named; what the builder means by it is an edge per member, and an array is the
   statement that there are no members to name.

## Performance

~500 nodes is the working bound (the largest golden venues are well under 100). One
solve is a depth-first walk with a socket-frame build and a 4×4 multiply per node —
tens of microseconds against a ~16 ms frame. Solve on **every** edit: no incremental
invalidation, no dirty flags. If it ever stops being free, memoize subtree transforms
by node id — but do not build that first.

## Goldens

`harness/goldens/snap*.json` pin `solve_snap` numerically and carry over unchanged —
the resolver's contract is not what is changing. Two new families: `describe()` per
fixture venue (a much tighter diff than a render), and per-node resolved world poses
captured at the end of phase 3 as the migration's proof. Render goldens recapture once
at phase 0, when beam = mount normal moves every rest direction.

## Audit findings this fixes

- Movers rest along `-Z` (`fixture_kinematics::REST_AXIS`), LED bars along `+Y`
  (`luminaire::beam_direction` uses `Vec3::Z` for procedurals), and
  `ask-venue-tool.ts`'s `facingLabel` assumes `+Y` with the **opposite yaw sign** —
  three rest conventions, three sign conventions. `facingLabel` is deleted outright.
- 460 fixtures across the golden venues, exactly one with a non-zero rotation — the
  rest-direction bug is invisible because nobody has ever successfully aimed a
  fixture. That is the symptom, not the mitigation.
- `visualizer.rs::object_pose` builds the gizmo rotation with `coords::euler_xyz(...)`
  — the stored triple *without* the `(x, z, y)` swap `coords::three_pose_from_data`
  exists to perform, so gizmo edits on a rotated object land in a mirrored frame.
- `stage_render.rs` says so itself: a copy of the visualizer's `scene`,
  `flatten_pieces`, `local_matrix`, `definition`, `lookup`, `meshes_root`. The graph
  resolver replaces both copies.

## Build order

0. **`fixture-kinematics` consolidation + beam = mount normal.** `Mount` takes a
   mount frame instead of a stored Euler triple; `head_world_position`,
   `luminaire::beam_direction`, and the venue bindings route through it. `facingLabel`
   deleted, render goldens recaptured. *Nothing downstream is trustworthy until this
   lands.*
1. **Gizmo space fix** — `object_pose` through `coords::three_pose_from_data`; delete
   the duplicate drawer (`visualizer.rs::install_translation_gizmo` /
   `install_rotation_gizmo` strip and replace the correct pivot-aware overlay in
   `render/src/overlay.rs`); keep overlay.rs, extend it to rotate and stage pieces.
2. **Procedural truss + socket catalog to Rust.** Corner box (2–6 way) and hinge join
   the landed straight generator; truss GLBs deleted, catalog metric. One catalog copy
   with a generated TS binding; polarity replaces `COMPATIBLE`; roll freedom per
   socket, hinge angle included. Goldens must still pass.
3. **Graph model + resolver-on-load + persistence** — the four tables, the migration
   pass, `flatten_pieces` and its copy deleted. This is the load-bearing phase.
4. **gpui builder** — `+` → picker → place mode, ghost down the snap ladder with
   hysteresis and inherited params; the face popover with live-re-solving ghosts;
   click-a-socket extend with the ray, gap default, and measurement gizmo; duplicate
   and flip from the selection's context menu; trim (lift) on floor and stage
   placements; no gizmo on snapped pieces — roll-freedom drag instead.
5. **Python facade** — `attach`/`extend`/`place_on`/`array`/`aim`/`duplicate`/`trim`,
   `describe`, `Placement` reports, `dangling()`.
6. **Tile map + POV render.**

## Open

- **Does the React app get the new model, or is it frozen?** The gpui builder is the
  one being invested in; keeping `src/features/stage` alive means porting polarity,
  the resolver, and the builder twice. Freezing it means the React visualizer renders
  venues it cannot edit.
- **`run.along(t)` across a mitred corner.** Arc length through a corner block is not
  the sum of segment lengths, and `along(0.5)` on an L-shaped run should probably
  mean half the *walked* distance. Unresolved.
