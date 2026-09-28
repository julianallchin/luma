// A color's input across space, from the outside: the source menu of the
// color and of the brightness offers "Across space"; a color then shows its
// axis and a gradient along it, a brightness its axis and a curve, and a
// brightness that moves shows the stroke's path, travel and width.

const WASH = { pattern: "form-clip", name: "Wash", start: 1, end: 3, preset: ["color@1", "Wash"] };
fixture({ seconds: 20, clips: [WASH], rig: 4, window: [1400, 1400] });

const node = (role, label) => until(label, (s) => s.find({ role, label })).find({ role, label });
const settle = () => app.frames(16, { waitMs: 60 });
const formClip = () => library.score().clips["form-clip"];

function open() {
  nav.venue("Test Venue");
  nav.track("Aurora");
  nav.expand();
  nav.stageOff();
  app.click(node("card", "Color"));
  until("the form inputs", (s) => s.find({ role: "row", label: "Brightness" }));
}

// Scroll the sheet until `target` is inside it.
function reveal(target) {
  const p = node("card", "Clip inputs").bounds;
  const b = target.bounds;
  if (b.y < p.y + 70 || b.y + b.height > p.y + p.height - 12) {
    app.scroll({ x: p.x + p.width / 2, y: p.y + p.height / 2 }, { dy: p.y + p.height / 3 - b.y, steps: 5 });
    app.frames(3);
  }
}

// The control of `role` and `label` inside the row named `row`.
function inRow(row, role, label) {
  reveal(node("row", row));
  const r = node("row", row).bounds;
  const found = app.snapshot().findAll({ role, label }).find((n) => n.bounds.y >= r.y && n.bounds.y < r.y + r.height);
  if (!found) throw new Error(`no ${role} ${label} in ${row}`);
  return found;
}

test("a color across space shows its axis and the gradient along it", () => {
  open();
  expect(app.snapshot().find({ role: "row", label: "Axis" })).toBe(undefined);
  app.click(inRow("Color", "select", "Fixed"));
  app.click(node("button", "↗ Across space"));
  until("the axis row", (s) => s.find({ role: "row", label: "Axis" }));
  until("the gradient", (s) => s.find({ role: "card", label: "Color strip" }));
  inRow("Color", "select", "↗ Across space");
  // The axis row offers the axis presets, spans and, for X, a mirror.
  inRow("Axis", "select", "X");
  inRow("Axis", "select", "Selection");
  settle();

  const color = formClip().inputs.color;
  expect(color.type).toBe("space");
  expect(color.value.axis.source.kind).toBe("u");
  // The gradient starts at the color it had: the Wash's white.
  expect(color.value.gradient.stops[0].color).toEqual([1, 1, 1]);
  assert(color.value.move === undefined, "a color across space does not move");

  // Another axis is stored in the source.
  app.click(inRow("Axis", "select", "X"));
  app.click(node("button", "Radial"));
  until("radial", (s) => s.find({ role: "select", label: "Radial" }));
  settle();
  expect(formClip().inputs.color.value.axis.source.kind).toBe("radial");

  // Fixed again takes the gradient's start.
  app.click(inRow("Color", "select", "↗ Across space"));
  app.click(node("button", "Fixed"));
  until("fixed", (s) => !s.find({ role: "row", label: "Axis" }));
  settle();
  expect(formClip().inputs.color).toEqual({ type: "color", value: [1, 1, 1] });
});

test("a brightness across space shows a curve, and moving shows the stroke", () => {
  open();
  app.click(inRow("Brightness", "select", "Fixed"));
  app.click(node("button", "↗ Across space"));
  until("the axis row", (s) => s.find({ role: "row", label: "Axis" }));
  // A flat curve is no preset, so its editor is open.
  until("the curve", (s) => s.find({ role: "card", label: "Brightness strip" }));
  inRow("Along the axis", "select", "Custom");
  expect(app.snapshot().find({ role: "row", label: "Travel" })).toBe(undefined);
  settle();
  const still = formClip().inputs.brightness;
  expect(still.type).toBe("space");
  assert(still.value.curve && !still.value.gradient, `a brightness reads a curve: ${JSON.stringify(still)}`);

  app.click(inRow("Move", "button", "Brightness move Moving"));
  until("the stroke", (s) => s.find({ role: "row", label: "Travel" }));
  for (const label of ["Shape", "Path", "Width", "Ends"]) node("row", label);
  settle();
  const moving = formClip().inputs.brightness.value.move;
  assert(moving, "the brightness did not start moving");
  expect(moving.boundary).toBe("clip");
  expect(moving.width_relative).toBe(true);

  // The stroke's ends wrap.
  app.click(inRow("Ends", "button", "Brightness ends Wrap"));
  settle();
  expect(formClip().inputs.brightness.value.move.boundary).toBe("wrap");
});
