# Graph editor interaction architecture

How `crates/app/src/graph.rs` becomes a pattern editor instead of a pattern
viewer. Scope: hit routing, param editing, wire creation, the add-node palette,
live preview, undo, and the order to build them in.

Reference semantics come from `src/shared/lib/react-flow-editor.tsx`,
`src/shared/lib/react-flow/`, and `src/features/patterns/components/pattern-editor.tsx`.
Where this design departs from the web, it says so and says why.

---

## 1. Current state

`graph.rs` (2427 lines) is a custom-painted canvas over the authored graph
document.

**Shape.** `Editor` holds the pattern, the node catalogue (`types`), the
`Document` (implementation id + revision + `Graph`), a `Scene`, a selection, a
`Gesture`, and a `Viewport`. `Scene` is *resolved geometry*: `cards` (origin,
width, height, `body_top`, ports with card-local anchors, a `Body`), `links`
stored as `(card_idx, port_idx)` pairs so moving a card moves its wires for
free, and a `measured` flag.

**The two-phase geometry rule.** `Scene::build` (`graph.rs:901`) runs on document
change and produces everything that does not need a text system.
`Scene::measure` (`graph.rs:1015`) runs *once* per rebuild, inside prepaint,
where a `Window` is in hand, and resolves every width that depends on shaped
text. This is load-bearing: the web card is shrink-to-fit, so a card's width is
a function of shaped glyphs, and a wire lands in the wrong place if you guess.

**Rendering.** One `canvas()` element (`graph.rs:1602`). Prepaint writes
`origin`, runs the one-shot measure, does `fitView`, registers one
`agent_paint_node(Role::Card, …)` per card, and returns a single hitbox.
Paint draws ground → wires → cards, culling cards to the viewport, and drops
text below `LABEL_FLOOR = 0.3` because shaping is the dominant cost.

**Interaction.** `listen()` (`graph.rs:1660`) registers four window-level mouse
handlers. Press and scroll are gated on the hitbox; move and release are
deliberately not, so a drag that wanders off the canvas keeps tracking.
`card_at` (`graph.rs:292`) hit-tests whole card rects, topmost-first.
`Gesture` is `Pan { last }` or `Move { node, grab, moved }`.

**Writes.** `save_graph` (`graph.rs:535`) is whole-document CAS through
`Library::save_pattern_graph` → `save_pattern_graph_document`, with a per-edit
`operation_id`, `base_revision`, one write in flight at a time, a `dirty` flag
for edits made mid-flight, and conflict → reload + a named error.

**Node bodies are pictures.** `Body` is a closed four-variant vocabulary
(`None`, `Params`, `Notice`, `Plot`, `Falloff`). `paint_slider`
(`graph.rs:2320`), `paint_control`, `paint_chevron` draw *pictures* of
`luma_slider` and `<Selector>`. Nothing has an `on_change`.

**Preview.** `ViewData` is a global keyed by node id, snapshotted into
`Editor::views` when the screen opens. Nothing in the gpui host publishes to it —
the only publisher today is `tests/app_pixel/gauntlet.rs` — so every plot draws
the web node's "waiting for signal data…" empty state.

**Opening.** Three doors reach `Luma::open_pattern` (`graph.rs:348`): a row in
the patterns overlay (`patterns.rs:203`), the new-tab menu's Pattern choice
(`tab_chrome.rs:554`), and a double-click on a clip in the track editor
(`track_editor.rs:2297`). Only the third knows a track. `Target::Graph` is keyed
on the pattern id alone. §6 changes this.

**Seam coverage.** `Library` has `node_types`, `pattern_graph`,
`save_pattern_graph` (`library.rs:1206`, `:1219`, `:1236`). It does **not** have
`run_graph`. The dispatch command exists
(`src-tauri/src/dispatch/mod.rs:204` → `handlers/node_graph.rs:39`) and returns
`RunResult { views, mel_specs, color_views, universe_state }`, so this is a
missing facade method, not a missing command.

---

## 2. Question 1 — interaction routing on a painted canvas

### Design A: one hitbox, an internal hit-tree

Keep the single hitbox. Extend the pass that already resolves geometry so each
`Card` carries a small list of card-local regions, and add one function:

```rust
enum Hit {
    Port { card: usize, port: usize, output: bool },
    Widget { card: usize, param: usize, kind: WidgetKind },
    Header { card: usize },
    Body { card: usize },
    Wire { link: usize },
    Empty,
}

impl Scene {
    fn hit(&self, at: Point<f32>) -> Hit;
}
```

Resolution is card-first (reverse iteration, as `card_at` does today), then a
linear scan of that card's regions — so it stays O(cards) with a small constant,
not O(cards × regions). Ports get a hit radius larger than their painted ring
(the web's ring is 9px; a 16px grab box is the difference between "wiring works"
and "wiring is a dexterity test"). Wire hit-testing is a distance-to-polyline
test against the same four corner points `paint_wire` uses, run only when
nothing else hit.

The regions are computed in `Scene::measure`, which already walks every card and
already knows every box — the interaction layer is one extra `Vec<Region>` per
card and *zero* per-frame work.

### Design B: real gpui elements per visible card

Each visible card becomes a `div()`, absolutely positioned, with children for
header, ports and body. gpui hit-tests it for free, focus works, and the `arg/`
widgets drop straight in.

Three costs, and they compound:

