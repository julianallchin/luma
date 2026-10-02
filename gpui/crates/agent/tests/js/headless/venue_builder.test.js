// The stage page, driven from outside.
//
// Every claim the builder makes is an element — the hand's readout, the
// landing it would commit, the relation the graph wrote down, the refusal
// that stops it, the beads a socket can be clicked by — so these tests can
// exist at all with the renderer off. What the picture adds is inspected in
// `js/pixel/venue_builder.test.js`.
//
// Nothing here asserts a coordinate. Every claim is read back off the solved
// graph — the edge that was written, the constraint that was checked, the
// freedom the joint admits — so an assertion cannot pass by restating the
// gesture that produced it.
//
// The inline-field probe (`height_row_is_inline_…`) stays in
// `tests/headless/venue_builder.rs`: it mounts a bespoke gpui view, not the
// app.

fixture({ seconds: 20, clips: [{ pattern: "pat-glow", name: "Glow", start: 2, end: 5 }], rig: 4, window: [1400, 900] });

// What the picture says. The builder publishes no state labels, so every
// claim is read off the thing that draws it: the ghost, the station marks,
// the measurement, the beads.
const said = () => app.snapshot().findAll({ role: "text" }).map((n) => n.label);
const one = (prefix) => said().find((l) => l.startsWith(prefix));
const marks = (prefix) => said().filter((l) => l.startsWith(prefix));
const sockets = () => app.snapshot().findAll({ role: "button" }).filter((n) => n.label.startsWith("Socket "));
const hasText = (prefix, also = () => true) => (s) =>
  s.findAll({ role: "text" }).some((n) => n.label.startsWith(prefix) && also(n.label));

// Reach the builder, with the room's own tab open.
function open() {
  nav.stage("Test Venue");
  nav.expand();
  app.frames(6);
  until("the builder", (s) => s.findAll({ role: "button", label: "Add element" }).length > 0);
}

// Press a button, having first walked the pointer onto it: gpui keeps a drag
// alive until a release lands somewhere the drag did not start, so the first
// press after a sweep would otherwise be spent ending that drag. `scroll`
// with no delta moves the pointer without pressing anything.
function press(label) {
  app.scroll(app.snapshot().find({ role: "button", label }), { dy: 0 });
  app.click(app.snapshot().find({ role: "button", label }));
  app.frames(4);
}

// A segmented choice is a toggle, not a button.
function pick(label) {
  app.click(app.snapshot().find({ role: "toggle", label }));
  app.frames(4);
}

// Poll in blocks of frames: a builder verb is two round trips deep (the
// attach, then the far-end check spawned from inside its answer), and a
// one-frame step does not reliably carry the second one back.
function settle(what, pred) {
  for (let i = 0; i < 160; i += 1) {
    if (pred(app.snapshot())) return;
    app.frames(4);
  }
  throw new Error(`never settled on ${what}: ${app.snapshot().nodes.map((n) => `${n.role}:${n.label}`).join(", ")}`);
}

// The add-element dialog is the only way into place mode. The query brings a
// row into view: the list is longer than the card, and a click on a clipped
// row is a refusal. The first word is a whole token the search matches.
function arm(row) {
  press("Add element");
  until("the dialog", (s) => s.findAll({ role: "input", label: "Search elements" }).length > 0);
  app.type(app.snapshot().find({ role: "input", label: "Search elements" }), row.split(" ")[0]);
  until("the row", (s) => {
    const n = s.findAll({ role: "row" }).find((n) => n.label === row);
    return n !== undefined && n.bounds.height > 0;
  });
  app.click(app.snapshot().find({ role: "row", label: row }));
  app.frames(6);
}

// Drop free on the floor at a fraction of the viewport, and wait for the
// round trip. Place mode is sticky, so the room is the evidence: a placed
// piece brings its own sockets. A one-pixel drag, because a point is not a
// node and `drag` walks the pointer first, which aims the ghost.
function dropAt(fx, fy) {
  const was = sockets().length;
  const pane = app.snapshot().find({ role: "card", label: "Stage drop surface" });
  app.drag({ x: pane.bounds.x + pane.bounds.width * fx, y: pane.bounds.y + pane.bounds.height * fy }, { dx: 1, dy: 0 }, { steps: 2 });
  settle("the placement to land", () => sockets().length > was);
  app.frames(8);
}

