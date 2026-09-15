# Graph editor interaction architecture

**Status:** built. This doc describes how the native graph canvas
(`gpui/crates/app/src/graph.rs`) routes input, edits a graph and previews it.
The agreed behavior and its limits are in
[graph-editor-redesign.md](graph-editor-redesign.md). Section numbers are
cited from code. Keep them stable.

## 1. Shape

- `Editor` holds a score graph view (`graph/score.rs`), the node catalogue, a
  `Scene`, a selection set, the current gesture and a viewport.
- `Scene` is resolved geometry. `Scene::build` runs when the document
  changes. `Scene::measure` runs once per rebuild, inside prepaint, because
  card widths need shaped text.
- Links store card and port indices. Moving a card moves its wires.
- One `canvas()` element paints the ground, the wires and the cards. Cards
  outside the viewport are culled.
- The timeline owns the editable score and its history. A graph tab edits a
  draft of that score.

## 2. Interaction routing on a painted canvas

The canvas has one hitbox and routes input itself. `Scene::hit(point, zoom)`
returns `Port`, `Header`, `Body`, `Wire` or `Empty`. It tests cards
topmost-first, then the regions of that card. It tests wires only when nothing
else is hit. `Scene::measure` computes the regions, so a frame does no hit-tree
work.

Real gpui elements per card are rejected, for three reasons:

1. Zoom would re-lay-out the element tree on every zoom frame.
2. A large graph would need thousands of layout nodes per frame.
3. Culling would create and drop entities during a pan, which loses focus.

Floating surfaces, such as node search, are real elements over the canvas.
They must `.occlude()`, or a press on them also reaches the canvas. The track
editor uses the same pattern.

`gpui/crates/agent/tests/app_pixel/graph_budget.rs` measures the pan and zoom
frame cost. That budget is the contract this section protects.

## 3. Value editing

Cards carry names and sockets, not value widgets. The inspector
(`graph/controls.rs`) edits values with the shared `luma_ui::arg` widgets:
number, color, gradient, envelope, mapping and signal.

## 4. Edits and wires: one write path

Every graph mutation is a `luma_patterns::GraphEdit`, applied by
`Definition::edit` (`backend/crates/patterns/src/edit.rs`). Canvas gestures,
undo and programmatic authors use the same set. The rules live there:

- type and rate compatibility at the wire,
- adding, renaming and removing Inputs,
- Output bindings,
- node ids.

A refused edit leaves the draft unchanged. Saving validates the whole score.

Compatibility is one predicate with two readers. `Definition::edit` refuses an
illegal wire. The canvas asks the same question to highlight targets during a
drag (`graph/interaction.rs`). The UI never pre-checks on its own.

`backend/src/models/node_graph/edit.rs` keeps the `Edit` / `apply` vocabulary
for the older untyped graph model. The GPUI editor does not call it.

Wire gestures:

- A press on a socket starts a wire drag.
- Valid targets highlight. An invalid target explains the refusal.
- A release on a valid socket binds it. A release anywhere else cancels.
- A failed drop keeps the existing bindings.
- Edges have stable selection identities. Delete disconnects them.

## 5. Node search

Right-click on the canvas, or Space, opens node search at the pointer
(`float::anchored_at`). Enter inserts the node at the original graph position.
The list uses the shared `float::Picker`, which chat history, the group
expression editor and the model picker also use.

## 6. Track context and preview

A graph tab always has a `TrackContext` (track and venue). A door that cannot
resolve one is inert and shows the reason (`patterns.rs`). Tab identity stays
keyed on the graph, so one document has one writer. Opening the same graph
from another track changes the context of the existing tab.

The visualizer plays one compiled clip (`graph/preview.rs`). The score
supplies timing, selection, seed and argument overrides. The cursor and the
loop setting are temporary. Inspect signals samples the compiled clip once
when the preview is prepared.

## 7. Undo

The timeline's `History<S>` (`gpui/crates/app/src/history.rs`) holds score
snapshots and graph draft snapshots. Each node insertion, Input insertion and
connection is one step. Undo can return to an incomplete graph without saving
an invalid score. Closing and reopening a graph tab restores its draft and
redo branch while the timeline is open.

## 8. Selection

The selection is a set. Shift-click adds or removes a node. A drag on empty
canvas draws a marquee (`Gesture::Marquee`). Delete and Backspace delete the
selected nodes.

## 9. Rulings

1. **A graph tab always has a track context.** Doors with no track are inert
   with a reason. The empty "waiting for data" state does not exist.
2. **Space opens node search** in `Graph && !TextInput`.
3. **Marquee selection ships with shift-click.**
4. **`ParamDef` has `range: Option<(f32, f32)>`** in the backend catalogue
   (`backend/src/models/node_graph.rs`). The view has no hardcoded ranges.

## 10. Single owners

Each graph contract has one owner. Callers use it and do not copy it.

| Contract | Owner |
|---|---|
| wire compatibility | `compatible` in `backend/crates/patterns/src/edit.rs`, read by `Definition::edit` and by the canvas drag preview |
| Input, Output and node edit rules | `Definition::edit` |
| query, rows and cursor for pickers | `luma_ui::float::Picker` |
| popups anchored at a point | `luma_ui::float::anchored_at` |
| undo stack | `History<S>` in `gpui/crates/app/src/history.rs` |
| slider ranges | `ParamDef::range` |
| agent instrumentation labels | the shared control constructors register their own label |