1. **Zoom has no cheap expression.** gpui has no scene-graph transform; a
   scaled card means recomputing every `px()` from `zoom` on every frame that the
   zoom changed, which means re-laying-out the tree. The "measured once, never
   per frame" invariant — the single most important thing about this file — is
   gone. This is the WKWebView lesson (`project_graph_editor_perf`) repeated in
   a different runtime: layout is the enemy, transform-only won the last fight.
2. **Layout volume.** 100 cards × ~15 boxes each is ~1500 layout nodes per
   frame, for a surface whose entire job today is one repaint of one element.
3. **Entity churn on pan.** Culling means creating and dropping entities as
   cards enter and leave the viewport. Every promoted widget is an `Entity`
   with a `FocusHandle`; churning those on a pan is how you lose focus
   mid-edit and how you get an allocation storm on a flick.

### Pick: A, with a promotion escape hatch (see §3)

Ousterhout terms:

- **Pull complexity downward.** B pushes zoom, culling, and element identity up
  into every card. A eats it once, inside `Scene`, which already owns geometry.
- **Deep module.** `Scene::hit(point) -> Hit` is a two-line interface over the
  whole card layout. Every caller — press, hover, right-click, the future agent
  harness — asks the same question.
- **Information leakage.** Under B, every card subtree has to know about
  `Viewport`. Under A, `Viewport` stays where it is: known to the canvas and to
  the four gesture handlers, and to nothing else.
- **Different layer → different abstraction.** A card is a coordinate-space
  object. A `div` is a layout-space object. B forces the first through the
  second and pays the conversion every frame.

**What comparable gpui apps do.** The vendored tree is gpui only
(`gpui/vendor/zed/crates/` has no `editor` crate), so I can't cite zed's editor
source directly. What gpui itself offers is the answer anyway: hitboxes are
per-element and paint-order-scoped (`insert_hitbox`, `HitboxBehavior`,
`.occlude()`), which means a custom-painted surface gets exactly one hitbox and
must route internally. The in-repo precedent is stronger than an external one:
`crates/app/src/track_editor.rs` is already a painted canvas with an internal
hit test (`layout().row_at()`, `view.time_at()`, `timeline_insert_menu` at
`:2232`) and a real, laid-out, `.occlude()`d menu element overlaid on top
(`insert_menu` at `:3163`). That hybrid is the house pattern; this design
adopts it rather than inventing a second one.

---

## 3. Question 2 — where entity-widgets live

`ColorArgEditor`, `DraftedNumber` and `GroupExpressionEditor` are gpui
`Entity`s with focus handles and event streams. They cannot be painted.

### Design A: focus promotes to element

At most **one** widget is real at a time — the one being edited.

```rust
enum Editing {
    None,
    Param { node: SharedString, param: SharedString, widget: ParamWidget },
}

enum ParamWidget {
    Number(Entity<DraftedNumber>),
    Text(Entity<TextInput>),
    Expression(Entity<GroupExpressionEditor>),
    Color(Entity<ColorArgEditor>),
}
```

Rendered as a sibling of the canvas — `div().relative()` wrapping
`canvas_element(...)` plus `.children(state.editing.as_element())` — absolutely
positioned at the slot's window-space rect, `.occlude()`d. The painted card
*skips* the promoted slot so there is no double-draw. Committing (Enter, blur,
Escape) drops the entity and returns to painted.

This is `insert_menu`'s pattern verbatim, and `insert_menu`'s comment already
records the trap: a `Normal` hitbox stacked over the canvas does **not** stop
the canvas's own mouse handler, so the promoted element must `.occlude()` or
the press that focuses it will also start a pan.

**Zoom.** The promoted widget renders at 1:1 regardless of `Viewport::zoom`,
anchored to the slot's top-left in window space. At zoom 1 it sits exactly over
the painted slot; away from 1 it reads as a control growing out of the slot.
Stated as a rule so there is one positioning system and not two: *promoted
widgets are window-space chrome, not graph-space content.* The alternative —
scaling a real element with the viewport — reintroduces exactly the per-frame
layout cost that §2 rejected, for the one element on screen that least needs it.

### Design B: all-painted with a parallel interaction layer

Paint the caret, the selection, the text run; route keys through a hand-rolled
state machine.

**Reject, loudly.** This is a second text-editing implementation, a second
keymap, a second IME story, and a second focus model. CLAUDE.md's "one canonical
way" and the task's "the arg widget kit is the ONLY param-editing vocabulary"
both forbid it, and `text_input.rs` has ~1400 lines of exactly the work that
would be duplicated. The only argument for it is visual consistency at
zoom ≠ 1, which §A's rule buys more cheaply.

### Design C: real elements always, for visible cards

That's §2's Design B. Same rejection.

### Pick: A, with a two-tier split

Not every control needs an entity. Split by *what the control needs from the
keyboard*:

**Tier 1 — painted, gesture-driven.** Slider drag, select trigger, checkbox
toggle, gradient-stop drag, palette swatch pick, port drag. These take a
pointer and nothing else. The hit-tree resolves them; the gesture writes through
§4's mutation layer. A select trigger opens a real `float::` menu (a floating
menu is *never* painted — that's the glass tier, per `ladder.rs`'s "structural
planes are opaque ladder, floats take glass").

**Tier 2 — promoted on focus.** Number, text, expression, color. One at a time.

This matters for perf: a param *drag* — the highest-frequency interaction, the
one that drives 20Hz preview — never allocates an entity.