// Click a node found on an earlier frame: the builder repaints on every
// readout.
function tap(node) {
  app.click(node, { restale: "match" });
  app.frames(4);
}

// Click-select a placed piece at a window point or a fraction of the room.
// Escape empties the selection, so a test that wants a piece's beads back
// selects it the way a person does.
function selectAt(x, y) {
  app.drag({ x, y }, { dx: 1, dy: 0 }, { steps: 2 });
  app.frames(6);
}
function select(fx, fy) {
  const room = app.snapshot().find({ role: "card", label: "Stage room" });
  selectAt(room.bounds.x + room.bounds.width * fx, room.bounds.y + room.bounds.height * fy);
}

function valueBox(name) {
  const box = app.snapshot().findAll({ role: "slider" }).find((n) => n.label.startsWith(`${name} = `));
  if (box === undefined) throw new Error(`no ${name}: ${said().join(", ")}`);
  return box;
}

// Sweep a value box to a fraction of its own range, dragged from a point:
// the box writes its value into its label, so a node re-resolved by label
// mid-drag no longer exists.
function sweep(name, fraction) {
  const box = valueBox(name);
  const y = box.bounds.y + box.bounds.height / 2;
  app.drag({ x: box.bounds.x + 2, y }, { dx: (box.bounds.width - 4) * fraction, dy: 0 }, { steps: 8 });
  app.frames(6);
}

// What a value box reads, off its own label.
const scrub = (name) => Number(valueBox(name).label.slice(name.length + 3));

// What a box reads once it has stopped moving: some boxes answer after a
// round trip, and gpui holds a drag until its release has been drawn.
function settled(name) {
  let last = scrub(name);
  for (let i = 0; i < 60; i += 1) {
    app.frames(4);
    const now = scrub(name);
    if (now === last) return now;
    last = now;
  }
  return last;
}

// Put a value box on an exact value without knowing its range: two sweeps
// calibrate the scale and are read back, then Newton on a line, because a
// scrub lands on its step. Neither probe is at an end: a zero-length drag
// moves nothing.
function setScrub(name, target) {
  sweep(name, 0.25);
  const a = settled(name);
  sweep(name, 0.75);
  const b = settled(name);
  if (b === a) return a;
  const per = (b - a) / 0.5;
  let f = 0.25 + (target - a) / per;
  for (let i = 0; i < 10; i += 1) {
    sweep(name, Math.min(1, Math.max(0.001, f)));
    const at = settled(name);
    if (at === target) return at;
    f += (target - at) / per;
  }
  return settled(name);
}

// Shrink the held truss to a stub: a landing through placed structure is
// refused, and most of these rooms have a deck in the middle.
function shorten() {
  sweep("stage-held-span", 0.03);
  app.frames(2);
}

function socket(suffix) {
  const found = sockets().find((n) => n.label.endsWith(suffix));
  if (found === undefined) throw new Error(`no ${suffix} bead: ${sockets().map((n) => n.label).join(", ")}`);
  return found;
}

// How many gizmo-mode cells the viewport offers: a widget that does not
// apply has no pair of modes.
const gizmoModes = () => app.snapshot().findAll({ role: "toggle" }).filter((n) => n.label === "Translate" || n.label === "Rotate").length;

// The context menu on a placed piece: a right press on its bead, then the
// verb.
function menu(bead, item) {
  app.click(bead, { button: "right", restale: "match" });
  until("the menu", (s) => s.findAll({ role: "button" }).some((n) => n.label === item));
  press(item);
}

const heldSurface = () => app.snapshot().find({ role: "card", label: "Stage drop surface" }) !== undefined;
const patched = () => app.snapshot().findAll({ role: "row" }).filter((n) => n.label.startsWith("Mover ")).length;

