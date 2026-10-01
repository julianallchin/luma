// The clip graph editor from the outside: a clip's graph is a canvas of node
// cards, sources on the left and the output on the right, with a wire from
// each node's output port into every input it feeds. The source chip
// promotes a value; a wire dragged from an output port onto an input links
// it; a wire dragged off an input unwires it; the add menu and a card's
// delete button add and remove nodes; segments store settings; a strip
// stores curve points; the name field names the clip on the timeline; the
// fade handle writes alpha; undo takes a whole gesture back.

// One clip of a shipped preset over beats 2–6 (seconds 1–3), keyed `graph-clip`.
const clipOf = (preset) => ({ pattern: "graph-clip", name: preset, start: 1, end: 3, preset });
fixture({ seconds: 20, clips: [clipOf("Chase")], rig: 4, window: [1400, 1400] });

const node = (role, label) => until(label, (s) => s.find({ role, label })).find({ role, label });
const settle = () => app.frames(4);
const stored = () => library.score().clips["graph-clip"];
const nodes = () => stored().graph.nodes;
const KINDS = ["clock", "time", "space", "noise", "audio", "curve", "mirror", "shuffle", "group", "split", "color", "aim", "strobe"];
// A node's card title: a kind and a number for an id such as `curve2`
// ("Curve 2"), and a given name (`pill`) as it is.
const titleOf = (id) => {
  const m = /^([a-z]+)(\d+)$/.exec(id);
  return m && KINDS.includes(m[1]) ? `${m[1][0].toUpperCase()}${m[1].slice(1)} ${m[2]}` : id;
};
// Whether a card titled `label` is one of the stored graph's nodes.
const nodeTitles = () => new Set(Object.keys(nodes()).map(titleOf));

// Open the clip's graph and widen the panel for it.
function open(clip) {
  nav.venue("Test Venue");
  nav.track("Aurora");
  nav.expand();
  nav.stageOff();
  app.click(node("card", clip));
  until("the graph", (s) => s.find({ role: "card", label: "Graph canvas" }) && s.find({ role: "input", label: "Name" }));
  nav.widenGraph();
}

const canvas = (snap = app.snapshot()) => snap.find({ role: "card", label: "Graph canvas" }).bounds;
const inside = (outer, inner) =>
  inner.x >= outer.x - 0.5 && inner.y >= outer.y - 0.5 &&
  inner.x + inner.width <= outer.x + outer.width + 0.5 && inner.y + inner.height <= outer.y + outer.height + 0.5;

// Pan the canvas until what `find` names is in view.
const reveal = (find, what) => nav.inGraph(find, what);

const shown = (role, label) => reveal((s) => s.find({ role, label }), label);

// The innermost node card holding `bounds`, in snapshot `snap`.
function cardOf(bounds, snap) {
  const titles = nodeTitles();
  return snap.findAll({ role: "card" })
    .filter((c) => titles.has(c.label) && inside(c.bounds, bounds))
    .sort((a, b) => a.bounds.width * a.bounds.height - b.bounds.width * b.bounds.height)[0];
}

// The row `row` of the card `card`, in view.
function rowOf(card, row) {
  until(`${card} ${row}`, (s) => s.findAll({ role: "row", label: row }).some((r) => cardOf(r.bounds, s)?.label === card)
    || s.find({ role: "card", label: card }));
  return reveal((s) => s.findAll({ role: "row", label: row }).find((r) => cardOf(r.bounds, s)?.label === card)
    ?? s.find({ role: "card", label: card }), `${card} ${row}`);
}

// The control of `role` and `label` in `card`'s row `row`, in view.
function inRow(card, row, role, label) {
  const r = rowOf(card, row).bounds;
  const found = app.snapshot().findAll({ role, label }).filter((n) => inside(r, n.bounds))[0];
  if (!found) throw new Error(`no ${role} ${label} in ${card} ${row}`);
  return found;
}

// Pick `option` from the source chip of `card`'s `row`, which reads `now`.
function source(card, row, now, option) {
  app.click(inRow(card, row, "select", now));
  app.click(node("button", option));
  settle();
}