**The duplication risk, and the seam that closes it.** A painted slider is a
picture of `luma_slider`; a painted select is a picture of `<Selector>`.
`graph.rs` already documents that (`:2318`). The rule that keeps this from
becoming a second widget set: *a painted twin may duplicate the drawing; it may
not duplicate the value semantics.* Clamping, min/max mapping, drafting and
commit rules come from shared code. Today `arg/mod.rs:66` has `fraction_of` but
not the drag half, and the "route a `DragMoveEvent` by id, map pointer to a
fraction of `event.bounds`" idiom is copy-pasted five times
(`slider.rs:49`, `gradient.rs:182`, `color.rs:198`, `color.rs:204`,
`palette.rs:41`). Extracting that is Phase 0 work and it is what makes the
two-tier split honest rather than a loophole.

---

## 4. Question 3 — wire creation, and one write path

### The model: a shared edit vocabulary in `luma_lib`

Every mutation of the document — human, keyboard, undo, or the future graph
agent — is one of a closed set, applied by shared code:

```rust
// luma_lib::models::node_graph::edit
pub enum Edit {
    AddNode { type_id: String, at: Point<f64> },
    RemoveNode { id: String },
    MoveNode { id: String, to: Point<f64> },
    SetParam { node: String, param: String, value: Value },
    Connect { from: PortRef, to: PortRef },
    Disconnect { edge: String },
}

pub fn apply(graph: &mut Graph, catalogue: &Catalogue, edit: Edit) -> Result<Changed, EditError>;
```

**Rules that live inside `apply`, not in the UI:**

| Rule | Web location today |
|---|---|
| type equality, out→in only | `connection-validation.ts:50` **and** `graph-tools.ts:382` |
| single-slot inputs — connect replaces | `react-flow-editor.tsx:650` **and** `graph-tools.ts:401` |
| `removeDirectEdgesIfSplit` | `react-flow-editor.tsx:571` |
| node id minting `{type_id}_{n}` | `node-builder.ts:44` |
| param defaults on create | `node-builder.ts:96` |
| `pattern_args` is undeletable | `react-flow-editor.tsx:829` |
| synthetic `pattern_args` def | `pattern-args-node-def.ts` **and** `graph-tools.ts:64` **and** `graph.rs:1474` |

Four of those seven already exist in two or three copies on the web side, and
two have already drifted (`graph-tools.ts` re-implements the type check; the
`typeId → component` table in `node-builder.ts:104` and
`react-flow-editor.tsx:319` disagree about `falloff` and `invert`). Porting them
one-per-call-site would import a known-bad structure. **The gpui port gets
exactly one copy, in `luma_lib`, and `graph.rs` calls it.** §10 is the full
ledger of what that collapses, and which phase collapses it.

`pattern_args_def` (`graph.rs:1474`) moves there in the same commit — it is a
catalogue fact, not a view fact, and it is currently the third copy.

**Validation is a predicate, used twice.** `compatible(a: &PortType, b: &PortType, ...) -> bool`
is called by `apply` to reject an illegal `Connect`, and by the UI to highlight
legal drop targets during a drag. One predicate, two readers — not "the UI
pre-checks and the model trusts it", which is the TOCTOU shape AGENTS.md warns
about.

**Rejected alternative:** encode legality in the type system —
`Connect { link: LegalConnection }` where `LegalConnection` is minted by a
checked constructor. Tempting ("define errors out of existence"), but it pushes
the catalogue lookup up to every caller and gives the agent a second thing to
construct correctly. `Result` at one choke point is the smaller interface here.

### The UX

- **Press on a port** starts `Gesture::Wire { from: PortRef, cursor }`. Press on
  a *wired input* port detaches the existing edge and drags its loose end — the
  web doesn't do this (ReactFlow default), but it's free once ports are a hit
  region and it's what makes single-slot replace discoverable.
- **Live ghost wire** painted with `paint_wire` from the source anchor to the
  cursor, in the source port's hue — the web's `FilletConnectionLine`
  (`react-flow-editor.tsx:139`).
- **Legal targets highlight** while dragging: ports failing `compatible` drop to
  a muted ring. No motion, no pulse (CLAUDE.md).
- **Release on a legal port** issues `Edit::Connect`. Release anywhere else
  cancels — and, per §7 below, does *not* open the add-node palette, because
  drag-to-empty-then-pick is a second palette trigger.

### Persistence

`save_pattern_graph` is whole-document CAS, and the existing serialize-plus-dirty
machinery (`graph.rs:535`) already handles coalescing correctly. Extend it, don't
replace it: **saves are coalesced at command boundaries.** A param drag saves
once, on release — not per tick. Live feedback during the drag comes from
`run_graph` (§6), not from the write. `Edit::MoveNode` already behaves this way.

**Divergence from the web, deliberate:** the web is manual-save with an
unsaved-changes blocker (`pattern-editor.tsx:1761`); gpui autosaves on gesture
end. That difference is why §7 says undo ships from day one — an autosaving
editor with no undo is a data-loss surface, whereas the web's ⌘Z gap is partly
covered by "just don't hit Save".

**Note on edge ids.** `Edge::id` is host-owned — `graphFingerprint`
(`graph-checkpoint.ts:41`) explicitly canonicalizes edges to
`from:port->to:port` and treats ids as not-authored. A locally minted id is a
placeholder that the seam's canonical response replaces. `Edit::Disconnect`
therefore takes an id but must tolerate it being rewritten under it; the
existing "adopt the saved graph unless the screen is already ahead"
(`graph.rs:575`) is the right handling and needs no change.

---

## 5. Question 4 — add-node palette

### Pick: reuse `float::`, extract the picker loop first