// The add-element dialog is what arms the hand; escape is the way back out.
test("the dialog arms a ghost and escape puts it down", () => {
  open();
  expect(marks("Ghost ")).toEqual([]);
  press("Add element");
  until("the dialog", (s) => s.findAll({ role: "input", label: "Search elements" }).length > 0);
  const rows = app.snapshot().findAll({ role: "row" }).map((n) => n.label);
  // A stick and a tower are two catalog rows over one generator, and
  // fixtures are in the same list as catalog pieces.
  expect(rows).toContain("Truss · tower");
  expect(rows).toContain("Luma Mover");
  app.click(app.snapshot().find({ role: "row", label: "Truss · straight" }));
  app.frames(6);
  // A held piece owns the pointer: the surface exists only while it does.
  assert(heldSurface(), "an armed hand did not take the pointer");
  // The ghost is the hand, so it has to be aimed before it can be seen.
  app.scroll(socket("corner_fl"), { dy: 0 });
  app.frames(6);
  expect(marks("Ghost ")).toEqual(["Ghost Truss · straight"]);
  app.key("escape");
  app.frames(6);
  expect(marks("Ghost ")).toEqual([]);
  assert(!heldSurface(), "the pointer stayed claimed after the hand was put down");
});

// A truss dropped on a deck corner mates that corner, and the edge the graph
// wrote names both halves of the joint.
test("a truss dropped on a deck corner bolts to it", () => {
  open();
  arm("Truss · straight");
  const bead = socket("corner_fl");
  const at = { x: bead.bounds.x + bead.bounds.width / 2, y: bead.bounds.y + bead.bounds.height / 2 };
  tap(bead);
  // The stick rose out of the corner, so it is clicked back a little above.
  app.key("escape");
  app.frames(4);
  selectAt(at.x, at.y - 40);
  settle("the placement to land", hasText("Edge: "));
  app.frames(8);
  const edge = one("Edge: ");
  expect(edge).toContain("corner_fl");
  assert(/end_a|seat|base/.test(edge), `the edge names no socket the truss has: ${edge}`);
  // A bolt circle has no roll, so the piece has no widget and no modes.
  expect(gizmoModes()).toBe(0);
  assert(sockets().some((n) => n.label.includes("face_")), "the truss's own faces never joined the room");
});

// Two sticks along the floor leave a gap. The ray measures it, a longer run
// is refused, and the exact one bridges — writing an edge plus a far-end
// check the solve reports satisfied.
test("a run measures its gap, refuses a longer one and bridges an exact one", () => {
  open();
  arm("Truss · straight");
  shorten();
  dropAt(0.3, 0.8);
  // Place mode is sticky: the second stick keeps the shortened span.
  dropAt(0.7, 0.8);
  app.key("escape");
  app.frames(4);
  select(0.7, 0.8);

  // The end facing the other stick is the one with the widest gap; which
  // end that is is a fact about the room. Each measurement starts from rest,
  // where only the selected stick's beads show.
  const ends = sockets().filter((n) => n.label.includes("end_")).map((n) => n.label);
  let measured = null;
  for (const label of ends) {
    menu(socket(label.slice("Socket ".length)), "Extend run");
    const gap = one("Gap: ");
    if (gap !== undefined) {
      const metres = Number(gap.slice(5, gap.indexOf(" m")));
      if (measured === null || metres > measured.metres) measured = { socket: label, metres };
    }
    app.key("escape");
    app.frames(4);
    select(0.7, 0.8);
  }
  assert(measured !== null, `no end measured a gap: ${sockets().map((n) => n.label).join(", ")}`);
  app.key("escape");
  app.frames(4);
  select(0.7, 0.8);
  menu(socket(measured.socket.slice("Socket ".length)), "Extend run");
  // Feet are display-only; their presence proves a measurement.
  const started = one("Gap: ");
  assert(started !== undefined && started.includes(" ft "), `clicking a socket did not start a measured run: ${started}`);

  // Past the gap: refused, and the commit is unreachable.
  sweep("stage-length", 1);
  expect(scrub("stage-length")).toBeGreaterThan(measured.metres);
  expect(app.snapshot().find({ role: "button", label: "Place run" }).enabled).toBe(false);

  // Back to exactly the gap — two readouts, one number — and place it.
  expect(setScrub("stage-length", measured.metres)).toBe(measured.metres);
  expect(app.snapshot().find({ role: "button", label: "Place run" }).enabled).toBe(true);
  press("Place run");
  settle("the run to land", hasText("Constraint: "));
  expect(one("Constraint: ")).toContain("satisfied");
  expect(one("Edge: ")).toContain("end_");
});

