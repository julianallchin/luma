// Color and brightness over space, from the outside: the source chip of the
// color and of the brightness offers "Over space". A color then reads a
// gradient along a space node, a brightness a curve along a space node whose
// offset moves over time; the space kind and wrap are stored on the node.

const WASH = { pattern: "graph-clip", name: "Wash", start: 1, end: 3, preset: "Wash" };
fixture({ seconds: 20, clips: [WASH], rig: 4, window: [1400, 1400] });

const node = (role, label) => until(label, (s) => s.find({ role, label })).find({ role, label });
const settle = () => app.frames(4);
const stored = () => library.score().clips["graph-clip"];
const nodes = () => stored().graph.nodes;

function open() {
  nav.venue("Test Venue");
  nav.track("Aurora");
  nav.expand();
  nav.stageOff();
  app.click(node("card", "Wash"));
  until("the graph", (s) => s.find({ role: "card", label: "Clip graph" }) && s.find({ role: "card", label: "Color 1" }));
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

test("a color over space reads a gradient along a space node", () => {
  open();
  source("Color 1", "Color", "Value", "Over space");
  until("the color wired", () => nodes().color1.inputs.color?.node);
  const curve = nodes()[nodes().color1.inputs.color.node];
  expect(curve.kind).toBe("curve");
  expect(curve.settings?.kind).toBe("color");
  const space = nodes()[curve.inputs.x.node];
  expect(space.kind).toBe("space");
  // The gradient ends at the color it had: the Wash's white.
  const stops = curve.inputs.gradient.stops;
  expect(stops.at(-1).color).toEqual([1, 1, 1]);
  node("card", "Space 1");
  inRow("Color 1", "Color", "select", "Over space");
  node("card", "Curve 2 strip");

  // Another space kind is stored on the node.
  app.click(inCard("Space 1", "button", "Radial"));
  until("radial", () => nodes().space1.settings?.kind === "radial");

  // Value again gives back the white.
  source("Color 1", "Color", "Over space", "Value");
  until("unwired", () => !nodes().color1.inputs.color?.node);
  expect(nodes().color1.inputs.color).toEqual([1, 1, 1]);
  expect(Object.values(nodes()).filter((n) => n.kind === "space").length).toBe(0);
});

test("a brightness over space moves over time, and the wrap switch stores wrap", () => {
  open();
  source("Color 1", "Brightness", "Value", "Over space");
  until("the brightness wired", () => nodes().color1.inputs.brightness?.node);
  const curve = nodes()[nodes().color1.inputs.brightness.node];
  expect(curve.kind).toBe("curve");
  assert(curve.inputs.shape?.points && !curve.inputs.gradient, `a brightness reads a curve: ${JSON.stringify(curve)}`);
  const space = nodes()[curve.inputs.x.node];
  expect(space.kind).toBe("space");
  expect(space.inputs.width).toBe(0.2);
  // The stroke moves: the offset is a curve over time.
  const offset = nodes()[space.inputs.offset.node];
  expect(offset.kind).toBe("curve");
  expect(nodes()[offset.inputs.x.node].kind).toBe("time");
  inRow("Space 1", "Offset", "select", "Over time");

  // The stroke's ends wrap.
  app.click(inCard("Space 1", "button", "Wrap off"));
  until("wrap stored", () => nodes().space1.settings?.wrap === "yes");
  inCard("Space 1", "button", "Wrap on");
});