`float.rs` already has the whole visual and structural vocabulary:
`popover_card()` (`:465`), `header_band()`/`footer_band()` for a search row and
key legend, `menu_row(RowState, fade)` (`:160`) with
`RowState::{Rest, Cursor, Selected}` — which is exactly the "keyboard cursor and
selection are different facts" distinction a palette needs — `viewport()`/`list()`
(`:240`, `:246`), `empty_row`, and the `key_cap` legend family. `dialog.rs`'s
`Popup<T>` (`:392`) owns the open → closing → closed lifecycle.

Two things are missing, and one of them is a duplication bug waiting to happen:

1. **`float::anchored_at(point, content)`.** `anchored_below` (`float.rs:489`)
   pins to the *parent element's* bottom edge via a zero-size absolute wrapper.
   A canvas hands you a window-space `Point<Pixels>`, not an element. This is a
   small sibling that feeds `gpui::anchored().position(p)` and keeps
   `snap_to_window_with_margin`. It belongs in `float.rs`, not at the call site.

2. **The query + rows + cursor loop is already written twice** — in
   `crates/app/src/chat_history.rs` (`refilter()` at `:134`, cursor stepping at
   `:258`, `scroll_to_item` at `:227`) and in
   `crates/ui/src/arg/expression.rs` (`suggestions` at `:149`, menu keyboard at
   `:319`). A third copy for the node palette is the bug, not the feature. Lift
   it into `float.rs` as a `Picker { query, rows, cursor }` and have all three
   compose it. That extraction is a Phase 0 gate.

**Behaviour**, matching the web (`react-flow-editor.tsx:682`, `:897`):

- Right-click on empty canvas opens it at the cursor; right-click on a card
  opens a one-item Delete menu instead (same popover, different rows).
- Searchable, `autoFocus`ed. Score: prefix on node name 2, substring on node
  name 1, substring on category 0.5 — the web's `commandScore` replacement.
- Grouped by `NodeTypeDef::category`, categories sorted, nodes sorted within.
  `pattern_args` is excluded — it is synthetic, one per graph, and undeletable.
- Enter commits `Edit::AddNode { type_id, at: <the right-click point in graph space> }`.

**Keyboard-first entry: bare `a`.** Right-click is a mouse gesture; the palette
also needs a key. `a` follows the track editor's precedent of bare letters for
canvas verbs (`h` = fit lanes) and reads as "add". The binding **must** carry
`&& !TextInput` (`text_input.rs`, context name at `lib.rs:82`) or it fires while
someone is typing in a promoted widget — which, given §3, is a state this screen
is in constantly. It opens the palette at the canvas centre, since a key press
has no cursor position of its own; the node lands where the palette was
anchored, exactly as the right-click path does.

**Rejected alternative:** a bespoke canvas popover painted into the scene.
It would need its own text field (see §3 Design B), its own scroll, its own
keyboard, and it would look like the app's menus without being one. Two menu
systems in one binary is the "one canonical way" violation.

---

## 6. Question 5 — preview data flow

### The context rule: no graph tab without a track

`run_graph` needs a `GraphContext` (`node_graph.rs:163`): `track_id`,
`venue_id`, `start_time`, `end_time`, and optionally a `BeatGrid`, `arg_values`
and an `instance_seed`. Today the editor can be opened three ways, and two of
them have no track:

| Door | Context |
|---|---|
| clip double-click (`track_editor.rs:2297`) | has both — the tab is `Target::TrackEditor { track, venue }` |
| pattern overlay row (`patterns.rs:203`) | none |
| new-tab menu → Pattern (`tab_chrome.rs:554`) | none |

**Ruling: the graph editor is not openable without a track context.** The
trackless doors go away, and with them the "waiting for signal data…" empty
state — every open graph tab is preview-capable by construction. This is
stronger than the web, where `pattern-editor.tsx` renders fine with no instance
selected and simply shows empty plots.

That is an error defined out of existence: `Editor` cannot hold a `None`
context, `Runner` has no "can't run yet" branch, and no plot has an empty state
to draw. Three things stop existing rather than being handled.

**Where the context comes from: the workspace, not a new picker.** The
patterns overlay stays what its module doc says it is — the door into the graph
editor — but the door now resolves a context from the tab strip: the active tab
if it is a `TrackEditor`, else the most recently active `TrackEditor` tab. With
no track editor open, pattern rows and the new-tab Pattern choice are **inert
with a named reason** ("open a track to edit patterns"); create, rename and
delete stay available, so the overlay is still a full pattern browser. The
precedent for a disabled choice carrying its reason is already in
`chrome.rs:690`.

Rejected alternative: **make the pattern list pick a track first.** That is two
pickers in one flow, and the order is backwards — you pick the track you are
working on, then the pattern in it, not the reverse. Rejected alternative:
**browse-only, editing reachable only from a clip.** Cleanest to state, but it
strands a freshly created pattern: you could make one and have no way to open
it. Resolving from the workspace keeps both flows alive with one rule.

**Tab identity does not change.** `Target::Graph { pattern }` stays
pattern-keyed and the context is a *field* on the `Editor`, retargetable.
Opening the same pattern against a different track reveals the existing tab and
re-points its context — a preview re-run, not a reload. Keying the tab on
`{ pattern, track, venue }` instead would let two tabs edit one document, which
is two writers racing through a CAS designed for exactly one. The conflict path
exists to survive *other people*; manufacturing a local race for it would be a
fight with the seam rather than a use of it.

