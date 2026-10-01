// Color and brightness over space, from the outside: the source chip of the
// color and of the brightness offers "Over space". A color then reads a
// gradient along a space node, a brightness a curve along a space node; the
// space kind and wrap are stored on the node.

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
  nav.widenGraph();
}

const inside = (outer, inner) =>
  inner.x >= outer.x - 0.5 && inner.y >= outer.y - 0.5 &&
  inner.x + inner.width <= outer.x + outer.width + 0.5 && inner.y + inner.height <= outer.y + outer.height + 0.5;

// The `role` node labelled `label` on the graph card `card`, in view.
const inCard = (card, role, label) => nav.inCard(card, role, label);

// The control of `role` and `label` in `card`'s row `row`, in view.
function inRow(card, row, role, label) {
  const r = inCard(card, "row", row).bounds;
  const found = app.snapshot().findAll({ role, label }).find((n) => inside(r, n.bounds));
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
  inRow("Color 1", "Color", "select", "← Curve 1");
  // The gradient's curve is a card of its own; open, its strip.
  app.click(inCard("Curve 1", "button", "Expand Curve 1"));
  inCard("Curve 1", "card", "Curve 1 strip");

  // Another space kind is stored on the node.
  app.click(inCard("Space 1", "button", "Radial"));
  until("radial", () => nodes().space1.settings?.kind === "radial");

  // Value again gives back the white.
  source("Color 1", "Color", "← Curve 1", "Value");
  until("unwired", () => !nodes().color1.inputs.color?.node);
  expect(nodes().color1.inputs.color).toEqual([1, 1, 1]);
  expect(Object.values(nodes()).filter((n) => n.kind === "space").length).toBe(0);
});

test("a brightness over space reads a curve along a space node, and the wrap switch stores wrap", () => {
  open();
  source("Color 1", "Brightness", "Value", "Over space");
  until("the brightness wired", () => nodes().color1.inputs.brightness?.node);
  const curve = nodes()[nodes().color1.inputs.brightness.node];
  expect(curve.kind).toBe("curve");
  assert(curve.inputs.shape?.points && !curve.inputs.gradient, `a brightness reads a curve: ${JSON.stringify(curve)}`);
  const space = nodes()[curve.inputs.x.node];
  expect(space.kind).toBe("space");
  // A space is only a place per head: it has no offset or width.
  expect(space.inputs?.offset).toBe(undefined);
  expect(space.inputs?.width).toBe(undefined);

  // The axis's ends meet.
  app.click(inCard("Space 1", "button", "Wrap off"));
  until("wrap stored", () => nodes().space1.settings?.wrap === "yes");
  inCard("Space 1", "button", "Wrap on");
});