// A free placement flies: trim moves it, and the number the box comes back
// with is the one the solve holds, not the one the drag asked for.
const heightNode = () => app.snapshot().findAll({ role: "slider" }).find((n) => n.label.startsWith("stage-height-"));
function height() {
  const node = heightNode();
  if (node === undefined) throw new Error(`no height box: ${app.snapshot().findAll({ role: "slider" }).map((n) => n.label)}`);
  return parseFloat(node.label.split(" = ")[1]);
}

// A free stick on the floor, selected, with its height typed to 3.25.
function placeAndType() {
  open();
  arm("Truss · straight");
  dropAt(0.15, 0.82);
  // Still stamping: place mode is sticky, so the sheet is withheld rather
  // than slid over the room the next click aims at.
  expect(heightNode()).toBe(undefined);
  app.key("escape");
  app.frames(6);
  select(0.15, 0.82);
  // A piece on the venue's own floor is the gizmo's one case.
  expect(gizmoModes()).toBe(2);
  expect(height()).toBe(0);
  app.drag(heightNode(), { dx: 80, dy: 0 }, { steps: 8 });
  app.frames(12, { waitMs: 40 });
  const lifted = height();
  expect(lifted).toBeGreaterThan(0);
  // A click turns the box into a text field; enter commits it.
  app.click(heightNode());
  app.key("ctrl-a 3 . 2 5");
  app.key("enter");
  app.frames(12, { waitMs: 40 });
  expect(height()).toBe(3.25);
  return lifted;
}

test("height drags, types and undoes a free placement", () => {
  const lifted = placeAndType();
  app.action("luma::UndoStage");
  app.frames(12, { waitMs: 40 });
  expect(height()).toBe(lifted);
  expect(one("Edge: ")).toContain("venue floor");
});

// Escape in the field cancels the draft and keeps the piece selected. It
// does not: the stage's escape binding matches before the field's key
// handler, so the same press empties the selection and the sheet goes.
test("escape in the height field cancels the draft and keeps the selection", () => {
  placeAndType();
  app.click(heightNode());
  app.key("ctrl-a 9");
  app.key("escape");
  app.frames(3);
  expect(height()).toBe(3.25);
  expect(gizmoModes()).toBe(2);
});

// ⌘D copies the selected subtree onto the cursor, and clicking a socket
// places it. Flip mirrors the copy, so it bolts to the opposite corner.
test("duplicate and flip place a copy on the opposite corner", () => {
  open();
  arm("Truss · straight");
  const fl = socket("corner_fl");
  const flAt = { x: fl.bounds.x + fl.bounds.width / 2, y: fl.bounds.y + fl.bounds.height / 2 };
  tap(fl);
  app.key("escape");
  app.frames(4);
  selectAt(flAt.x, flAt.y - 40);
  settle("the first wing", hasText("Edge: "));
  app.frames(8);
  expect(one("Edge: ")).toContain("corner_fl");

  app.key("secondary-d");
  app.frames(6);
  // A held piece owns the pointer: the picture saying the copy is in hand.
  assert(heldSurface(), "⌘D put nothing in the hand");
  menu(socket("corner_fr"), "Flip");
  assert(heldSurface(), "Flip put the copy down instead of turning it over");
  tap(socket("corner_fr"));
  settle("the copy to land", hasText("Edge: ", (l) => l.includes("corner_fr")));
});

// A fixture carried onto a face is a row: the popover previews one ghost per
// body and commits every one of them in a single gesture.
test("a fixture on a face previews a row and places all of it", () => {
  open();
  // A stick on the floor, so there is a face to hang a row along.
  arm("Truss · straight");
  dropAt(0.5, 0.35);
  app.key("escape");
  app.frames(4);
  const before = patched();
  arm("Luma Mover");
  tap(socket("face_-y"));
  until("the popover", (s) => s.findAll({ role: "slider" }).some((n) => n.label.startsWith("stage-count = ")));
  app.frames(8);
  const wanted = setScrub("stage-count", 4);
  expect(wanted).toBe(4);
  // One station mark per body the row would seat, before anything commits.
  expect(marks("Station ").length).toBe(wanted);
  press("Place");
  settle("the row to land", () => patched() > before);
  app.frames(10);
  expect(patched()).toBe(before + wanted);
});