**The context must be visible.** The toolbar gains a readout of the track the
graph is being evaluated against, beside the existing node count. Implicit
context that silently changes what the plots mean is the obscurity half of
Ousterhout's two causes; a resolved context that is never shown is worse than
no context at all.

### The gap

`Library` needs the `run_graph` facade method; the command already exists
(`dispatch/mod.rs:204`) and takes
`(graph, context, include_mel_specs, agent_thread_id, agent_execution_id, drive_live_preview)`.

### The Runner

One per editor, mirroring the web's two-stage throttle exactly:

1. **50ms leading + trailing coalescer** (`ONCHANGE_THROTTLE_MS`,
   `react-flow-editor.tsx:192`). `structural` is OR-ed across everything
   coalesced into a window so a topology change is never downgraded by a later
   slider tick.
2. **Single-in-flight, latest-wins queue** (`pattern-editor.tsx:1470`). A request
   made while one is out is stashed (OR-ing `include_mel_specs`); a monotonic run
   id discards stale responses.

`include_mel_specs` is false for param-only edits. This is the same shape as
`save_graph`'s existing saving/dirty pair, and for the same reason — worth
naming that symmetry rather than writing a second mechanism, but they are
genuinely different rates (a save is per-command; a run is per-tick) so they
stay separate objects.

### Getting results to painted bodies without a rebuild

**This is the one place the current architecture actively blocks the feature.**

`Body::Plot(Option<Trace>)` (`graph.rs:824`) embeds run data *inside* the Scene.
`Editor::views` is snapshotted at open, and a new publish is only picked up by
`rebuild()` (`graph.rs:280`) — which throws away the whole `measure` pass. At
20Hz that is a full geometry rebuild plus a full re-shape of every card, twenty
times a second, to move some polylines.

The fix is a separation the file's own module docs already argue for: **the Scene
holds shape; a separate store holds data.**

- `Body::Plot` carries the plot's *box* and nothing else.
- Traces live in `Rc<RefCell<HashMap<SharedString, Trace>>>`, shared by handle
  with the paint closure — exactly as `view`, `origin` and `fitted_size` already
  are (`graph.rs:110`–`:125`). That handle-sharing idiom is already the file's
  answer to "state the paint needs that changes at a different rate than the
  document"; the trace store is one more instance of it.
- A publish writes the store and calls `cx.notify()`. No rebuild, no re-measure.
- Legend chip widths need shaping, so trace resolution keeps the one-shot
  prepaint pattern: publish sets a flag, prepaint resolves the new traces once.

`ViewData` stays a global for the reason `graph.rs:136` gives — a run publishes
every view at once and the editor is one reader among several — but the editor
subscribes rather than snapshotting.

**What §9 ruling 1 deletes here.** A plot with no trace is now only the gap
between opening the tab and the first run returning, not a durable state, so
`WAITING` (`graph.rs:2121`) and the `Body::Plot(None)` paint branch
(`graph.rs:2027`) both go. What replaces them is nothing: the plot box paints
its ground and the traces arrive. The gauntlet fixture that pins the empty
state is retired with them, and the fixture that pins a *populated* plot becomes
the one that matters — it already exists, since the gauntlet capture is the only
publisher today.

**Smell to fix while here:** `RunResult::color_views` is produced by the backend,
threaded into the web store (`react-flow-editor.tsx:450`), and read by nothing.
A dead channel; don't port it (§10).

---

## 7. Question 6 — undo/redo

### Pick: ship it in Phase 1, before wire creation

The web has none. That is a gap, not a decision, and three things make it worse
here than there:

1. **gpui autosaves.** Every command is a durable write. The web's manual-save
   model gives you a crude undo by not pressing Save; this design does not.
2. **The graph agent writes through the same path.** Undo is the mechanism that
   makes accepting an agent edit safe. Without it, "the agent rewired my graph"
   is unrecoverable.
3. **The machinery already exists and is not clip-specific.**
   `track_editor.rs:456` — `History { past, future }`, `DEPTH = 100`,
   `record` clearing the future on a new branch, `checkpoint` /
   `abandon_checkpoint` / `undo` / `redo` / `restore`.

`History` should be **lifted out of `track_editor.rs`** into a shared module
rather than copied — a second copy is the smell. It is already generic in shape;
only `Snapshot` is timeline-specific.

**Snapshot shape.** `Snapshot { graph: Rc<Graph>, selected: Option<SharedString> }`.
Whole snapshots, not inverse edits — the same argument `track_editor.rs:449`
makes: the document is already the unit that gets written, so a command's inverse
is the document it replaced, and every `Edit` gets undo for free instead of owing
an inverse. A graph is a few hundred small structs behind an `Rc`; a step costs
one clone of the spine.

**Granularity — one checkpoint per command, at the gesture boundary:**

| Interaction | Checkpoint |
|---|---|
| node drag | at press; `abandon_checkpoint` if the node didn't move (`track_editor.rs:1143` already does exactly this with `Rc::ptr_eq`) |
| param drag (slider) | at drag start; the ticks mutate in place |
| number/text commit | one per commit event — `DraftedNumber` only emits when the value actually changed (`number.rs:120`), so this coalesces for free |
| connect / disconnect / add / delete | one each |
| agent turn | one for the whole turn — an agent edit is one thing the user accepted |

**Undo is a write like any other.** It goes through the same apply-then-save
path, exactly as `track_edit` states (`track_editor.rs:2213`) — the only
difference being that the undo step itself does not record a checkpoint,
because it is already moving along the stack.