// Where each node card sits with the view at rest: sweep the view to the
// left edge of the graph, reading each card while it is wholly in view, and
// take the pan back out.
function placements() {
  app.click(node("button", "Actual size"));
  settle();
  const places = {};
  const titles = nodeTitles();
  let panned = 0;
  for (let i = 0; i < 12; i++) {
    const snap = app.snapshot();
    const c = canvas(snap);
    for (const card of snap.findAll({ role: "card" }).filter((n) => titles.has(n.label))) {
      if (!(card.label in places) && card.bounds.width > 200 && inside(c, card.bounds)) {
        places[card.label] = { x: card.bounds.x - panned, y: card.bounds.y };
      }
    }
    const step = c.width / 2;
    app.scroll({ x: c.x + c.width / 2, y: c.y + c.height - 8 }, { dx: step, steps: 2 });
    app.frames(1);
    panned += step;
  }
  app.click(node("button", "Actual size"));
  settle();
  return places;
}

// A wire is a text node "Clock 1 → Time 1 clock", or "k → t clock".
const wire = (from, to) => node("text", `${from} → ${to}`);

test("a Chase shows its nodes left to right with a wire into each input", () => {
  open("Chase");
  expect(stored().name).toBe("Chase");
  // A pill over the space, whose shift moves with time.
  const chain = ["k", "t", "move", "place", "pill", "Color 1"];
  for (const label of chain) node("card", label);
  wire("k", "t clock");
  wire("t", "move x");
  wire("move", "place shift");
  wire("place", "pill x");
  wire("pill", "Color 1 brightness");
  const places = placements();
  for (let i = 1; i < chain.length; i++) {
    assert(places[chain[i - 1]].x < places[chain[i]].x, `${chain[i - 1]} left of ${chain[i]}: ${JSON.stringify(places)}`);
  }
  expect(inRow("Color 1", "Brightness", "select", "Over space").label).toBe("Over space");
});

test("the output sits in view at rest, and the view comes back to it", () => {
  open("Chase");
  const color = () => app.snapshot().find({ role: "card", label: "Color 1" });
  assert(inside(canvas(), color().bounds) && color().bounds.width > 200, "Color 1 in view at rest");
  const c = canvas();
  app.drag({ x: c.x + 20, y: c.y + c.height - 20 }, { dx: 300, dy: -200 }, { steps: 6 });
  assert(!inside(canvas(), color().bounds) || color().bounds.width < 200, "the drag panned the view");
  app.click(node("button", "Actual size"));
  until("Color 1 in view again", () => inside(canvas(), color().bounds) && color().bounds.width > 200);
});

test("Over time on a Wash's brightness adds a time and a curve, wired", { fixture: { clips: [clipOf("Wash")] } }, () => {
  open("Wash");
  const before = Object.keys(nodes()).length;
  source("Color 1", "Brightness", "Value", "Over time");
  until("two nodes stored", () => Object.keys(nodes()).length === before + 2);
  const curve = nodes().color1.inputs.brightness.node;
  expect(nodes()[curve].kind).toBe("curve");
  expect(nodes()[nodes()[curve].inputs.x.node].kind).toBe("time");
  wire("Curve 1", "Color 1 brightness");
  wire("Time 1", "Curve 1 x");
  const places = placements();
  assert(places["Time 1"].x < places["Curve 1"].x && places["Curve 1"].x < places["Color 1"].x,
    `new nodes sit left of what they feed: ${JSON.stringify(places)}`);
  expect(inRow("Color 1", "Brightness", "select", "Over time").label).toBe("Over time");
});

// The Pulse's one clock.
const clockOf = () => Object.keys(nodes()).find((id) => nodes()[id].kind === "clock");

// Put Over time on the Pulse's alpha: a second time, with no clock.
function secondTime() {
  source("Color 1", "Alpha", "Value", "Over time");
  until("alpha over time", () => nodes().color1.inputs.alpha?.node);
  return nodes()[nodes().color1.inputs.alpha.node].inputs.x.node;
}