// A row that will not fit is refused whole, and the refusal carries the
// length that would make it fit. Pressing that offer makes the same row fit.
test("a row that will not fit is refused and the offer makes it fit", () => {
  open();
  arm("Truss · straight");
  dropAt(0.5, 0.35);
  app.key("escape");
  app.frames(4);
  const before = patched();
  arm("Luma Mover");
  tap(socket("face_-y"));
  until("the popover", (s) => s.findAll({ role: "slider" }).some((n) => n.label.startsWith("stage-count = ")));
  app.frames(8);
  // A metre apart, more of them than the stick is long.
  pick("Spacing");
  const wanted = setScrub("stage-count", 24);
  app.frames(6);
  const refusal = said().find((l) => l.includes(" m") && !l.startsWith("Gap: ") && !l.includes(" will fit") && l.includes("face"));
  assert(refusal !== undefined, "a row too long for its face was not refused in words");
  const offer = app.snapshot().findAll({ role: "button" }).map((n) => n.label).find((l) => l.startsWith("Extend"));
  assert(offer !== undefined && offer.includes(" m"), `the refusal offered no length to fix it: ${offer}`);
  expect(app.snapshot().find({ role: "button", label: "Place" }).enabled).toBe(false);
  expect(marks("Station ").length).toBe(0);

  press(offer);
  settle("the refit", hasText("", (l) => l.includes(" will fit")));
  app.frames(8);
  // The count never changed; the extend is what made it fit.
  assert(one(`${wanted} will fit`) !== undefined, "the offered extend did not make the row fit");
  press("Place");
  settle("the row to land", () => patched() > before);
  app.frames(10);
  expect(patched()).toBe(before + wanted);
});

// A socket with nothing in front of it still builds: "ray hits nothing →
// ghost at 0.5 m, type a length". A stub is how a rig grows into empty air.
test("a socket facing nothing still builds a stub at the length asked for", () => {
  open();
  // One stick alone: every end faces nothing.
  arm("Truss · straight");
  dropAt(0.15, 0.82);
  app.key("escape");
  app.frames(4);
  select(0.15, 0.82);
  menu(socket("Truss · straight end_b"), "Extend run");
  app.frames(6);
  expect(one("Gap: ")).toBe(undefined);
  assert(app.snapshot().findAll({ role: "slider" }).some((n) => n.label.startsWith("stage-length = ")),
    "an end facing nothing offered no length box");
  const asked = setScrub("stage-length", 3);
  expect(asked).toBe(3);
  expect(app.snapshot().find({ role: "button", label: "Place run" }).enabled).toBe(true);
  press("Place run");
  // The landed run selects itself; its relation is the graph's claim.
  settle("the stub to land", hasText("Edge: ", (l) => l.includes("end_")));
  app.frames(10);
  // The span the graph took, read off the sheet's own control.
  expect(settled("stage-span")).toBe(asked);
  expect(one("Edge: ")).toContain("end_");
});

// A venue with nothing in it is already buildable: `+` alone lands the first
// piece, and the second lands without going back to the dialog.
test("an empty venue takes the first piece from the button alone", { fixture: { rig: 0 } }, () => {
  open();
  // The venue's own synthesized planes are the room, not something in it.
  const pieces = () => sockets().filter((n) => !n.label.includes(":venue "));
  expect(pieces().length).toBe(0);
  arm("Truss · straight");
  shorten();
  dropAt(0.25, 0.72);
  // Place mode is sticky: no second trip through the dialog.
  expect(app.snapshot().findAll({ role: "input", label: "Search elements" }).length).toBe(0);
  dropAt(0.75, 0.72);
  // Counted by a face, one per stick, while the hand still stamps: an end
  // can be consumed by a mate or culled at the screen's edge.
  const faces = pieces().map((n) => n.label).filter((l) => l.endsWith("face_-y"));
  expect(faces.length).toBe(2);
  app.key("escape");
});