`Editor::selected` is part of the snapshot for the reason
`track_editor.rs:437` gives: an undo that restored a node but left the selection
naming something that no longer exists puts the next command somewhere the eye
is not.

---

## 8. Question 7 — phases

Each phase is independently shippable and gated by an agent-harness test.
The harness addresses painted content through `agent_paint_node` registrations
(`ui/src/node.rs:183`), which must be called during prepaint and which
self-clip to the content mask.

### Phase 0 — seams, no visible change

**Ordered.** Items 1–5 touch only `luma_lib`, `crates/ui` and `graph.rs`;
item 6 is deliberately last — see the ownership note below.

1. `luma_lib::…::edit` — the `Edit` vocabulary, `apply`, `compatible`, id
   minting, param defaults, `pattern_args_def` moved out of `graph.rs`.
2. `ParamDef::range` added to the backend catalogue (approved — §9), with the
   gpui-side hardcoded table in `body_for` deleted against it.
3. `Library::run_graph` facade method; `Target::Graph`'s context rule (§6)
   applied to the three opening doors.
4. `float::anchored_at` + `float::Picker` extracted; `chat_history.rs` and
   `GroupExpressionEditor` migrated onto `Picker` in the same commit (otherwise
   the extraction is a third copy, not a deduplication).
5. The `DragMoveEvent`-by-id idiom extracted next to `arg::fraction_of`.
6. `History<S>` lifted out of `track_editor.rs`.

> **Ownership handoff.** Item 6 is the only one that edits
> `crates/app/src/track_editor.rs`, which the **ui-previews** agent currently
> owns pending its final pixel verification. The lift is a pure move — `History`
> is already generic in shape and only `Snapshot` is timeline-specific — but it
> is a move through a file whose pixel fixtures are mid-verification, and a
> rebase on top of an unverified diff is how a pixel regression gets attributed
> to the wrong change. Sequence it last in Phase 0 and take the handoff from
> ui-previews explicitly before starting it. Phase 1 needs `History`, so this is
> the one hard cross-agent dependency in the plan; if the handoff slips, Phase 1
> can start on the hit-tree and land undo second, but it must not ship without
> it.

**Gate:** unit tests on `apply` covering single-slot replace, split removal,
type rejection, id minting against existing ids, `pattern_args` protection. Every
graph-editor pixel fixture passes byte-identical, including after item 2 — the
ranges the catalogue now carries must be the ones the table hardcoded, which is
what makes the deletion verifiable rather than merely plausible. The one
intentional pixel change in this phase is item 3's inert pattern rows, so the
patterns-overlay fixture gains a no-track-open variant and is the only fixture
allowed to move.

### Phase 1 — hit-tree, selection, delete, undo

- `Region`/`Hit`, resolved in `Scene::measure`.
- `agent_paint_node` registrations for ports and param slots
  (`Role::Slider`, `Role::Select`, `Role::Input`, `Role::Button` for ports) so a
  script can say "the `phase` input of `osc_1`".
- Delete key, undo/redo bound.
- **Selection: shift-click *and* marquee.** Marquee is table stakes, not a
  Phase 2 nicety. It costs a third `Gesture` variant (`Marquee { from, to }`), a
  painted 1px rect in `ladder::primary()` with no fill animation, and a
  rect-intersect scan over `Scene::cards` — the same walk `extent()` already
  does. `Editor::selected` becomes `Vec<SharedString>`, which delete, undo's
  snapshot and the future agent all want anyway. Doing it now avoids widening
  `selected` twice.

**Gate:** harness test — click a port region and assert the right node is hit;
marquee-drag over three cards and assert all three select; delete the selection
and assert the document; undo restores it. A new
`tests/app_pixel/graph_budget.rs` alongside `track_editor_budget.rs` pinning
pan/zoom frame cost at 100+ nodes. That budget is the contract §2 was written to
protect, so it lands with the first interaction change, not after.

### Phase 2 — wire creation

Drag from port, ghost wire, legal-target highlight, detach-and-redrag, all
routed through `Edit::Connect` / `Edit::Disconnect`.

**Gate:** harness drags port→port and asserts edge count; a test that wiring
A→N then N→B removes a direct A→B edge; a test that an incompatible drop is
refused; a pixel fixture of a mid-drag ghost wire.

### Phase 3 — param editing

Tier 1 painted (slider, select, checkbox) and Tier 2 promoted (number, text,
expression, color). Plus the conditional-widget rule — hide an inline widget
when an incoming edge targets a handle whose id equals the param id. Make it
**generic** here; the web hand-rolls it twice with two different scoping hacks
(`beat-envelope-node.tsx:711` gates exactly two named params;
`noise-node.tsx:21` additionally filters against a hardcoded four-item
`PARAM_PORT_IDS`, which is a drifted copy of what the backend node def already
knows).

Tier-1 sliders read their bounds from `ParamDef::range`, added in Phase 0 — so
this phase adds no new hardcoded ranges and inherits none.

**Gate:** harness types into a promoted number field and asserts the document;
drags a painted slider and asserts one save on release, not N; a pixel fixture
of a promoted widget over its slot; a fixture of a node with a wired param port
showing the widget suppressed.

### Phase 4a — add-node palette

`float::Picker` anchored at the right-click point, grouped + scored, Enter
commits `Edit::AddNode`.

**Gate:** harness right-clicks empty canvas, types a query, arrows to a row,
presses Enter, asserts the node exists with the expected `{type_id}_{n}` id and
default params. Pixel fixture of the open palette.

