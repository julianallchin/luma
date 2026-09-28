// Form clips from the outside: a placed form clip's sheet shows the form's
// inputs, and the sheet edits them — a choice, a promotion to a curve over
// time, a pick from the curve thumbnails and back to a fixed value, a
// color's strip, a number's strip, and a field that commits on blur. A chase
// is a color whose brightness moves across space.

// One clip of a shipped preset over beats 2–6 (seconds 1–3), keyed `form-clip`.
const clipOf = (form, preset) => ({ pattern: "form-clip", name: preset, start: 1, end: 3, preset: [form, preset] });
const CHASE = clipOf("color@1", "Chase");
const WASH = clipOf("color@1", "Wash");
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
// x and ease everywhere, and every y the preset's times one factor.
function expectScaled(stored, preset) {
  const got = stored.points;
  const want = preset.points;
  expect(got.length).toBe(want.length);
  const top = Math.max(...want.map(([, y]) => y));
  const factor = Math.max(...got.map(([, y]) => y)) / top;
  assert(factor > 0, `the stored curve is flat: ${JSON.stringify(stored)}`);
  got.forEach(([x, y, ease], i) => {
    assert(Math.abs(x - want[i][0]) < 1e-9 && Math.abs(y - want[i][1] * factor) < 1e-9,
      `point ${i} ${JSON.stringify([x, y])} is not ${JSON.stringify(want[i])} scaled by ${factor}`);
    assert(JSON.stringify(ease) === JSON.stringify(want[i][2]),
      `point ${i} ease ${JSON.stringify(ease)} is not ${JSON.stringify(want[i][2])}`);
  });
}

test("a placed form clip's sheet shows the form's inputs, without alpha", { fixture: { clips: [], graph_score: { clips: {} } } }, () => {
  open();
  app.type(node("input", "Search presets…"), "chase");
  app.frames(2);
  app.click(node("row", "Chase"));
  until("the form inputs", (s) => s.find({ role: "row", label: "Travel" }));
  const rows = app.snapshot().findAll({ role: "row" }).map((n) => n.label);
  // The form's inputs, and the moving brightness's parts inside its row.
  const order = ["Color", "Brightness", "Axis", "Shape", "Move", "Path", "Travel", "Width", "Ends", "Every"];
  expect(rows.filter((label) => order.includes(label))).toEqual(order);
  inRow("Brightness", "select", "↗ Across space");
  // Alpha is edited on the timeline, not in the sheet.
  assert(!rows.includes("Alpha"), `the sheet has an Alpha row: ${rows}`);

  // A double-click on the clip keeps the inputs up.
  app.click(node("card", "Color"), { count: 2 });
  app.frames(12, { waitMs: 40 });
  assert(app.snapshot().find({ role: "card", label: "Clip inputs" }), "a double-click closed the sheet");
});