// A fixture carried onto the floor is a row too: the face is whatever the ray
// met, and the popover opens on it, within the spec's 32 px of the click.
test("a fixture dropped on the floor opens the row popover where it landed", () => {
  open();
  arm("Luma Mover");
  const pane = app.snapshot().find({ role: "card", label: "Stage drop surface" });
  const at = { x: pane.bounds.x + pane.bounds.width * 0.42, y: pane.bounds.y + pane.bounds.height * 0.78 };
  app.drag(at, { dx: 1, dy: 0 }, { steps: 2 });
  until("the popover", (s) => s.findAll({ role: "slider" }).some((n) => n.label.startsWith("stage-count = ")));
  app.frames(8);
  const card = app.snapshot().find({ role: "card", label: "Row popover" }).bounds;
  // Distance to the nearest point of the card.
  const dx = Math.max(card.x - at.x, at.x - (card.x + card.width), 0);
  const dy = Math.max(card.y - at.y, at.y - (card.y + card.height), 0);
  expect(one("On ")).toContain("floor");
  expect(Math.hypot(dx, dy) <= 32).toBe(true);
  expect(marks("Station ").length).toBeGreaterThan(0);
});

// `A` opens the dialog, escape closes it, `A` opens it again: a close path
// that forgets to reseat focus leaves it on the unmounted field.
test("A, escape, A reopens the dialog", () => {
  open();
  const dialogUp = () => app.snapshot().findAll({ role: "input", label: "Search elements" }).length > 0;
  app.key("a");
  until("the first open", dialogUp);
  app.key("escape");
  app.frames(12);
  assert(!dialogUp(), "escape did not close the dialog");
  app.key("a");
  app.frames(12);
  assert(dialogUp(), "A did not reopen the dialog");
});

// ⌘Z restores the graph a placement replaced, and ⇧⌘Z brings it back.
test("undo takes a placement back and redo replays it", () => {
  open();
  // Beads at rest belong to the selection.
  expect(sockets().length).toBe(0);
  arm("Truss · straight");
  // Open floor away from the deck and its fixtures.
  dropAt(0.15, 0.82);
  app.key("escape");
  app.frames(4);
  // "Is the piece there" is asked by selecting where it was dropped, re-asked
  // each poll because the verbs are asynchronous.
  const there = () => {
    select(0.15, 0.82);
    return sockets().length;
  };
  settle("the placement to land", () => there() > 0);
  app.key("escape");
  app.frames(4);
  app.action("luma::UndoStage");
  settle("the placement to unwind", () => there() === 0);
  app.action("luma::RedoStage");
  settle("the piece to come back", () => there() > 0);
});

test("the element list removes immediately and undo restores it", () => {
  nav.stage("Test Venue");
  const fixtures = () => app.snapshot().findAll({ role: "row" }).filter((n) => n.label.startsWith("Mover "));
  until("the fixture inventory", () => fixtures().length > 0);
  const fixtureCount = fixtures().length;
  nav.step("objects", "toggle", "Stage objects");
  const elements = () => app.snapshot().findAll({ role: "row" }).filter((n) => n.label.startsWith("Element "));
  until("the stage elements", () => elements().length > 0);
  const before = elements().length;
  nav.step("an element", "row", elements()[0].label);
  nav.step("remove", "button", "Remove element");
  app.frames(6);
  // Immediate: no confirmation, because undo is the way back.
  expect(app.snapshot().find({ role: "card", label: "Confirm dialog" })).toBe(undefined);
  nav.step("the remaining objects", "toggle", "Stage objects");
  until("the element removed", () => elements().length < before);
  app.key("escape");
  // A light never outlives its place: the lights hung on the element go
  // with it, and undo brings them back with their patch rows.
  until("the fixture inventory after removal", () => fixtures().length <= fixtureCount);
  app.action("luma::UndoStage");
  until("the restored fixtures", () => fixtures().length === fixtureCount);
  nav.step("the restored objects", "toggle", "Stage objects");
  until("the restored elements", () => elements().length === before);
});

// A light deleted from the room leaves the patch with it, and undo brings the
// light back with its patch row.
test("deleting a light unpatches it and undo patches it again", () => {
  nav.stage("Test Venue");
  const fixtures = () => app.snapshot().findAll({ role: "row" }).filter((n) => n.label.startsWith("Mover ")).map((n) => n.label);
  until("the fixture inventory", () => fixtures().includes("Mover 0"));
  const before = fixtures();
  nav.step("a light", "row", "Mover 0");
  app.action("luma::DeleteStageElement");
  until("the light to leave the patch", () => !fixtures().includes("Mover 0"));
  expect(fixtures().length).toBe(before.length - 1);
  app.action("luma::UndoStage");
  until("the light to come back", () => fixtures().includes("Mover 0"));
  expect(fixtures()).toEqual(before);
});