test("a wire dragged from a clock onto a second time links it, and dragged off unwires it", { fixture: { clips: [clipOf("Pulse")] } }, () => {
  open("Pulse");
  const time = secondTime();
  const clock = clockOf();
  shown("button", `${titleOf(clock)} output port`);
  app.drag(shown("button", `${titleOf(clock)} output port`), shown("button", `${titleOf(time)} clock port`), { steps: 8, restale: "match" });
  until("one clock shared", () => nodes()[time].inputs?.clock?.node === clock);
  expect(Object.values(nodes()).filter((n) => n.kind === "clock").length).toBe(1);
  wire(titleOf(clock), `${titleOf(time)} clock`);
  // Picked up off the input and let go over empty canvas: the time runs once
  // over the clip again.
  const c = canvas();
  const port = shown("button", `${titleOf(time)} clock port`);
  app.drag(port, { dx: 0, dy: c.y + c.height - 12 - (port.bounds.y + port.bounds.height / 2) }, { steps: 6 });
  until("unwired", () => nodes()[time].inputs?.clock === undefined);
  const first = Object.keys(nodes()).find((id) => id !== time && nodes()[id].kind === "time");
  expect(nodes()[first].inputs.clock.node).toBe(clock);
});

test("a wire the input cannot take is refused and says why", { fixture: { clips: [clipOf("Pulse")] } }, () => {
  open("Pulse");
  const before = JSON.stringify(stored().graph);
  // A clock feeds a time's clock, never a number.
  const clock = titleOf(clockOf());
  const time = titleOf(nodes()[nodes().color1.inputs.brightness.node].inputs.x.node);
  shown("button", `${clock} output port`);
  app.drag(shown("button", `${clock} output port`), shown("button", `${time} phase port`), { steps: 8, restale: "match" });
  node("text", `${clock} cannot feed ${time} phase`);
  expect(JSON.stringify(stored().graph)).toBe(before);
});

test("a node from the add menu joins the graph when its output is wired", { fixture: { clips: [clipOf("Wash")] } }, () => {
  open("Wash");
  app.click(node("button", "Add node"));
  app.click(node("button", "Noise"));
  const draft = node("card", "New noise");
  expect(Object.keys(nodes()).length).toBe(1);
  app.drag(shown("button", "New noise output port"), shown("button", "Color 1 brightness port"), { steps: 8, restale: "match" });
  until("noise stored", () => Object.values(nodes()).some((n) => n.kind === "noise"));
  const curve = nodes()[nodes().color1.inputs.brightness.node];
  expect(curve.kind).toBe("curve");
  expect(nodes()[curve.inputs.x.node].kind).toBe("noise");
  until("the draft gone", (s) => s.find({ role: "card", label: "New noise" }) === undefined);
  expect(draft.label).toBe("New noise");
});

// The Chase's space.
const spaceOf = () => Object.keys(nodes()).find((id) => nodes()[id].kind === "space");

test("deleting a node gives its input back, and undo brings it back in one step", () => {
  open("Chase");
  const before = JSON.stringify(stored().graph);
  const space = spaceOf();
  const curve = nodes().color1.inputs.brightness.node;
  app.click(shown("button", `Delete ${titleOf(space)}`));
  // The space's curve reads nothing without it, so it goes too; the
  // brightness it fed is back to its empty value.
  until("space deleted", () => !nodes()[space] && !nodes()[curve]);
  expect(nodes().color1.inputs?.brightness).toBe(undefined);
  until("the card gone", (s) => s.find({ role: "card", label: titleOf(space) }) === undefined);
  app.key("secondary-z");
  until("undone", () => JSON.stringify(stored().graph) === before);
  node("card", titleOf(space));
});

test("a space kind segment stores the setting", () => {
  open("Chase");
  const space = spaceOf();
  const card = shown("card", titleOf(space)).bounds;
  app.click(app.snapshot().findAll({ role: "button", label: "Angle" }).find((n) => inside(card, n.bounds)));
  until("angle stored", () => nodes()[space].settings.kind === "angle");
  // An angle wraps by default, so the wrap follows the kind.
  expect(nodes()[space].settings.wrap).toBe("yes");
});

