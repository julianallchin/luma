// The builder's picture: the half the element tree cannot carry.
//
// `headless/venue_builder.test.js` proves every transition; these prove the
// *shape* — a ghost is drawn, a refused run is red, the distribution popup is
// drawn over the room, a held fixture draws a body. A capture is kept for
// each so a person can look, and each claim is measured off the pixels.
//
// The CAMERA readout is read either side of every gesture: building never
// moves the eye.

fixture({
  seconds: 20,
  clips: [{ pattern: "pat-glow", name: "Glow", start: 2, end: 5 }],
  rig: 4,
  window: [1400, 900],
});

const said = () => app.snapshot().findAll({ role: "text" }).map((n) => n.label);
const camera = () => said().find((l) => l.startsWith("CAMERA "));
const marks = (prefix) => app.snapshot().findAll({ role: "text" }).filter((n) => n.label.startsWith(prefix));
const sockets = () => app.snapshot().findAll({ role: "button" }).filter((n) => n.label.startsWith("Socket "));

// Drop free on the floor at a fraction of the room, and wait for the round
// trip. Place mode is sticky, so the room is the evidence: a placed piece
// brings its own sockets.
function dropAt(fx, fy) {
  const was = sockets().length;
  const pane = app.snapshot().find({ role: "card", label: "Stage drop surface" });
  app.drag({ x: pane.bounds.x + pane.bounds.width * fx, y: pane.bounds.y + pane.bounds.height * fy }, { dx: 1, dy: 0 }, { steps: 2 });
  until("the placement to land", () => sockets().length > was, { timeoutMs: 15000 });
  app.frames(8);
}

// The objects list's element rows. With a renderer the room is a canvas, and
// a point pick there is the camera's gesture too, so a test selects a piece
// off this list.
const elements = () => app.snapshot().findAll({ role: "row" }).filter((n) => n.label.startsWith("Element "));
function showObjects() {
  if (elements().length === 0) nav.step("the objects list", "toggle", "Stage objects");
  until("the objects list", () => elements().length > 0);
}
// Select a piece placed since `before` (a list of labels).
function selectNew(before) {
  showObjects();
  const fresh = until("the placed piece", () => elements().some((n) => !before.includes(n.label)));
  app.click(fresh.findAll({ role: "row" }).filter((n) => n.label.startsWith("Element ") && !before.includes(n.label)).at(-1), { restale: "match" });
  app.frames(6);
}

// A value box, swept to a fraction of its range from a point: the box
// writes its value into its label, so a node re-resolved mid-drag is gone.
function valueBox(name) {
  const box = app.snapshot().findAll({ role: "slider" }).find((n) => n.label.startsWith(`${name} = `));
  assert(box !== undefined, `no ${name}: ${said()}`);
  return box;
}
function sweep(name, fraction) {
  const box = valueBox(name);
  const y = box.bounds.y + box.bounds.height / 2;
  app.drag({ x: box.bounds.x + 2, y }, { dx: (box.bounds.width - 4) * fraction, dy: 0 }, { steps: 8 });
  app.frames(6);
}
const scrub = (name) => Number(valueBox(name).label.slice(name.length + 3));

function open() {
  nav.stage("Test Venue");
  nav.expand();
  app.frames(10);
}

// The dialog is the only way into place mode. The query brings the row into
// view: the list is longer than the card, and a clipped row refuses a click.
function arm(row) {
  nav.step("add element", "button", "Add element");
  until("the dialog", (s) => s.find({ role: "input", label: "Search elements" }) !== undefined);
  // The first word: a whole token, so the library's own search matches it.
  app.type(app.snapshot().find({ role: "input", label: "Search elements" }), row.split(" ")[0]);
  nav.step(`the ${row} row`, "row", row);
  app.frames(6);
}

test("a ghost and its beads are drawn without moving the eye", { timeoutMs: 120000 }, () => {
  open();
  const before = camera();
  const empty = app.screenshot();
  arm("Truss · straight");
  const bead = sockets().find((n) => n.label.endsWith("corner_fl"));
  assert(bead !== undefined, `no corner bead: ${sockets().map((n) => n.label)}`);
  // Aim without committing: `scroll` moves the pointer with no press, and a
  // press over the room is a placement.
  app.scroll(bead, { dy: 0, restale: "match" });
  app.frames(14);
  const ghost = app.screenshot();
  image.keep(empty, "venue-builder/room-empty");
  image.keep(ghost, "venue-builder/ghost");

  expect(camera()).toBe(before);
  expect(marks("Ghost ").map((n) => n.label)).toEqual(["Ghost Truss · straight"]);
  expect(image.diff(empty, ghost)).toBeGreaterThan(0.002);
});

