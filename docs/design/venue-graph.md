# Venue graph

A venue stops being a bag of world poses and becomes a **tree of relations**. Every
node stores `(parent, my_socket, their_socket, params)` and *no* world pose. Poses
are derived by walking the tree through the one snap resolver, on load and after
every edit. Moving a truss moves what is bolted to it because there is nothing else
it could do.

Today, `stage_pieces` stores `pos_*`/`rot_*` in parent-local space, `flatten_pieces`
composes the chain at read time, and the relation that produced the pose — which
socket met which — is thrown away the moment the drag ends. That is why the agent
can only be handed flattened metres, and why "put a light on the downstage truss" is
not expressible.

## Decisions

- **Live graph, no baked poses, no hybrid.** `solve_snap` is the only thing that
  produces a world transform, for the builder, the renderer, eval, and the agent.
  `stage_pieces.pos_*`/`rot_*` **go away** — not a cache. A cache would be a second
  source of truth with no way to tell it is stale, and the solve is microseconds
  (see Performance). The one exception is the root's own placement, which is the
  venue frame and therefore not a pose at all.
- **A free-floating piece is not a special case.** It is a child of the venue floor
  at a grid cell: `parent = venue.floor`, `their_socket = grid(i, j)`, same resolver,
  same invariants. There is no "unparented" branch to test.
- **Beam = mount normal.** A fixture's rest direction is the outward normal of the
  socket it is mounted on. Floor socket → up. Under-truss socket → down. Truss
  downstage face → toward the house. There is no per-fixture-type rest axis, and
  pan/tilt are relative to the mount frame. `fixture-kinematics` becomes the single
  implementation and takes the mount frame as an input; the renderer, eval
  (`head_world_position`), and the agent bindings all call it.
- **Node kinds are a small closed alphabet.** `venue` (root: floor plus audience
  direction, which is what defines downstage/upstage/stage-left/right), `stage`,
  `run`, `tower`, `piece`, `fixture`, `array`, `mirror`. Nothing else. A new set
  object is a `piece` with sockets, never a new kind.
  - `run` — a chain of truss segments with **one `along(t)` parametrization over the
    run's total length**, so `run.along(0.25)` means the same thing whether the run
    is one 3 m segment or four. `extend(run, to=12.0)` fits catalog segment lengths
    with a shape-grammar fit (`[C],[S]*,[C]` — corner, spans, corner, in the manner
    of Unreal PCG splines), not by scaling a mesh.
  - `tower` — height in catalog increments, same fit.
  - `array` — `count`, `span`, along a parent feature; children are **derived**, not
    stored rows.
  - `mirror` — an `axis`; the subtree is derived.
  - Arrays and mirrors are ordinary nodes. There is no "re-solve" step, because
    solving is the only way poses exist at all.
- **Agent API is the same resolver behind a Python facade, in stage vocabulary,
  with no coordinates.** `attach(piece, to=host.feature(...), socket=...)`,
  `extend`, `place_on(surface, u, v)`, `array`, `mirror`,
  `aim(fixture, at=point|"audience"|"stage_center")`. Features are named:
  `stage.corner("downstage_left")`, `truss.face("downstage")`, `run.along(0.25)`.
  Fractions and counts for positions, never metres; metres are fine for *lengths*.
  There is no `set_position`.