test("dragging a curve point stores new points, and undo takes the drag back", { fixture: { clips: [clipOf("Pulse")] } }, () => {
  open("Pulse");
  const before = JSON.stringify(stored().graph);
  const curve = nodes().color1.inputs.brightness.node;
  const title = titleOf(curve);
  const shape = () => nodes()[curve].inputs.shape.points;
  const points = JSON.stringify(shape());
  shown("card", `${title} strip`);
  const box = node("card", `${title} strip`).bounds;
  app.drag(shown("slider", `${title} point ${shape().length}`), { dx: 0, dy: -box.height / 2 }, { steps: 8 });
  until("points stored", () => JSON.stringify(shape()) !== points);
  const end = shape().at(-1);
  expect(end[0]).toBe(1);
  assert(end[1] > 0.2, `the end point did not rise: ${JSON.stringify(end)}`);
  app.key("secondary-z");
  until("undone", () => JSON.stringify(stored().graph) === before);
});

test("a curve widens its strip", { fixture: { clips: [clipOf("Pulse")] } }, () => {
  open("Pulse");
  const title = titleOf(nodes().color1.inputs.brightness.node);
  const narrow = shown("card", `${title} strip`).bounds.width;
  app.click(shown("button", `Widen ${title}`));
  node("button", `Narrow ${title}`);
  const wide = shown("card", `${title} strip`).bounds.width;
  assert(wide > narrow * 1.5, `the strip is ${wide} wide, was ${narrow}`);
});

const cardWidth = (label) => node("card", label).bounds.width;

test("the zoom buttons and a Ctrl-scroll zoom the cards, about the pointer", () => {
  open("Chase");
  const rest = cardWidth("Color 1");
  app.click(node("button", "Zoom in"));
  node("text", "Zoom 120%");
  assert(shown("card", "Color 1").bounds.width > rest * 1.1, "the card grew");
  app.click(node("button", "Actual size"));
  until("actual size", () => Math.abs(cardWidth("Color 1") - rest) < 0.5);
  // What is under the pointer stays under it while the view zooms.
  const card = shown("card", "Color 1").bounds;
  const at = { x: card.x + card.width / 2, y: card.y + 20 };
  app.scroll(at, { dy: 60, steps: 3, modifiers: ["secondary"] });
  until("zoomed", () => !app.snapshot().find({ role: "text", label: "Zoom 100%" }));
  // Measured on the card's left edge, which stays in view: the pointer
  // keeps its distance from it in card widths.
  const scale = Number(app.snapshot().findAll({ role: "text" }).find((n) => /^Zoom \d+%$/.test(n.label)).label.slice(5, -1)) / 100;
  assert(scale > 1.1, `zoomed to ${scale}`);
  const left = node("card", "Color 1").bounds.x;
  const share = (at.x - left) / (rest * scale);
  assert(Math.abs(share - (at.x - card.x) / rest) < 0.03, `the pointer's share of the card moved: ${share}`);
  // The range ends: many steps out stop at half size.
  for (let i = 0; i < 8; i++) app.click(node("button", "Zoom out"));
  node("text", "Zoom 50%");
  assert(Math.abs(cardWidth("Color 1") - rest / 2) < 1, "half size");
});

test("a middle-button drag pans, even over a card", () => {
  open("Chase");
  const before = node("card", "Color 1").bounds;
  app.drag({ x: before.x + before.width / 2, y: before.y + 60 }, { dx: -120, dy: 40 }, { steps: 6, button: "middle" });
  const after = node("card", "Color 1").bounds;
  assert(Math.abs(after.x - (before.x - 120)) < 1 && Math.abs(after.y - (before.y + 40)) < 1,
    `the card moved by ${after.x - before.x}, ${after.y - before.y}`);
});

test("hovering a wire lights both its ports", () => {
  open("Chase");
  const from = titleOf(nodes().color1.inputs.brightness.node);
  const ends = () => [app.snapshot().find({ role: "button", label: `${from} output port` }).bounds.width,
    app.snapshot().find({ role: "button", label: "Color 1 brightness port" }).bounds.width];
  shown("button", `${from} output port`);
  const [out, into] = ends();
  const mid = node("text", `${from} → Color 1 brightness`).bounds;
  app.scroll({ x: mid.x + mid.width / 2, y: mid.y + mid.height / 2 }, { dy: 0 });
  until("both ends lit", () => ends()[0] > out && ends()[1] > into);
});