test("a refused run is red and an accepted one is not", { timeoutMs: 120000 }, () => {
  open();
  showObjects();
  const existing = elements().map((n) => n.label);
  // A press outside an open float is eaten by its dismissal: close it first.
  nav.step("the objects list", "toggle", "Stage objects");
  until("the objects list to close", () => elements().length === 0);
  // Two stubs along the floor leave a gap between their ends: only a run
  // with something to reach can be refused. Stubs, because a landing through
  // the deck in the middle of the room is refused.
  arm("Truss · straight");
  sweep("stage-held-span", 0.03);
  dropAt(0.3, 0.8);
  dropAt(0.7, 0.8);
  app.key("escape");
  app.frames(4);
  selectNew(existing);

  // The end facing the other stick is the one that measures a gap.
  let measured = false;
  for (const label of sockets().filter((n) => n.label.includes("end_")).map((n) => n.label)) {
    const bead = sockets().find((n) => n.label === label);
    app.click(bead, { button: "right", restale: "match" });
    until("the menu", (s) => s.find({ role: "button", label: "Extend run" }) !== undefined);
    app.click(app.snapshot().find({ role: "button", label: "Extend run" }));
    app.frames(6);
    if (said().some((l) => l.startsWith("Gap: "))) {
      measured = true;
      break;
    }
    app.key("escape");
    app.frames(4);
    selectNew(existing);
  }
  assert(measured, `no end measured a gap: ${sockets().map((n) => n.label)}`);
  app.frames(12);
  const accepted = app.screenshot();
  const start = scrub("stage-length");
  // The far end of the length box is well past any gap in this room.
  sweep("stage-length", 1);
  app.frames(12);
  const refused = app.screenshot();
  image.keep(accepted, "venue-builder/run-accepted");
  image.keep(refused, "venue-builder/run-refused");

  // The two shots differ by the gesture, not by a repaint…
  expect(scrub("stage-length")).toBeGreaterThan(start);
  // …and the refusal is the ghost going red: 200 more red pixels.
  const red = (shot) => image.tint(shot, { channel: "red", margin: 60 });
  expect(red(refused)).toBeGreaterThan(red(accepted) + 200 / (1400 * 900 * accepted.scale * accepted.scale));
});

test("the distribution popup is drawn over the room", { timeoutMs: 120000 }, () => {
  open();
  const before = app.screenshot();
  // A fixture in hand, then the face it is laid along: pointing at the room
  // is the only way into the popover.
  arm("Luma Mover");
  const face = sockets().find((n) => n.label.endsWith("Deck top"));
  assert(face !== undefined, "no feature to lay a row along");
  app.click(face, { restale: "match" });
  until("the popover", (s) => s.find({ role: "button", label: "Place" }) !== undefined);
  app.frames(10);
  const popup = app.screenshot();
  image.keep(before, "venue-builder/popup-before");
  image.keep(popup, "venue-builder/popup");
  expect(image.diff(before, popup)).toBeGreaterThan(0.01);
});

// A fixture in the hand draws a *body*, the way a truss does — it used to
// draw only the element layer's mark, a dot.
test("a held fixture draws a body over the face it is aimed at", { timeoutMs: 120000 }, () => {
  open();
  const before = camera();
  const empty = app.screenshot();
  arm("Luma Mover");
  const face = sockets().find((n) => n.label.endsWith("Deck top"));
  assert(face !== undefined, "no face to aim at");
  // Aim without committing: a press over a face opens the row popover.
  app.scroll(face, { dy: 0, restale: "match" });
  app.frames(14);
  const held = app.screenshot();
  image.keep(empty, "venue-builder/fixture-empty");
  image.keep(held, "venue-builder/fixture-ghost");

  expect(camera()).toBe(before);
  expect(marks("Ghost ").map((n) => n.label)).toEqual(["Ghost Luma Mover"]);
  // A housing at this range covers thousands of pixels; the mark over it is
  // a five-pixel dot, orders of magnitude less.
  expect(image.diff(empty, held)).toBeGreaterThan(0.0002);
});

test("the selected object card follows a camera orbit", { timeoutMs: 120000 }, () => {
  open();
  until("the stage", (s) => s.find({ role: "toggle", label: "Stage objects" }) !== undefined);
  nav.step("objects", "toggle", "Stage objects");
  app.frames(12, { waitMs: 60 });
  const element = until("an element", (s) => s.findAll({ role: "row" }).find((n) => n.label.startsWith("Element ")))
    .findAll({ role: "row" }).find((n) => n.label.startsWith("Element "));
  nav.step("select element", "row", element.label);
  app.frames(12, { waitMs: 60 });
  const card = () => app.snapshot().find({ role: "card", label: "Selected object" });
  until("the object card", () => card() !== undefined);
  const beforeCard = card().bounds;
  const room = app.snapshot().find({ role: "card", label: "Stage" }).bounds;
  const cameraBefore = camera();
  app.drag({ x: room.x + room.width * 0.5, y: room.y + room.height * 0.25 }, { dx: 110, dy: 35 }, { steps: 10 });
  app.frames(12, { waitMs: 60 });
  assert(camera() !== cameraBefore, "the drag did not orbit");
  assert(JSON.stringify(card().bounds) !== JSON.stringify(beforeCard), "the card stayed fixed while the camera orbited");
});