### Phase 4b — live preview

`Runner` with the two-stage throttle; trace store moved out of `Scene`;
`ViewData` subscription.

**Gate:** a throttle unit test (N rapid edits → at most one in flight, latest
wins, `include_mel_specs` OR-ed correctly); a test asserting a publish does
**not** rebuild or re-measure the Scene; the existing gauntlet plot fixture
re-pointed at the new store and still byte-identical.

4b is last because it is the only phase blocked on an unsettled question (§9).

---

## 9. Rulings

All four open questions settled by Julian, 2026-08-25. Recorded here because
each one is load-bearing somewhere else in the doc.

1. **The graph editor is not openable without a track context.** Stronger than
   either alternative offered: rather than degrade preview when there is no
   track, the trackless path stops existing. `Target::Graph` always carries a
   resolved track + venue; the "waiting for signal data…" empty state dies with
   it; preview is always live-capable. Design consequences in §6 — the patterns
   overlay resolves context from the tab strip and goes inert-with-a-reason when
   no track editor is open, and tab identity stays pattern-keyed so one document
   never has two writers.

2. **Palette keyboard entry is bare `a`.** §5.

3. **Marquee select is table stakes** — Phase 1, alongside shift-click, not
   deferred. §8.

4. **`ParamDef` gains `range: Option<(f32, f32)>`** in the backend catalogue —
   approved. The gpui-side hardcoded table in `body_for` is deleted against it in
   Phase 0, so Phase 3's Tier-1 sliders never introduce one.

   **Follow-up, not in scope here:** the web has the same hardcoded ranges in
   `falloff-node.tsx` and `threshold-node.tsx` and should be deleted against the
   same catalogue field. Tracking it as a separate item rather than folding it in
   — this doc's phases touch `gpui/` only, and a web change riding along in a
   gpui phase is how a phase gate stops meaning anything.

---

## 10. De-duplication ledger

Reducing the web app's duplication is an **explicit goal of this port**, not a
side effect of it. Every duplication found while reading the reference
implementation is listed here with the single owner that replaces it and the
phase that kills it, so the payoff is auditable when phases land rather than
asserted in a summary.

The pattern worth naming: almost every one of these is a *contract* — a rule
about what a legal graph is — that got written wherever it was first needed. A
contract with two homes has no home. That is why §4 puts all of them behind one
`apply()` instead of porting them where they sit.

### Contracts the web wrote more than once

| Duplication | Copies | Single owner | Phase |
|---|---|---|---|
| port-type compatibility | `connection-validation.ts:50` + `graph-tools.ts:382` (**drifted** — the agent re-implements the check) | `compatible()` in `luma_lib`, read by `apply` to reject and by the UI to highlight | 0 |
| single-slot input replace | `react-flow-editor.tsx:650` + `graph-tools.ts:401` | `apply(Edit::Connect)` | 0 |
| `removeDirectEdgesIfSplit` | one copy, but **UI-only** — the agent's edits silently skip it | `apply(Edit::Connect)`, so agent and human get it identically | 0 |
| node id minting `{type_id}_{n}` | `node-builder.ts:44` (+ `syncNodeIdCounter` at `:34`) | `apply(Edit::AddNode)` | 0 |
| param defaults on create | `node-builder.ts:96` | `apply(Edit::AddNode)` | 0 |
| `pattern_args` synthetic def | `pattern-args-node-def.ts` + `graph-tools.ts:64` + `graph.rs:1474` — **three copies** | `luma_lib`, beside the edit vocabulary | 0 |
| `pattern_args` undeletable | `react-flow-editor.tsx:829` | `apply(Edit::RemoveNode)` | 0 |
| port-type colors | `types.ts:17` + `ladder.rs:244`, plus `port_key()` (`graph.rs:1512`) as a third spelling of the enum | `PortType::key()` in `luma_lib`; `ladder::port` keeps string keys so `luma_ui` stays independent | 0 |
| slider min/max | `graph.rs:1390` + `falloff-node.tsx` + `threshold-node.tsx` | `ParamDef::range` in the catalogue | 0 (gpui); web is a tracked follow-up |
| `typeId` → component dispatch, 18 branches | `node-builder.ts:104` + `react-flow-editor.tsx:319` — **drifted** (`falloff`/`invert` ordering; two redundant branches) | already dead: `body_for` (`graph.rs:1340`) is the single dispatch, and `Body`'s four-variant vocabulary is why a second one is not needed | — (killed at v1) |
| conditional inline widget | `beat-envelope-node.tsx:711` (two named params) + `noise-node.tsx:21`, whose `PARAM_PORT_IDS` is a **drifted four-item copy** of the port list the node def already carries | one generic rule in the body builder: suppress iff an incoming edge targets a handle whose id equals the param id | 3 |
| Delete key | window `keydown` (`react-flow-editor.tsx:733`) **and** ReactFlow's own `deleteKeyCode`, both live on Backspace, calling `removeNodeParams` twice | one Delete action → `apply(Edit::RemoveNode)` | 1 |
| `color_views` | produced by the backend, stored by the web (`react-flow-editor.tsx:450`), **read by nothing** | none — not ported; removing it from `RunResult` is a tracked follow-up | 4b (by omission) |

### Duplications on our side, closed before they grow

