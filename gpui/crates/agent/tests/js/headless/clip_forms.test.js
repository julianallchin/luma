// Form clips from the outside: a placed form clip's sheet shows the form's
// inputs, and the sheet edits them — a choice, a promotion to a curve over
// time, a pick from the curve thumbnails and back to a fixed value, the
// gradient editor, the envelope editor, and a field that commits on blur.

// One clip of a shipped preset over beats 2–6 (seconds 1–3), keyed `form-clip`.
const clipOf = (form, preset) => ({ pattern: "form-clip", name: preset, start: 1, end: 3, preset: [form, preset] });
const CHASE = clipOf("color.chase@1", "Chase");
fixture({ seconds: 20, clips: [CHASE], rig: 4, window: [1400, 1000] });

const node = (role, label) => until(label, (s) => s.find({ role, label })).find({ role, label });
const settle = () => app.frames(16, { waitMs: 60 });
const formClip = () => library.score().clips["form-clip"];

function open() {
  nav.venue("Test Venue");
  nav.track("Aurora");
  nav.expand();
  nav.stageOff();
}

// Scroll the sheet until `target` is inside it, `margin` px clear of its foot.
function reveal(target, { margin = 12, to = null } = {}) {
  const p = node("card", "Clip inputs").bounds;
  const b = target.bounds;
  if (b.y < p.y + 70 || b.y + b.height > p.y + p.height - margin) {
    const dy = (to ?? p.y + p.height / 2) - b.y;
    app.scroll({ x: p.x + p.width / 2, y: p.y + p.height / 2 }, { dy, steps: 5 });
    app.frames(3);
  }
}

// The control of `role` and `label` inside the row named `row`.
function inRow(row, role, label, options) {
  reveal(node("row", row), options);
  const r = node("row", row).bounds;
  const found = app.snapshot().findAll({ role, label }).find((n) => n.bounds.y >= r.y && n.bounds.y < r.y + r.height);
  if (!found) throw new Error(`no ${role} ${label} in ${row}`);
  return found;
}

const buttons = () => app.snapshot().findAll({ role: "button" }).map((n) => n.label);

// A stored curve is the preset's shape scaled to the input's range: the same
// x everywhere, and every y the preset's times one factor.
function expectScaled(stored, preset) {
  const pairs = (curve) => [
    ...curve.points,
    ...(curve.segments ?? []).flatMap((s) => (s.bezier ? [s.bezier.control1, s.bezier.control2] : [])),
  ];
  const got = pairs(stored);
  const want = pairs(preset);
  expect(got.length).toBe(want.length);
  const top = Math.max(...want.map(([, y]) => y));
  const factor = Math.max(...got.map(([, y]) => y)) / top;
  assert(factor > 0, `the stored curve is flat: ${JSON.stringify(stored)}`);
  got.forEach(([x, y], i) => {
    assert(Math.abs(x - want[i][0]) < 1e-9 && Math.abs(y - want[i][1] * factor) < 1e-9,
      `point ${i} ${JSON.stringify([x, y])} is not ${JSON.stringify(want[i])} scaled by ${factor}`);
  });
}

test("a placed form clip's sheet shows the form's inputs, without alpha", { fixture: { clips: [], graph_score: { clips: {} } } }, () => {
  open();
  app.type(node("input", "Search presets…"), "chase");
  app.frames(2);
  app.click(node("row", "Chase"));
  until("the form inputs", (s) => s.find({ role: "row", label: "Travel" }));
  const rows = app.snapshot().findAll({ role: "row" }).map((n) => n.label);
  const order = ["Color", "Axis", "Every", "Travel", "Width", "Shape", "Path", "Boundary"];
  expect(rows.filter((label) => order.includes(label))).toEqual(order);
  // Alpha is edited on the timeline, not in the sheet.
  assert(!rows.includes("Alpha"), `the sheet has an Alpha row: ${rows}`);

  // A double-click on the clip keeps the inputs up.
  app.click(node("card", "Chase"), { count: 2 });
  app.frames(12, { waitMs: 40 });
  assert(app.snapshot().find({ role: "card", label: "Clip inputs" }), "a double-click closed the sheet");
});