test("a wire dropped near a port that takes it snaps onto it", { fixture: { clips: [clipOf("Pulse")] } }, () => {
  open("Pulse");
  const time = secondTime();
  const clock = clockOf();
  shown("button", `${titleOf(clock)} output port`);
  const from = shown("button", `${titleOf(clock)} output port`).bounds;
  const to = shown("button", `${titleOf(time)} clock port`).bounds;
  const start = { x: from.x + from.width / 2, y: from.y + from.height / 2 };
  // 18 px short of the port: outside its ring, inside its reach.
  app.drag(start, { dx: to.x + to.width / 2 - start.x - 18, dy: to.y + to.height / 2 - start.y }, { steps: 8 });
  until("linked", () => nodes()[time].inputs?.clock?.node === clock);
});

test("Delete removes the selected node", () => {
  open("Chase");
  const space = spaceOf();
  const card = shown("card", titleOf(space)).bounds;
  // Its title takes the keyboard.
  app.drag({ x: card.x + 40, y: card.y + 22 }, { dx: 0, dy: 0 }, { steps: 1 });
  app.key("delete");
  until("space deleted", () => !nodes()[space]);
  expect(stored().name).toBe("Chase");
});

test("a direction is a plain U, V, Z vector, or best fit", () => {
  open("Chase");
  source(titleOf(spaceOf()), "Direction", "Best fit", "Value");
  until("a direction stored", () => Array.isArray(nodes()[spaceOf()].inputs?.direction));
  const field = (axis) => nav.inGraph((s) => s.findAll({ role: "input" })
    .find((n) => n.label.startsWith(`${titleOf(spaceOf())} direction: ${axis} = `)), `direction ${axis}`);
  for (const axis of ["u", "v", "z"]) field(axis);
  expect(app.snapshot().findAll({ role: "input" }).some((n) => /turn|tilt/.test(n.label))).toBe(false);
  app.click(field("v"));
  app.key("secondary-a backspace");
  app.type(field("v"), "0.5");
  app.key("enter");
  until("v stored", () => nodes()[spaceOf()].inputs.direction[1] === 0.5);
  expect(nodes()[spaceOf()].inputs.direction).toEqual([1, 0.5, 0]);
  source(titleOf(spaceOf()), "Direction", "Value", "Best fit");
  until("best fit again", () => nodes()[spaceOf()].inputs?.direction === undefined);
});

test("a direction shows each component to three decimals", () => {
  open("Chase");
  source(titleOf(spaceOf()), "Direction", "Best fit", "Value");
  until("a direction stored", () => Array.isArray(nodes()[spaceOf()].inputs?.direction));
  const field = (axis) => nav.inGraph((s) => s.findAll({ role: "input" })
    .find((n) => n.label.startsWith(`${titleOf(spaceOf())} direction: ${axis} = `)), `direction ${axis}`);
  for (const [axis, value, index] of [["u", "0.56583", 0], ["z", "0.67219", 2]]) {
    app.click(field(axis));
    app.key("secondary-a backspace");
    app.type(field(axis), value);
    app.key("enter");
    until(`${axis} stored`, () => nodes()[spaceOf()].inputs.direction[index] === Number(value));
  }
  expect(nodes()[spaceOf()].inputs.direction).toEqual([0.56583, 0, 0.67219]);
  until("three decimals", () => ["u = 0.566", "v = 0", "z = 0.672"]
    .every((end) => app.snapshot().find({ role: "input", label: `${titleOf(spaceOf())} direction: ${end}` })));
});

test("renaming stores the name and the timeline shows it", () => {
  open("Chase");
  const field = () => node("input", "Name");
  app.click(field());
  app.key("secondary-a backspace");
  app.type(field(), "Kick chase");
  app.key("enter");
  until("name stored", () => stored().name === "Kick chase");
  node("card", "Kick chase");
});

