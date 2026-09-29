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
const isNode = (label) => /^[A-Z][a-z]+ \d+$/.test(label);

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
  return snap.findAll({ role: "card" })
    .filter((c) => isNode(c.label) && inside(c.bounds, bounds))
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
  app.click(node("button", "Fit graph"));
  settle();
  const places = {};
  let panned = 0;
  for (let i = 0; i < 12; i++) {
    const snap = app.snapshot();
    const c = canvas(snap);
    for (const card of snap.findAll({ role: "card" }).filter((n) => isNode(n.label))) {
      if (!(card.label in places) && card.bounds.width > 200 && inside(c, card.bounds)) {
        places[card.label] = { x: card.bounds.x - panned, y: card.bounds.y };
      }
    }
    const step = c.width / 2;
    app.scroll({ x: c.x + c.width / 2, y: c.y + c.height - 8 }, { dx: step, steps: 2 });
    app.frames(1);
    panned += step;
  }
  app.click(node("button", "Fit graph"));
  settle();
  return places;
}

// A wire is a text node "Clock 1 → Time 1 clock".
const wire = (from, to) => node("text", `${from} → ${to}`);

test("a Chase shows its nodes left to right with a wire into each input", () => {
  open("Chase");
  expect(stored().name).toBe("Chase");
  const chain = ["Clock 1", "Time 1", "Curve 1", "Space 1", "Curve 2", "Color 1"];
  for (const label of chain) node("card", label);
  wire("Clock 1", "Time 1 clock");
  wire("Time 1", "Curve 1 x");
  wire("Curve 1", "Space 1 offset");
  wire("Space 1", "Curve 2 x");
  wire("Curve 2", "Color 1 brightness");
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
  app.click(node("button", "Fit graph"));
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

test("a wire dragged from a clock onto a second time links it, and dragged off unwires it", { fixture: { clips: [clipOf("Pulse")] } }, () => {
  open("Pulse");
  source("Color 1", "Alpha", "Value", "Over time");
  until("a second time", () => nodes().time2);
  shown("button", "Clock 1 output port");
  app.drag(shown("button", "Clock 1 output port"), shown("button", "Time 2 clock port"), { steps: 8, restale: "match" });
  until("one clock shared", () => nodes().time2.inputs?.clock?.node === "clock1");
  expect(Object.values(nodes()).filter((n) => n.kind === "clock").length).toBe(1);
  wire("Clock 1", "Time 2 clock");
  // Picked up off the input and let go over empty canvas: the time runs once
  // over the clip again.
  const c = canvas();
  const port = shown("button", "Time 2 clock port");
  app.drag(port, { dx: 0, dy: c.y + c.height - 12 - (port.bounds.y + port.bounds.height / 2) }, { steps: 6 });
  until("unwired", () => nodes().time2.inputs?.clock === undefined);
  expect(nodes().time1.inputs.clock.node).toBe("clock1");
});

test("a wire the input cannot take is refused and says why", { fixture: { clips: [clipOf("Pulse")] } }, () => {
  open("Pulse");
  const before = JSON.stringify(stored().graph);
  // A clock feeds a time's clock, never a number.
  shown("button", "Clock 1 output port");
  app.drag(shown("button", "Clock 1 output port"), shown("button", "Time 1 phase port"), { steps: 8, restale: "match" });
  node("text", "Clock 1 cannot feed Time 1 phase");
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

test("deleting a node gives its input back, and undo brings it back in one step", () => {
  open("Chase");
  const before = JSON.stringify(stored().graph);
  app.click(shown("button", "Delete Space 1"));
  // The space's curve reads nothing without it, so it goes too; brightness
  // is back to its empty value.
  until("space deleted", () => !nodes().space1 && !nodes().curve2);
  expect(nodes().color1.inputs?.brightness).toBe(undefined);
  until("the card gone", (s) => s.find({ role: "card", label: "Space 1" }) === undefined);
  app.key("secondary-z");
  until("undone", () => JSON.stringify(stored().graph) === before);
  node("card", "Space 1");
});

test("a space kind segment stores the setting", () => {
  open("Chase");
  const card = shown("card", "Space 1").bounds;
  app.click(app.snapshot().findAll({ role: "button", label: "Angle" }).find((n) => inside(card, n.bounds)));
  until("angle stored", () => nodes().space1.settings.kind === "angle");
  // An angle wraps by default, so the wrap follows the kind.
  expect(nodes().space1.settings.wrap).toBe("yes");
});

test("dragging a curve point stores new points, and undo takes the drag back", { fixture: { clips: [clipOf("Pulse")] } }, () => {
  open("Pulse");
  const before = JSON.stringify(stored().graph);
  const points = JSON.stringify(nodes().curve1.inputs.shape.points);
  shown("card", "Curve 1 strip");
  const box = node("card", "Curve 1 strip").bounds;
  app.drag(shown("slider", "Curve 1 point 3"), { dx: 0, dy: -box.height / 2 }, { steps: 8 });
  until("points stored", () => JSON.stringify(nodes().curve1.inputs.shape.points) !== points);
  const end = nodes().curve1.inputs.shape.points.at(-1);
  expect(end[0]).toBe(1);
  assert(end[1] > 0.2, `the end point did not rise: ${JSON.stringify(end)}`);
  app.key("secondary-z");
  until("undone", () => JSON.stringify(stored().graph) === before);
});

test("a curve widens its strip", { fixture: { clips: [clipOf("Pulse")] } }, () => {
  open("Pulse");
  const narrow = shown("card", "Curve 1 strip").bounds.width;
  app.click(shown("button", "Widen Curve 1"));
  node("button", "Narrow Curve 1");
  const wide = shown("card", "Curve 1 strip").bounds.width;
  assert(wide > narrow * 1.5, `the strip is ${wide} wide, was ${narrow}`);
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