- **Every mutating call returns a `Placement` report** — `ok`, `parent`, `warnings`,
  `collisions` (OBB against neighbours), `span_exceeds` (an array or piece past the
  run's end, carrying a suggested `extend`), dangling sockets, unsupported spans.
  Reports **suggest fixes, they do not refuse**. The single class of hard error is a
  type-level socket mismatch, which is a category error and not a judgement call.
- **Verification channels, ranked:** `describe(node)` (the tree read back in stage
  words) → `dangling()` and the `Placement` reports → a top-down quantized tile map
  ("Gauntlet view") → `render(view=fixture.pov)` → `render(view="front")` last. A
  front render is the *worst* verification signal per token and the most tempting.
  Humans get the same tools; there is no agent-only diagnostic.
- **Human UX is Roblox-simple.** The palette plus the snap ladder (socket → surface →
  grid) is the whole builder; the gizmo is an escape hatch, not the primary verb.
  Drag a truss near a stage corner: it snaps and stands up. Drag a light near a
  truss: it sticks to the face and points out of it. Ghost preview while dragging,
  commit on release, hysteresis (snap-in radius strictly smaller than snap-out) so a
  held piece does not chatter at the boundary. Copy and mirror operate on subtrees.
  **No empty game objects** — the parent piece *is* the group.
- **Sockets keep what is golden-tested and gain polarity.** Bbox-anchor authoring
  stays. Both orientation guards in `snap.ts` stay: the edge-mode opposing-`outward`
  test, and the parallel-normal self-mating side test that otherwise ties an
  upside-down pose with the correct one. The directed `COMPATIBLE` table is
  **replaced by polarity** — `Male` / `Female` / `Neutral` — plus a roll freedom per
  socket. A thirteen-entry hand-maintained adjacency list is a lookup table
  pretending to be a rule. The catalog moves to Rust as the single copy with a
  generated TS binding; it exists twice today
  (`src/features/stage/lib/sockets.ts`, `gpui/crates/scene/src/sockets.rs`) and the
  goldens exist precisely because it drifted.

## Data model

`stage_pieces` is replaced, not extended. New tables (append-only migrations; the old
table's rows are converted by one solve-and-invert pass at migration time, then it is
dropped):

    venue_nodes(id, venue_id, kind, catalog_ref, label, created_at, …)
    venue_edges(child_id PK, parent_id, my_socket, their_socket, roll)
    venue_node_params(node_id, key, value)   -- count, span, axis, length, u, v

`venue_edges` is keyed by `child_id`, which makes "exactly one parent" a primary key
rather than a check. `fixtures.pos_*`/`rot_*` follow the same fate: a fixture is a
node with an edge like anything else.

**Invariants**, each enforced where it lives:

1. **Acyclic** — a child's parent must already be reachable from the root; enforced
   in the resolver's insertion order, which is the only writer.
2. **Every non-root node has a resolvable socket pair** — polarity-compatible, both
   present on their catalog entries. Checked at edge insert, not at solve.
3. **Deterministic solve order** — depth-first over children sorted by node id, so
   two solves of the same graph produce byte-identical poses. Golden capture depends
   on this.

## Performance

~500 nodes is the working bound (the largest golden venues are well under 100). One
solve is a depth-first walk with a socket-frame build and a 4×4 multiply per node —
tens of microseconds, versus the ~16 ms frame. Solve on **every** edit, no
incremental invalidation, no dirty flags. Arrays expand at solve time, so a 64-cell
array is 64 matrix multiplies and zero rows. If this ever stops being free, the fix
is memoizing subtree transforms by node id — but do not build that first.

## Goldens

`harness/goldens/snap*.json` pin `solve_snap` numerically and carry over unchanged —
the resolver's contract is not what is changing. Two new families: `describe()`
output per fixture venue (a much tighter diff than a render), and per-node resolved
world poses, captured at the end of phase 3 as the migration's proof. Render goldens
recapture once, at phase 0, when beam = mount normal moves every rest direction.

## Audit findings this fixes

- Movers rest along `-Z` (`fixture_kinematics::REST_AXIS`), LED bars along `+Y`
  (`luminaire::beam_direction` uses `Vec3::Z` for procedurals), and
  `ask-venue-tool.ts`'s `facingLabel` assumes `+Y` with the **opposite yaw sign** —
  three rest conventions, three sign conventions. `facingLabel` is deleted outright.
- 460 fixtures across the golden venues, exactly one with a non-zero rotation. The
  rest-direction bug is invisible today because nobody has ever successfully aimed a
  fixture — the symptom, not the mitigation.
- `visualizer.rs::object_pose` builds the gizmo rotation with
  `coords::euler_xyz(rot[0], rot[1], rot[2])` — the stored triple applied *without*
  the `(x, z, y)` swap that `coords::three_pose_from_data` exists to perform. Gizmo
  edits on a rotated object are therefore made in a mirrored frame and land wrong.
- `stage_render.rs` says so itself: it is a copy of the visualizer's `scene`,
  `flatten_pieces`, `local_matrix`, `definition`, `lookup` and `meshes_root`. The
  graph resolver replaces both copies.

## Build order

0. **`fixture-kinematics` consolidation + beam = mount normal.** `Mount` takes a
   mount frame instead of a stored Euler triple. `head_world_position`,
   `luminaire::beam_direction`, and the venue bindings all route through it.
   `facingLabel` deleted. Recapture render goldens. *Nothing downstream is
   trustworthy until this lands.*
1. **Gizmo space fix** — `object_pose` through `coords::three_pose_from_data`; delete
   the duplicate drawer — `visualizer.rs::install_translation_gizmo` /
   `install_rotation_gizmo` strip and replace the correct pivot-aware overlay in
   `render/src/overlay.rs`; keep overlay.rs, extend it to rotate and stage pieces.
2. **Socket catalog to Rust**, one copy, generated TS binding; polarity replaces
   `COMPATIBLE`; roll freedom per socket. Goldens must still pass.
3. **Graph model + resolver-on-load + persistence** — the three tables, the migration
   pass, `flatten_pieces` and its copy deleted. This is the load-bearing phase.
4. **gpui palette / ghost / snap-on-drag**, with hysteresis.
5. **Python facade** — `attach`/`extend`/`place_on`/`array`/`mirror`/`aim`,
   `describe`, `Placement` reports, `dangling()`.
6. **Tile map + POV render.**

## Open

- **How is a hand-dragged free piece's grid cell chosen?** Nearest cell to the drop
  point is obvious but makes a 5 cm nudge a discrete jump. Sub-cell `(u, v)` on the
  floor socket avoids that but reintroduces a continuous coordinate through the back
  door.
- **Does the React app get the new model, or is it frozen?** The gpui builder is the
  one being invested in; keeping `src/features/stage` alive means porting polarity,
  the resolver, and the palette twice. Freezing it means the React visualizer renders
  venues it cannot edit.
- **`run.along(t)` across a mitred corner.** Arc length through a corner block is not
  the sum of segment lengths, and `along(0.5)` on an L-shaped run should probably
  mean half the *walked* distance. Unresolved.
- **Does `mirror` mirror fixture aim?** A mirrored mover that keeps its pan sign
  points the wrong way; one that flips it breaks any pattern authored against pan.
- **Catalog fit failure.** `extend(run, to=7.3)` with no segment combination summing
  to 7.3 — nearest-under, nearest-over, or a report with both?