test("the sheet edits a choice and promotes an input to a curve and back", () => {
  const curves = library.curves("every").map((c) => c.name);
  open();
  app.click(node("card", "Chase"));
  until("the form inputs", (s) => s.find({ role: "row", label: "Shape" }));

  // A named shape: the select stores it, and its editor stays shut.
  app.click(inRow("Shape", "select", "Hard"));
  app.click(node("button", "Comet"));
  until("comet", (s) => s.find({ role: "select", label: "Comet" }));
  settle();
  expect(app.snapshot().find({ role: "card", label: "Envelope curve" })).toBe(undefined);
  // Custom opens the shape's editor, and a named shape shuts it again.
  app.click(inRow("Shape", "select", "Comet"));
  app.click(node("button", "Custom"));
  until("the shape editor", (s) => s.find({ role: "card", label: "Envelope curve" }));
  app.click(inRow("Shape", "select", "Comet"));
  app.click(node("button", "Comet"));
  until("the shape editor hidden", (s) => !s.find({ role: "card", label: "Envelope curve" }));
  settle();
  const comet = library.shapes().find((s) => s.name === "Comet").value;
  expect(formClip().inputs.shape).toEqual(comet);

  // Every's mode menu: Fixed first, and a curve over time.
  app.click(inRow("Every", "select", "Fixed"));
  const offered = buttons().filter((l) => l === "Fixed" || l.startsWith("↗"));
  expect(offered[0]).toBe("Fixed");
  expect(offered).toContain("↗ Over time");
  app.click(node("button", "↗ Over time"));
  until("the curve editor", (s) => s.find({ role: "card", label: "Envelope curve" }));
  inRow("Every", "select", "↗ Over time");
  settle();

  // The popover shows one thumbnail per curve preset; a pick closes it and
  // the editor with it.
  app.click(inRow("Every", "select", "Custom"));
  node("button", "Swell");
  expect(buttons().filter((l) => curves.includes(l))).toEqual(curves);
  app.click(node("button", "Swell"));
  until("swell", (s) => s.find({ role: "select", label: "Swell" }));
  settle();
  expect(app.snapshot().find({ role: "button", label: "Swell" })).toBe(undefined);
  expect(app.snapshot().find({ role: "card", label: "Envelope curve" })).toBe(undefined);
  expect(formClip().inputs.every.type).toBe("time");

  // Back to fixed takes the curve's first value. Swell starts at zero, and a
  // speed stays above zero, so it lands on the least beats a speed takes —
  // less than one beat, which the field shows per beat.
  app.click(inRow("Every", "select", "↗ Over time"));
  app.click(node("button", "Fixed"));
  until("fixed again", (s) => s.find({ role: "select", label: "Fixed" }));
  inRow("Every", "select", "Fixed");
  settle();
  const every = formClip().inputs.every;
  expect(every.type).toBe("beats");
  assert(every.value > 0 && every.value < 1, `every came back as ${every.value} beats`);
  const fields = app.snapshot().findAll({ role: "input" }).map((n) => n.label).filter((l) => l.startsWith("Every"));
  expect(fields).toEqual([`Every: Beats = ${1 / every.value}`]);
});

test("a curve input offers its curves and Custom opens the editor", () => {
  open();
  app.click(node("card", "Chase"));
  until("the form inputs", (s) => s.find({ role: "row", label: "Width" }));
  app.click(inRow("Width", "select", "Fixed"));
  app.click(node("button", "↗ Over time"));
  // A flat curve is no preset: Custom, with its editor open.
  until("the curve editor", (s) => s.find({ role: "card", label: "Envelope curve" }));
  settle();
  // The editor drops its own presets beside the picker.
  const editorPresets = ["Hard", "Soft", "Triangle"].filter((l) => app.snapshot().find({ role: "button", label: l }));
  expect(editorPresets).toEqual([]);

  const chip = inRow("Width", "select", "Custom");
  app.click(chip);
  const grid = node("card", "Presets").bounds;
  // The popover lies over the editor; keep only its own thumbnails.
  const offered = library.curves("width").map((c) => c.name).concat(["Custom"]);
  const cells = app.snapshot().findAll({ role: "button" })
    .filter((n) => n.bounds.x >= grid.x && n.bounds.y >= grid.y && n.bounds.x < grid.x + grid.width && n.bounds.y < grid.y + grid.height)
    .filter((n) => offered.includes(n.label));
  // The input's curves, then Custom.
  expect(cells.map((n) => n.label)).toEqual(offered);
  // A tidy grid: every row as full as the first, the last no longer.
  const rows = [];
  let last = NaN;
  for (const cell of cells) {
    if (cell.bounds.y !== last) {
      rows.push(0);
      last = cell.bounds.y;
    }
    rows[rows.length - 1] += 1;
  }
  assert(rows.slice(0, -1).every((n) => n === rows[0]) && rows.at(-1) <= rows[0], `grid rows ${rows}`);
  // The chip spans the column like every value control.
  expect(chip.bounds.width).toBeGreaterThan(250);
  assert(cells.every((n) => n.bounds.width >= 40), "a thumbnail is too narrow to read");
  // The popover hangs under the chip.
  assert(grid.y >= chip.bounds.y + chip.bounds.height, "the popover is not under the chip");

  app.click(node("button", "Swell"));
  until("picked", (s) => s.find({ role: "select", label: "Swell" }));
  settle();
  expect(app.snapshot().find({ role: "card", label: "Presets" })).toBe(undefined);
  // Picking a preset hides the editor again.
  expect(app.snapshot().find({ role: "card", label: "Envelope curve" })).toBe(undefined);

  // The pick stores the curve, scaled to the width's range.
  const width = formClip().inputs.width;
  expect(width.type).toBe("time");
  expectScaled(width.value, library.curves("width").find((c) => c.name === "Swell").curve);
});