test("the fade handle writes an alpha curve", { fixture: { clips: [clipOf("Wash")], window: [1400, 900] } }, () => {
  nav.venue("Test Venue");
  nav.track("Aurora");
  nav.expand();
  nav.stageOff();
  until("the waveform", (s) => s.find({ role: "card", label: "Waveform" }));
  settle();
  const card = node("card", "Wash").bounds;
  app.drag(node("slider", "Wash fade in"), { dx: card.width / 4, dy: 0 }, { steps: 6 });
  until("alpha wired", () => nodes().color1.inputs.alpha?.node);
  const curve = nodes()[nodes().color1.inputs.alpha.node];
  expect(curve.kind).toBe("curve");
  expect(nodes()[curve.inputs.x.node].kind).toBe("time");
  const points = curve.inputs.shape.points;
  assert(Math.abs(points[0][1]) < 1e-9 && Math.abs(points[1][0] - 0.25) < 0.02, `fade: ${JSON.stringify(points)}`);
});

test("a noise source shows its preview in the noise card", { fixture: { clips: [clipOf("Wash")] } }, () => {
  open("Wash");
  source("Color 1", "Brightness", "Value", "Noise");
  until("noise stored", () => Object.values(nodes()).some((n) => n.kind === "noise"));
  const preview = shown("card", "Noise 1 preview");
  assert(inside(node("card", "Noise 1").bounds, preview.bounds), "the preview sits in the Noise 1 card");
  node("text", "Noise 1 over time");
  node("text", "Noise 1 along the rig");
  expect(app.snapshot().find({ label: "These settings give no noise to show" })).toBe(undefined);
});

test("a brightness multiplies by a list of values and wires", { fixture: { clips: [clipOf("Wash")] } }, () => {
  open("Wash");
  app.click(shown("button", "Multiply Color 1 brightness"));
  until("a list of two", () => JSON.stringify(nodes().color1.inputs?.brightness) === "[1,1]");
  // A new source dropped on a list joins it as one more item.
  source("Color 1", "Brightness", "List", "Over time");
  until("a wire in the list", () => nodes().color1.inputs.brightness.length === 3);
  const curve = nodes().color1.inputs.brightness[2].node;
  expect(curve).toBe("curve1");
  wire("Curve 1", "Color 1 brightness");
  // Taking items out leaves one: a plain value again.
  app.click(shown("button", "Remove Color 1 brightness item 3"));
  until("the wire gone", () => nodes().color1.inputs.brightness.length === 2 && !nodes()[curve]);
  app.click(shown("button", "Remove Color 1 brightness item 2"));
  until("one value", () => nodes().color1.inputs.brightness === 1);
});

// A number field's text, by the start of its label: "Space 1 shift = 0 %".
const fieldOf = (prefix) => nav.inGraph((s) => s.findAll({ role: "input" }).find((n) => n.label.startsWith(prefix)), prefix);

// The x of each head mark on a strip, left to right by head.
const heads = (strip) => {
  const marks = app.snapshot().findAll({ role: "text" }).filter((n) => n.label.startsWith(`${strip} head `));
  return marks.sort((a, b) => a.label.localeCompare(b.label, "en", { numeric: true })).map((n) => n.bounds.x);
};

