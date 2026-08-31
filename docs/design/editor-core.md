# Editor core

The stage builder is a small game-editor kernel, and this document is its
contract. It exists because the first builder grew as UI garnish on the solver
— four parallel spatial systems, selection by frame index, input hardcoded per
mode — and every one of those seams produced a user-visible bug. The rules
here are what the 2026-08-30 rebuild enforces; anything that violates one is a
regression, not a style choice.

## Identity

**A thing's name is its venue-graph node id, everywhere.** `EditorObject`
(fixture id / piece id) is the selection's element type, the pick result, the
highlight key, and the builder's `selected`. Nothing user-facing holds an
index into a frame, a draw list, or a scene graph — an index names whatever
inherited the slot after a re-solve, which is how deleting a piece used to
leave the selection pointing at some other object.

Corollary: when a scene reloads, `Selection::retain` drops names the new scene
no longer has. Selection survives a re-solve exactly when the object does.

## One pick, many askers

`PickSnapshot` (the BVH over the displayed frame) answers every "what is under
the pointer" question: click select, marquee anchors, gizmo grab, hover, and
the placement ray (`Visualizer::stage_cursor`). The element layer's beads are
*aiming affordances for named sockets*, not a second pick system — their
hitboxes exist because a raised socket is metres from where the cursor's ray
lands, never because the room is otherwise unclickable.

A face hit resolves to the piece's **named** face socket
(`Room::face_socket_for`), because the graph's edges name sockets: a landing
found by pointing must be committed by the same verb as one found by a bead.
No virtual sockets reach the graph.

## Input: camera under, tool over

The viewport listener owns all pointer routing. The camera is always
reachable: left-*drag* orbits, right pans, middle dollies, in every mode —
place mode included. The active mode owns only the left *click's meaning*
(`ReleaseAct`): a placing hand's click is aim-and-drop; a finished gizmo drag
is a pose commit; otherwise a click is a pick. No mode mounts an occluder over
the room to steal the pointer.

Escape is handled at the shell's dismissal ladder (`dismiss_overlay`), so it
works wherever focus is; the one exception is a focused text field, whose own
key handler forwards it. Mode keys (`W`/`E`, Delete) are stage-scoped with the
text exclusion.

## The gizmo tells the truth

The widget is drawn exactly where it can act, and every drag it accepts is
committed. A **free piece**'s drag previews by writing the scene pose and
commits on release by inverting the mate (`invert_placement` → `set_params`
with `u,v,yaw,trim`) — the re-solve hands back the same numbers. A snapped
piece gets no widget (its pose is a relation); its one freedom is the
inspector's. The mode pair (Translate/Rotate) renders iff the widget does.

*Known debt:* a selected **fixture** still wears a gizmo whose drag writes a
scene pose the graph discards on the next re-solve. The honest end state is no
fixture gizmo — a clamped fixture slides in `u` — but removing it is a product
change to the patch page tested by `visualizer_gizmo`, deferred deliberately.

## Validity is the solver's

A landing that would pass a piece through placed structure is **refused** (red
ghost, drop inert) before it can be committed. The test is OBB-vs-OBB
(`luma_scene::aabb::obb_intersects`, 15-axis SAT) over per-piece local bounds:
the catalog's measured GLB box for mesh pieces, `procedural_bounds` for
generated ones. Contact is not collision — boxes shrink by 2 cm so flush mates
stay legal — and the landing's own host is skipped, since a mate touches its
host by construction. Fixtures carry no box: a light is inventory, not
structure.

## Frames follow facts

The idle gate re-presents the last frame only while nothing changed. Anything
that changes what a frame would show must reach the `IdleKey` or kill the
`idle` slot: a reloaded scene does the latter (`rig_loaded`), pointer-driven
modes hold the gate open via `interacting`. Never enumerate a new kind of
change into "it'll redraw eventually" — that is the deleted-piece-stays-drawn
bug, and it will pass every logic test while looking broken.

## Affordances live with their depth

In-scene marks are depth-tested with a faint x-ray pass (beads), so occlusion
reads truthfully without losing the joint. Chrome that is *about the window*
(the add-element dialog) anchors to the window, not to the page's box.