test("the gradient editor adds, drags off, types hex and picks a preset",
  { fixture: { clips: [clipOf("color.time@1", "Color fade")] } }, () => {
    const stops = () => app.snapshot().findAll({ role: "slider" }).filter((n) => n.label.startsWith("graph-gradient:stop:"));
    const hex = () => app.snapshot().findAll({ role: "input" }).find((n) => n.label.startsWith("Stop color hex = "));
    const opacity = () => app.snapshot().findAll({ role: "input" }).find((n) => n.label.startsWith("Stop color opacity = "));
    const swatch = () => node("button", "Stop color swatch");
    const show = () => {
      if (!hex()) app.click(swatch());
      until("the picker", () => hex());
    };
    const shut = () => {
      if (hex()) {
        app.click(swatch());
        until("the picker closed", () => !hex());
      }
    };
    open();
    app.click(node("card", "Color over time"));
    until("the gradient", (s) => s.find({ role: "card", label: "graph-gradient bar" }));
    const before = stops().length;
    expect(before).toBe(2);
    // A click on the bar adds a stop.
    app.click(node("card", "graph-gradient bar"));
    until("a stop added", () => stops().length === before + 1);
    settle();
    // Hex and opacity live in the picker.
    assert(!hex() && !opacity(), "hex and opacity show outside the picker");
    show();
    // The new stop is selected and carries the bar's color there.
    expect(hex().label).toBe("Stop color hex = #9964A2");
    expect(opacity().label).toBe("Stop color opacity = 100");
    shut();
    // A stop dragged off goes.
    app.drag(stops()[1], { dx: 0, dy: 90 }, { steps: 6 });
    until("dragged off", () => stops().length === before);
    settle();
    show();
    app.click(hex());
    app.key("secondary-a backspace");
    app.type(hex(), "#00ff00");
    app.key("enter");
    until("the hex", () => hex().label === "Stop color hex = #00FF00");
    shut();
    settle();
    app.click(node("select", "Custom"));
    app.click(node("button", "Fire"));
    until("fire", (s) => s.find({ role: "select", label: "Fire" }));
    settle();

    const fire = library.gradients().find((g) => g.name === "Fire").gradient;
    const colors = formClip().inputs.colors;
    expect(colors.type).toBe("gradient");
    expect(colors.value.stops.length).toBe(fire.stops.length);
    colors.value.stops.forEach((stop, i) => {
      const want = fire.stops[i];
      assert(Math.abs(stop.t - want.t) < 1e-4 && stop.color.every((c, k) => Math.abs(c - want.color[k]) < 3e-3),
        `stop ${i} ${JSON.stringify(stop)} is not ${JSON.stringify(want)}`);
    });
  });

test("an envelope point dragged outside the editor keeps following and clamps",
  { fixture: { window: [1400, 1400] } }, () => {
    open();
    app.click(node("card", "Chase"));
    until("the form inputs", (s) => s.find({ role: "row", label: "Width" }));
    const low = { margin: 160, to: node("card", "Clip inputs").bounds.y + 140 };
    app.click(inRow("Width", "select", "Fixed", low));
    app.click(node("button", "↗ Over time"));
    until("the curve editor", (s) => s.find({ role: "card", label: "Envelope curve" }));
    settle();
    const box = node("card", "Envelope curve").bounds;
    // Down and to the left, far past the box, and released out there.
    app.drag(node("slider", "Envelope anchor 2"), { dx: -300, dy: box.height * 3 }, { steps: 8 });
    settle();
    const width = formClip().inputs.width;
    expect(width.type).toBe("time");
    // The end point keeps its x and follows the pointer to the bottom.
    expect(width.value.points.at(-1)).toEqual([1, 0]);
  });