test("the sheet edits a choice and promotes an input to a curve and back", () => {
  const curves = library.curves("every").map((c) => c.name);
  open();
  app.click(node("card", "Color"));
  until("the form inputs", (s) => s.find({ role: "row", label: "Shape" }));

  // A named shape: the select stores it, and its editor stays shut.
  app.click(inRow("Shape", "select", "Hard"));
  app.click(node("button", "Comet"));
  until("comet", (s) => s.find({ role: "select", label: "Comet" }));
  settle();
  expect(app.snapshot().find({ role: "card", label: "Brightness strip" })).toBe(undefined);
  // Custom opens the shape's editor, and a named shape shuts it again.
  app.click(inRow("Shape", "select", "Comet"));
  app.click(node("button", "Custom"));
  until("the shape editor", (s) => s.find({ role: "card", label: "Brightness strip" }));
  app.click(inRow("Shape", "select", "Comet"));
  app.click(node("button", "Comet"));
  until("the shape editor hidden", (s) => !s.find({ role: "card", label: "Brightness strip" }));
  settle();
  const comet = library.shapes().find((s) => s.name === "Comet").value.value;
  expect(formClip().inputs.brightness.value.curve).toEqual(comet);

  // Every's mode menu: Fixed first, and a curve over time.
  app.click(inRow("Every", "select", "Fixed"));
  const offered = buttons().filter((l) => l === "Fixed" || l.startsWith("↗"));
  expect(offered[0]).toBe("Fixed");
  expect(offered).toContain("↗ Over time");
  app.click(node("button", "↗ Over time"));
  until("the curve editor", (s) => s.find({ role: "card", label: "Every strip" }));
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
  expect(app.snapshot().find({ role: "card", label: "Every strip" })).toBe(undefined);
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

test("a curve input offers its curves and Custom opens the editor", { fixture: { clips: [WASH] } }, () => {
  open();
  app.click(node("card", "Color"));
  until("the form inputs", (s) => s.find({ role: "row", label: "Brightness" }));
  app.click(inRow("Brightness", "select", "Fixed"));
  app.click(node("button", "↗ Over time"));
  // A flat curve is no preset: Custom, with its editor open.
  until("the curve editor", (s) => s.find({ role: "card", label: "Brightness strip" }));
  settle();
  // The editor drops its own presets beside the picker.
  const editorPresets = ["Hard", "Soft", "Triangle"].filter((l) => app.snapshot().find({ role: "button", label: l }));
  expect(editorPresets).toEqual([]);

  const chip = inRow("Brightness", "select", "Custom");
  app.click(chip);
  const grid = node("card", "Presets").bounds;
  // The popover lies over the editor; keep only its own thumbnails.
  const offered = library.curves("brightness").map((c) => c.name).concat(["Custom"]);
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
  expect(app.snapshot().find({ role: "card", label: "Brightness strip" })).toBe(undefined);

  // The pick stores the curve, scaled to the brightness's range.
  const brightness = formClip().inputs.brightness;
  expect(brightness.type).toBe("time");
  expectScaled(brightness.value, library.curves("brightness").find((c) => c.name === "Swell").curve);
});

const colorStops = () => app.snapshot().findAll({ role: "slider" }).filter((n) => n.label.startsWith("Color point "));

test("a color strip adds a stop, moves it in x only, removes it and picks a preset",
  { fixture: { clips: [clipOf("color@1", "Color fade")] } }, () => {
    open();
    app.click(node("card", "Color"));
    until("the strip", (s) => s.find({ role: "card", label: "Color strip" }));
    const before = colorStops().length;
    expect(before).toBe(2);
    // A double-click adds a stop, here in the middle.
    app.click(node("card", "Color strip"), { count: 2 });
    until("a stop added", () => colorStops().length === before + 1);
    settle();
    const stored = () => formClip().inputs.color.value.gradient.stops;
    expect(stored().length).toBe(before + 1);
    // A drag moves the new stop along x; the drag's y goes nowhere.
    const b = node("card", "Color strip").bounds;
    const added = colorStops()[1];
    app.drag(added, { dx: b.width * 0.2, dy: 30 }, { steps: 6 });
    settle();
    const moved = colorStops()[1];
    assert(moved.bounds.x > added.bounds.x + b.width * 0.1, `the stop did not move right: ${added.bounds.x} → ${moved.bounds.x}`);
    expect(moved.bounds.y).toBe(added.bounds.y);
    const t = stored()[1].t;
    assert(t > 0.6 && t < 0.8, `the stored stop is at ${t}`);
    // A right-click removes it.
    app.click(colorStops()[1], { button: "right" });
    until("removed", () => colorStops().length === before);
    settle();
    expect(stored().length).toBe(before);

    // The strip's preset chip sits over it.
    const chip = app.snapshot().findAll({ role: "select" })
      .filter((n) => n.bounds.y < b.y)
      .sort((a, c) => c.bounds.y - a.bounds.y)[0];
    app.click(chip);
    app.click(node("button", "Fire"));
    until("fire", (s) => s.find({ role: "select", label: "Fire" }));
    settle();

    const fire = library.gradients().find((g) => g.name === "Fire").gradient;
    // A color fade is a gradient read once per hit.
    const color = formClip().inputs.color;
    expect(color.type).toBe("hit");
    expect(color.value.gradient.stops.length).toBe(fire.stops.length);
    color.value.gradient.stops.forEach((stop, i) => {
      const want = fire.stops[i];
      assert(Math.abs(stop.t - want.t) < 1e-4 && stop.color.every((c, k) => Math.abs(c - want.color[k]) < 3e-3),
        `stop ${i} ${JSON.stringify(stop)} is not ${JSON.stringify(want)}`);
    });
  });

test("a stop's swatch opens the color picker from a form row",
  { fixture: { clips: [clipOf("color@1", "Color fade")] } }, () => {
    const hex = () => app.snapshot().findAll({ role: "input" }).find((n) => n.label.startsWith("Color point hex = "));
    const opacity = () => app.snapshot().findAll({ role: "input" }).find((n) => n.label.startsWith("Color point opacity = "));
    open();
    app.click(node("card", "Color"));
    until("the strip", (s) => s.find({ role: "card", label: "Color strip" }));
    app.click(node("card", "Color strip"), { count: 2 });
    until("a stop added", () => colorStops().length === 3);
    settle();
    // Hex and opacity live in the picker.
    assert(!hex() && !opacity(), "hex and opacity show outside the picker");
    app.click(node("button", "Color point swatch"));
    until("the picker", () => hex());
    // The new stop is selected and carries the strip's color there.
    const [a, c] = [0, 2].map((i) => formClip().inputs.color.value.gradient.stops[i].color);
    const shown = hex().label.slice(-7);
    assert(shown !== "#000000" || a.concat(c).every((v) => v === 0), `the new stop shows ${shown}`);
    expect(opacity().label).toBe("Color point opacity = 100");
    app.click(hex());
    app.key("secondary-a backspace");
    app.type(hex(), "#00ff00");
    app.key("enter");
    until("the hex", () => hex().label === "Color point hex = #00FF00");
    settle();
    // Hex is sRGB; the stop stores light in linear Rec. 2020: sRGB green is
    // the green column of ITU-R BT.2087's matrix, inside sRGB.
    const green = formClip().inputs.color.value.gradient.stops[1].color;
    [0.3293, 0.9195, 0.0880].forEach((want, k) =>
      assert(Math.abs(green[k] - want) < 1e-3, `sRGB green stored as ${JSON.stringify(green)}`));
    assert(!app.snapshot().find({ role: "text", label: "Color point outside srgb" }), "sRGB green is inside sRGB");

    // The picker reaches Rec. 2020's own green, which sRGB cannot show.
    const hue = node("slider", "Color point:hue").bounds;
    app.drag({ x: hue.x + hue.width / 3 - 24, y: hue.y + hue.height / 2 }, { dx: 24, dy: 0 });
    const sv = node("slider", "Color point:sv").bounds;
    app.drag({ x: sv.x + sv.width / 2, y: sv.y + sv.height / 2 }, { dx: sv.width, dy: -sv.height });
    until("outside sRGB", (s) => s.find({ role: "text", label: "Color point outside srgb" }));
    settle();
    const wide = formClip().inputs.color.value.gradient.stops[1].color;
    assert(wide[1] > 0.99 && wide[0] < 0.1 && wide[2] < 0.1, `Rec. 2020 green stored as ${JSON.stringify(wide)}`);
  });

test("a number strip's point dragged outside it keeps following and clamps",
  { fixture: { clips: [WASH], window: [1400, 1400] } }, () => {
    open();
    app.click(node("card", "Color"));
    until("the form inputs", (s) => s.find({ role: "row", label: "Brightness" }));
    const low = { margin: 160, to: node("card", "Clip inputs").bounds.y + 140 };
    app.click(inRow("Brightness", "select", "Fixed", low));
    app.click(node("button", "↗ Over time"));
    until("the curve editor", (s) => s.find({ role: "card", label: "Brightness strip" }));
    settle();
    const box = node("card", "Brightness strip").bounds;
    // Down and to the left, far past the box, and released out there.
    app.drag(node("slider", "Brightness point 2"), { dx: -300, dy: box.height * 3 }, { steps: 8 });
    settle();
    const brightness = formClip().inputs.brightness;
    expect(brightness.type).toBe("time");
    // The end point keeps its x and follows the pointer to the bottom.
    expect(brightness.value.points.at(-1)).toEqual([1, 0]);
  });

test("a click elsewhere blurs a field and commits its value", () => {
  const field = () => app.snapshot().findAll({ role: "input" }).find((n) => n.label.startsWith("Brightness: Width = "));
  const width = () => formClip().inputs.brightness.value.move.width;
  open();
  app.click(node("card", "Color"));
  until("the width", () => field());
  const typed = width().value === 1.5 ? 2.5 : 1.5;
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
  expect(field().label).toBe(`Brightness: Width = ${typed}`);
  expect(width()).toEqual({ type: "number", value: typed });
});

test("the Wash sheet shows brightness and every", { fixture: { clips: [clipOf("color@1", "Pulse")] } }, () => {
  open();
  app.click(node("card", "Color"));
  until("the form inputs", (s) => s.find({ role: "row", label: "Brightness" }));
  const order = ["Color", "Brightness", "Every"];
  const rows = app.snapshot().findAll({ role: "row" }).map((n) => n.label);
  expect(rows.filter((label) => order.includes(label))).toEqual(order);
  const r = node("row", "Brightness").bounds;
  app.click(app.snapshot().findAll({ role: "select", label: "↗ Per hit" }).find((n) => n.bounds.y >= r.y && n.bounds.y < r.y + r.height));
  const offered = buttons().filter((l) => l === "Fixed" || l.startsWith("↗"));
  expect(offered).toEqual(["Fixed", "↗ Over time", "↗ Per hit", "↗ Noise", "↗ Audio", "↗ Across space"]);
  app.key("escape");
  app.frames(4, { waitMs: 40 });
  app.click(node("button", "Every Once"));
  until("once", (s) => !s.find({ role: "input", label: "Every: Beats = 1" }));
  app.frames(8, { waitMs: 40 });
  const clip = formClip();
  expect(clip.graph).toBe("color@1");
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