test("a space shifts and stretches its heads, and a shift over time sweeps through", { fixture: { clips: [clipOf("Wash")] } }, () => {
  open("Wash");
  source("Color 1", "Brightness", "Value", "Over space");
  until("a space stored", () => nodes().space1);
  // An unshifted space over its whole length: the shift and length rows
  // show what empty stands for.
  for (const row of ["Shift", "Length"]) rowOf("Space 1", row);
  fieldOf("Space 1 shift = ");
  fieldOf("Space 1 length = ");
  shown("card", "Curve 1 strip");
  until("the heads marked", () => heads("Curve 1").length > 1);
  const before = heads("Curve 1");
  const field = fieldOf("Space 1 shift = ");
  app.click(field);
  app.key("secondary-a backspace");
  app.type(fieldOf("Space 1 shift = "), "50");
  app.key("enter");
  until("shift stored", () => nodes().space1.inputs?.shift === 0.5);
  // Every head slides back along the curve, or holds at its start.
  shown("card", "Curve 1 strip");
  until("the heads moved", () => heads("Curve 1").some((x, i) => x < before[i] - 0.5));
  const after = heads("Curve 1");
  after.forEach((x, i) => assert(x <= before[i] + 0.5, `head ${i + 1} moved right: ${before[i]} → ${x}`));
  // A moving shift starts with the band wholly before the axis and ends
  // wholly past it.
  source("Space 1", "Shift", "Value", "Over time");
  until("shift wired", () => nodes().space1.inputs?.shift?.node);
  const sweep = nodes()[nodes().space1.inputs.shift.node];
  const length = nodes().space1.inputs?.length ?? 1;
  assert(sweep.inputs.low <= -length && sweep.inputs.high >= 1, `sweep: ${JSON.stringify(sweep.inputs)}`);
});

// Open the name of the card titled `title` for typing, type `name`, enter.
function renameNode(title, name) {
  app.click(shown("text", title), { count: 2 });
  const field = node("input", `Rename ${title}`);
  app.click(field);
  app.key("secondary-a backspace");
  app.type(node("input", `Rename ${title}`), name);
  app.key("enter");
  settle();
}

test("renaming a node moves every wire into it, refuses a bad name, and undoes in one step", { fixture: { clips: [clipOf("Wash")] } }, () => {
  open("Wash");
  source("Color 1", "Brightness", "Value", "Over time");
  until("a curve stored", () => nodes().curve1);
  app.click(shown("button", "Multiply Color 1 brightness"));
  source("Color 1", "Alpha", "Value", "Link…");
  app.click(node("button", "Curve 1 · Ramp up"));
  // Each edit committed, so the rename is an undo step of its own.
  until("a list and a link stored", () => Array.isArray(nodes().color1.inputs.brightness) && nodes().color1.inputs.alpha?.node === "curve1");
  const before = JSON.stringify(stored().graph);
  const uses = (id) => (JSON.stringify(stored().graph).match(new RegExp(`"node":"${id}"`, "g")) ?? []).length;
  const wires = uses("curve1");
  assert(wires >= 2, `curve1 has ${wires} wires`);

  renameNode("Curve 1", "cut");
  until("renamed", () => nodes().cut && !nodes().curve1);
  expect(uses("cut")).toBe(wires);
  expect(uses("curve1")).toBe(0);
  expect(nodes().color1.inputs.alpha.node).toBe("cut");
  assert(nodes().color1.inputs.brightness.some((item) => item?.node === "cut"), "the list item follows the name");
  node("card", "cut");
  wire("cut", "Color 1 alpha");
  const renamed = JSON.stringify(stored().graph);

  // A builder's name and a name that starts with a digit are refused; the
  // card keeps its name and says why.
  for (const bad of ["time", "2x"]) {
    renameNode("cut", bad);
    until(`${bad} refused`, (s) => s.findAll({ role: "text" }).some((n) => n.label.includes(`"${bad}"`)));
    node("card", "cut");
    expect(JSON.stringify(stored().graph)).toBe(renamed);
  }

  app.key("secondary-z");
  until("undone", () => JSON.stringify(stored().graph) === before);
  node("card", "Curve 1");
});

test("a card's menu renames its node", { fixture: { clips: [clipOf("Wash")] } }, () => {
  open("Wash");
  source("Color 1", "Brightness", "Value", "Over time");
  until("a time stored", () => nodes().time1);
  app.click(shown("text", "Time 1"), { button: "right" });
  app.click(node("button", "Rename"));
  const field = node("input", "Rename Time 1");
  app.click(field);
  app.key("secondary-a backspace");
  app.type(node("input", "Rename Time 1"), "bar_clock");
  app.key("enter");
  until("renamed", () => nodes().bar_clock && !nodes().time1);
  expect(nodes()[nodes().color1.inputs.brightness.node].inputs.x.node).toBe("bar_clock");
  node("card", "bar_clock");
});