test("a click elsewhere blurs a field and commits its value", () => {
  const field = () => app.snapshot().findAll({ role: "input" }).find((n) => n.label.startsWith("Width = "));
  open();
  app.click(node("card", "Chase"));
  until("the width", () => field());
  const typed = formClip().inputs.width.value === 1.5 ? 2.5 : 1.5;
  app.click(field());
  until("focused", () => field().focused);
  app.key("secondary-a backspace");
  app.type(field(), String(typed));
  app.frames(4);
  expect(field().focused).toBe(true);
  // A press on the sheet's own text, nowhere near the field.
  app.click(node("text", "Form"));
  until("blurred", () => !field().focused);
  app.frames(16, { waitMs: 60 });
  expect(field().label).toBe(`Width = ${typed}`);
  expect(formClip().inputs.width).toEqual({ type: "number", value: typed });
});

test("the Wash sheet shows brightness and every", { fixture: { clips: [clipOf("color.constant@1", "Pulse")] } }, () => {
  open();
  app.click(node("card", "Constant color"));
  until("the form inputs", (s) => s.find({ role: "row", label: "Brightness" }));
  const order = ["Color", "Brightness", "Every"];
  const rows = app.snapshot().findAll({ role: "row" }).map((n) => n.label);
  expect(rows.filter((label) => order.includes(label))).toEqual(order);
  const r = node("row", "Brightness").bounds;
  app.click(app.snapshot().findAll({ role: "select", label: "↗ Per hit" }).find((n) => n.bounds.y >= r.y && n.bounds.y < r.y + r.height));
  const offered = buttons().filter((l) => l === "Fixed" || l.startsWith("↗"));
  expect(offered).toEqual(["Fixed", "↗ Over time", "↗ Per hit", "↗ Noise", "↗ Audio"]);
  app.key("escape");
  app.frames(4, { waitMs: 40 });
  app.click(node("button", "Every Once"));
  until("once", (s) => !s.find({ role: "input", label: "Every: Beats = 1" }));
  app.frames(8, { waitMs: 40 });
  const clip = formClip();
  expect(clip.graph).toBe("color.constant@1");
  // Once stores 0 beats.
  expect(clip.inputs.every).toEqual({ type: "beats", value: 0 });
});

test("every sheet row has one shape", { fixture: { clips: [clipOf("color.sparkle@1", "Shimmer")], window: [1400, 1400] } }, () => {
  open();
  app.click(node("card", "Sparkle"));
  until("the rows", (s) => s.find({ role: "row", label: "Grain" }));
  app.frames(8, { waitMs: 40 });
  const snap = app.snapshot();
  const sheet = snap.find({ role: "card", label: "Clip inputs" }).bounds;
  const rows = snap.findAll({ role: "row" }).filter((r) => r.bounds.x >= sheet.x && r.bounds.x < sheet.x + sheet.width);
  const inside = (r, n) => n.bounds.y >= r.bounds.y && n.bounds.y < r.bounds.y + r.bounds.height && n.bounds.x >= r.bounds.x;
  const controls = snap.findAll((n) => n.role === "select" || n.role === "input");
  const out = rows.map((r) => {
    const own = controls.filter((n) => inside(r, n));
    return {
      label: r.label,
      head: own.filter((n) => n.bounds.y < r.bounds.y + 24).map((n) => n.bounds.width),
      body: own.filter((n) => n.bounds.y >= r.bounds.y + 24).map((n) => ({ w: n.bounds.width, y: n.bounds.y - r.bounds.y })),
    };
  });
  const labels = out.map((r) => r.label);
  // Row labels are sentence case.
  for (const label of labels) assert(label[0] === label[0].toUpperCase(), `row labels: ${labels}`);
  for (const want of ["Blend", "Selection", "Brightness", "Grain"]) expect(labels).toContain(want);
  const same = (xs) => xs.every((x) => Math.abs(x - xs[0]) < 1);
  // Mode menus in the header all have one width.
  const modes = out.flatMap((r) => r.head);
  assert(modes.length >= 3 && same(modes), `mode widths ${modes}`);
  // Value controls span the column, one control height, one gap under the
  // header.
  const bodies = out.filter((r) => r.label !== "Selection" && r.body.length > 0).map((r) => r.body[0]);
  assert(bodies.length >= 5, `value controls: ${JSON.stringify(out)}`);
  assert(same(bodies.map((b) => b.w)), `value widths ${bodies.map((b) => b.w)}`);
  assert(same(bodies.map((b) => b.y)), `label gaps ${bodies.map((b) => b.y)}`);
});
