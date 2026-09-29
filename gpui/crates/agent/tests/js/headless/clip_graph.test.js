// The clip graph inspector from the outside: a clip's graph is a tree of
// node cards under its output, each wired input showing the card of the node
// that feeds it. The source chip promotes a value and links a shared node;
// segments store settings; a strip stores curve points; the name field names
// the clip on the timeline; the fade handle writes alpha; undo takes a whole
// drag back.

// One clip of a shipped preset over beats 2–6 (seconds 1–3), keyed `graph-clip`.
const clipOf = (preset) => ({ pattern: "graph-clip", name: preset, start: 1, end: 3, preset });
fixture({ seconds: 20, clips: [clipOf("Chase")], rig: 4, window: [1400, 1400] });

const node = (role, label) => until(label, (s) => s.find({ role, label })).find({ role, label });
const settle = () => app.frames(4);
const stored = () => library.score().clips["graph-clip"];
const nodes = () => stored().graph.nodes;

function open(clip) {
  nav.venue("Test Venue");
  nav.track("Aurora");
  nav.expand();
  nav.stageOff();
  app.click(node("card", clip));
  until("the graph", (s) => s.find({ role: "card", label: "Clip graph" }) && s.find({ role: "input", label: "Name" }));
}

const inside = (outer, inner) =>
  inner.x >= outer.x - 0.5 && inner.y >= outer.y - 0.5 &&
  inner.x + inner.width <= outer.x + outer.width + 0.5 && inner.y + inner.height <= outer.y + outer.height + 0.5;
const area = (b) => b.width * b.height;

// The innermost node card holding `bounds`, in snapshot `snap`.
function cardOf(bounds, snap = app.snapshot()) {
  return snap.findAll({ role: "card" })
    .filter((c) => /^[A-Z][a-z]+ \d+$/.test(c.label) && inside(c.bounds, bounds))
    .sort((a, b) => area(a.bounds) - area(b.bounds))[0];
}

// Scroll the sheet until `target` is inside it.
function reveal(target) {
  const p = node("card", "Clip graph").bounds;
  const b = target.bounds;
  if (b.y < p.y + 90 || b.y + b.height > p.y + p.height - 12) {
    const dy = p.y + p.height / 2 - b.y;
    app.scroll({ x: p.x + p.width / 2, y: p.y + p.height / 2 }, { dy, steps: 5 });
    app.frames(3);
  }
}

// The row `row` of the card `card` itself, not of a card nested in it.
function rowOf(card, row) {
  const find = () => {
    const snap = app.snapshot();
    return snap.findAll({ role: "row", label: row }).find((r) => cardOf(r.bounds, snap)?.label === card);
  };
  until(`${card} ${row}`, () => find());
  reveal(find());
  return find();
}

// The `role` node labelled `label` whose innermost card is `card`.
function inCard(card, role, label) {
  const find = () => {
    const snap = app.snapshot();
    return snap.findAll({ role, label }).find((n) => cardOf(n.bounds, snap)?.label === card);
  };
  until(`${card} ${label}`, () => find());
  reveal(find());
  return find();
}

// The control of `role` and `label` in `card`'s row `row`, above any card
// nested in that row.
function inRow(card, row, role, label) {
  const r = rowOf(card, row).bounds;
  const snap = app.snapshot();
  const found = snap.findAll({ role, label })
    .filter((n) => inside(r, n.bounds) && cardOf(n.bounds, snap)?.label === card)
    .sort((a, b) => a.bounds.y - b.bounds.y)[0];
  if (!found) throw new Error(`no ${role} ${label} in ${card} ${row}`);
  return found;
}

// Pick `option` from the source chip of `card`'s `row`, which reads `now`.
function source(card, row, now, option) {
  app.click(inRow(card, row, "select", now));
  app.click(node("button", option));
  settle();
}

test("a Chase shows its name, the Color card and the tree under Brightness", () => {
  open("Chase");
  node("input", "Name");
  expect(stored().name).toBe("Chase");
  const color = node("card", "Color 1");
  const brightness = rowOf("Color 1", "Brightness");
  const curve = node("card", "Curve 2");
  // The curve feeding brightness is nested under that row, and the space
  // feeding the curve under the curve's own row.
  assert(inside(brightness.bounds, curve.bounds), "Curve 2 sits under Brightness");
  const x = rowOf("Curve 2", "X");
  assert(inside(x.bounds, node("card", "Space 1").bounds), "Space 1 sits under the curve's x");
  assert(inside(color.bounds, node("card", "Space 1").bounds), "the tree hangs under the Color card");
  expect(inRow("Color 1", "Brightness", "select", "Over space").label).toBe("Over space");
});

test("Over time on a Wash's brightness adds a time and a curve", { fixture: { clips: [clipOf("Wash")] } }, () => {
  open("Wash");
  const before = Object.keys(nodes()).length;
  source("Color 1", "Brightness", "Value", "Over time");
  until("two nodes stored", () => Object.keys(nodes()).length === before + 2);
  const wire = nodes().color1.inputs.brightness.node;
  expect(nodes()[wire].kind).toBe("curve");
  expect(nodes()[nodes()[wire].inputs.x.node].kind).toBe("time");
  node("card", "Time 1");
  node("card", "Curve 1");
  expect(inRow("Color 1", "Brightness", "select", "Over time").label).toBe("Over time");
});

test("a space kind segment stores the setting", () => {
  open("Chase");
  app.click(inCard("Space 1", "button", "Angle"));
  until("angle stored", () => nodes().space1.settings.kind === "angle");
  // An angle wraps by default, so the wrap follows the kind.
  expect(nodes().space1.settings.wrap).toBe("yes");
});

test("dragging a curve point stores new points", { fixture: { clips: [clipOf("Pulse")] } }, () => {
  open("Pulse");
  const before = JSON.stringify(nodes().curve1.inputs.shape.points);
  const point = node("slider", "Curve 1 point 3");
  reveal(point);
  const box = node("card", "Curve 1 strip").bounds;
  app.drag(node("slider", "Curve 1 point 3"), { dx: 0, dy: -box.height / 2 }, { steps: 6 });
  until("points stored", () => JSON.stringify(nodes().curve1.inputs.shape.points) !== before);
  const end = nodes().curve1.inputs.shape.points.at(-1);
  expect(end[0]).toBe(1);
  assert(end[1] > 0.2, `the end point did not rise: ${JSON.stringify(end)}`);
});

test("Link… on a second time input shares one clock and shows a link chip", { fixture: { clips: [clipOf("Pulse")] } }, () => {
  open("Pulse");
  source("Color 1", "Alpha", "Value", "Over time");
  until("a second time", () => nodes().time2);
  source("Time 2", "Clock", "Once", "Link…");
  app.click(node("button", "Clock 1"));
  until("one clock shared", () => nodes().time2.inputs?.clock?.node === "clock1");
  expect(Object.values(nodes()).filter((n) => n.kind === "clock").length).toBe(1);
  inCard("Time 2", "button", "→ Clock 1");
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

test("undo after a drag restores the graph in one step", { fixture: { clips: [clipOf("Pulse")] } }, () => {
  open("Pulse");
  const before = JSON.stringify(stored().graph);
  const point = node("slider", "Curve 1 point 3");
  reveal(point);
  const box = node("card", "Curve 1 strip").bounds;
  app.drag(node("slider", "Curve 1 point 3"), { dx: 0, dy: -box.height / 2 }, { steps: 8 });
  until("drag stored", () => JSON.stringify(stored().graph) !== before);
  app.key("secondary-z");
  until("undone", () => JSON.stringify(stored().graph) === before);
});