| Duplication | Copies | Single owner | Phase |
|---|---|---|---|
| query + cursor + scroll picker loop | `chat_history.rs:134` + `arg/expression.rs:149` | `float::Picker`, with both existing consumers migrated in the same commit | 0 |
| "route `DragMoveEvent` by id, map pointer to a fraction of `event.bounds`" | `slider.rs:49`, `gradient.rs:182`, `color.rs:198`, `color.rs:204`, `palette.rs:41` — **five copies**; `arg/mod.rs:66` already extracted the `fraction_of` half and stopped | one helper beside `fraction_of` | 0 |
| undo stack | `track_editor.rs:456`, private and about to be needed twice | lifted `History<S>` | 0 |
| instrumentation label | written twice at ~95 `.agent_node` call sites (`graph.rs:1568`, `chrome.rs:280`/`:690`/`:738`, `chat_history.rs:345`/`:355`, `add_tracks.rs:1108`) | **decided (Phase 0): registration inside the shared control constructors** — the constructor already holds the label (`luma_button(label, …)`), so it registers; a read-back combinator would be a second way to say the same thing. New shared-control registrations go inside the constructor from here on; migrating the ~95 existing call sites is a recorded follow-up, not Phase 0 work | 0 (decision) |

**Not de-duplicated, deliberately.** `graph.rs`'s geometry constants
(`CARD_MIN_WIDTH`, `PORT_ANCHOR`, `WIRE_FILLET`, …) restate values the web
spells as Tailwind classes. That is a port of a *rendering*, not a duplicated
contract: the two hosts must agree pixel-for-pixel and there is no shared
runtime to hold the number, so the duplication is checked by the gauntlet pixel
fixtures rather than removed. `graph.rs:655` already says this; the ledger
records it so nobody "fixes" it later.

---

## 11. Smells found in passing

Ordered by how much they cost.

1. **`Body::Plot(Option<Trace>)` conflates document shape with run data**
   (`graph.rs:824`). Cost: a full `Scene::build` + `Scene::measure` per preview
   publish. This is what blocks 20Hz preview, and it is the one thing in the
   file that has to change structurally rather than incrementally. Fixed in
   Phase 4b; described in §6.

2. **`paint_body` re-wraps the falloff blurb every frame.** `Body::measure`
   wraps `blurb` at `graph.rs:1186` purely to count lines and *throws the
   result away*; `paint_body` calls `wrap()` again at `graph.rs:2043` on every
   frame, for every visible falloff card. Meanwhile `row.help` is wrapped once
   and stored as `help_lines` — so the file already has the right pattern
   sitting next to the wrong one. The module docs name shaping as "by far the
   most expensive thing on this canvas" (`graph.rs:1800`). Fix: store
   `blurb_lines` beside `help_lines`. One-line change, no design needed.

3. **Instrumentation labels written twice at the call site.**
   `graph.rs:1568` — `.child(state.pattern.name.clone()).agent_node(Role::Text, state.pattern.name.clone())`.
   Same shape at `chrome.rs:280`, `:690`, `:738`, `chat_history.rs:345`, `:355`,
   `add_tracks.rs:1108`, and across ~95 `.agent_node` call sites app-wide. The
   failure mode is silent: someone edits the visible child, the node label keeps
   the old string, and every harness test that finds by label now finds a stale
   node or nothing. Two candidate fixes: an `.agent_labelled(Role)` that reads
   the child's text back, or registration inside the shared control constructors
   (`luma_button` already takes the label). **Ruled in Phase 0: the
   constructors register.** The constructor is the one place that provably
   holds the visible label, so the label cannot drift from the ink; a
   read-back combinator would instead teach the instrumentation to trust the
   element tree. The graph editor's new registrations follow this rule; the
   ~95 existing call-site registrations migrate as a recorded follow-up.

4. **Three spellings of `PortType`.** The enum in
   `node_graph.rs:8`, the string keys in `ladder::port()` (`ladder.rs:245`,
   documented as mirroring `PORT_TYPE_COLORS` in `types.ts`), and `port_key()`
   in `graph.rs:1512` converting between them. The string keying is a deliberate
   tradeoff (`luma_ui` must not depend on `luma_lib`), but `port_key` should be
   `impl PortType { fn key(&self) -> &'static str }` in `luma_lib` so there is
   one spelling and not two. The web has a fourth copy of the *compatibility*
   rule in `graph-tools.ts:382`, which §4 already addresses.

5. **Gestures are anchored to the active tab, not to the tab they started in.**
   `graph_drag` / `graph_release` / `save_graph` all resolve through
   `active_body_mut()` (`graph.rs:462`, `:500`, `:536`), while the async load
   path correctly uses `edit_graph_tab(&target, …)` (`graph.rs:419`) and says
   why. Switching tabs mid-drag leaves the original tab's `gesture` set
   permanently — a stuck pan or, once wires exist, a stuck wire drag. Wire
   creation makes this visible; the fix is for `Gesture` to carry its `Target`,
   the same way the load does.

6. **Hardcoded catalogue copy in the view.** `body_for` (`graph.rs:1385`) carries
   falloff's blurb, its two slider ranges and both help strings, and
   `audio_input`'s notice text (`:1382`), as literals. Faithful to the web,
   which has the same problem in `falloff-node.tsx` — but it is catalogue
   knowledge living in a painter. `ParamDef::range` (§9 ruling 4) is the fix for
   the half of it that matters and lands in Phase 0; the blurb and help strings
   are the remainder, and want a `description` on `ParamDef` eventually.

7. **`RunResult::color_views` is dead** — produced by the backend, stored by the
   web (`react-flow-editor.tsx:450`), read by nothing. Don't port it; consider
   deleting it from `RunResult`.
