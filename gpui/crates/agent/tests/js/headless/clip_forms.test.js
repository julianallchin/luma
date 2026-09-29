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
const settle = () => app.frames(2);
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

test("Color sources share one form and fades stay on the timeline", () => {
  open();
  app.click(node("card", "Color"));
  until("source inputs", s => s.find({role:"row", label:"Width"}));
  inRow("Brightness", "select", "Space");
  inRow("Offset", "select", "Time");
  const rows = app.snapshot().findAll({role:"row"}).map(n => n.label);
  for (const name of ["Color", "Brightness", "Axis", "Grain", "Offset", "Width", "Events", "Every"]) expect(rows).toContain(name);
  assert(!rows.includes("Clip fade") && !rows.includes("Alpha"), "fade belongs on the timeline");
});

test("numeric source inputs can take sources and return to fixed", () => {
  open(); app.click(node("card", "Color"));
  app.click(inRow("Width", "select", "Fixed"));
  app.click(node("button", "Time"));
  until("width source stored", () => formClip().inputs.brightness.value.width.type === "time");
  inRow("Width", "card", "Width strip");
  app.click(inRow("Width", "select", "Time"));
  app.click(node("button", "Fixed"));
  until("fixed width stored", () => formClip().inputs.brightness.value.width.type === "number");
  assert(formClip().inputs.brightness.value.width.value > 0);
});

test("events repeat, follow another input, or span the clip", {fixture:{clips:[WASH], window:[1400,1400]}}, () => {
  open(); app.click(node("card", "Color"));
  app.click(inRow("Brightness", "select", "Fixed")); app.click(node("button", "Time"));
  app.click(inRow("Events", "select", "Inherit")); app.click(node("button", "Repeat"));
  until("own events", () => formClip().inputs.brightness.value.events?.every?.value > 0);
  app.click(inRow("Events", "select", "Repeat")); app.click(node("button", "Over clip"));
  until("clip events", () => formClip().inputs.brightness.value.events?.every?.value === 0);
  assert(!("every" in formClip().inputs), "the clock belongs to its source");
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
    until("a stop added", () => colorStops().length === before + 1 && formClip().inputs.color.value.gradient.stops.length === before + 1);
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
    until("moved stop saved",()=>stored()[1].t>0.6);
    const t = stored()[1].t;
    assert(t > 0.6 && t < 0.8, `the stored stop is at ${t}`);
    // A right-click removes it.
    app.click(colorStops()[1], { button: "right" });
    until("removed", () => colorStops().length === before && formClip().inputs.color.value.gradient.stops.length === before);
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
    until("gradient preset saved",()=>formClip().inputs.color.value.gradient.stops.length===fire.stops.length);
    const color = formClip().inputs.color;
    expect(color.type).toBe("time");
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
    until("a stop added", () => colorStops().length === 3 && formClip().inputs.color.value.gradient.stops.length === 3);
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
    until("green saved",()=>formClip().inputs.color.value.gradient.stops[1].color[1]>0.9);
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
    until("wide color saved",()=>formClip().inputs.color.value.gradient.stops[1].color[1]>0.99);
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
    app.click(node("button", "Time"));
    until("the curve editor", (s) => s.find({ role: "card", label: "Brightness strip" }) && formClip().inputs.brightness.type === "time");
    settle();
    const box = node("card", "Brightness strip").bounds;
    // Down and to the left, far past the box, and released out there.
    app.drag(node("slider", "Brightness point 2"), { dx: -300, dy: box.height * 3 }, { steps: 8 });
    settle();
    until("curve drag saved", () => formClip().inputs.brightness.type === "time" && formClip().inputs.brightness.value.points.at(-1)[1] === 0);
    const brightness = formClip().inputs.brightness;
    expect(brightness.type).toBe("time");
    // The end point keeps its x and follows the pointer to the bottom.
    expect(brightness.value.points.at(-1)).toEqual([1, 0]);
  });

test("a click elsewhere blurs a field and commits its value", () => {
  const field = () => app.snapshot().findAll({ role: "input" }).find((n) => n.label.startsWith("Width = "));
  const width = () => formClip().inputs.brightness.value.width;
  open();
  app.click(node("card", "Color"));
  until("the width", () => field());
  const typed = width().value === 1.5 ? 2.5 : 1.5;
  reveal(field());
  app.click(field());
  until("focused", () => field().focused);
  app.key("secondary-a backspace");
  app.type(field(), String(typed));
  app.frames(4);
  expect(field().focused).toBe(true);
  // A press on the sheet's own text, nowhere near the field.
  app.click(node("text", "Form"));
  until("blurred", () => !field().focused);
  until("width saved", () => width().value === typed);
  expect(field().label).toBe(`Width = ${typed}`);
  expect(width()).toEqual({ type: "number", value: typed });
});

test("Shimmer uses Random with shared grain", {fixture:{clips:[clipOf("color@1", "Shimmer")]}}, () => {
  open(); app.click(node("card", "Color"));
  inRow("Brightness", "select", "Random");
  app.click(inRow("Grain", "select", "Head")); app.click(node("button", "Fixture"));
  until("fixture grain", () => formClip().inputs.brightness.value.grain === "fixture");
  expect(formClip().graph).toBe("color@1");
});

test("spatial shapes retain their named presets",()=>{
  open();app.click(node("card","Color"));
  app.click(inRow("Brightness","select","Hard"));app.click(node("button","Comet"));
  const comet=library.shapes().find(s=>s.name==="Comet").value.value;
  until("comet saved",()=>JSON.stringify(formClip().inputs.brightness.value.curve)===JSON.stringify(comet));
});

test("timing inputs accept each numeric source with a valid default", {fixture:{clips:[clipOf("color@1","Pulse")],window:[1400,1400]}},()=>{
  open();app.click(node("card","Color"));
  let previous="Fixed";
  for(const name of ["Noise","Space","Random","Audio","Time"]) {
    app.click(inRow("Every","select",previous));app.click(node("button",name));
    until(`${name} clock saved`,()=>formClip().inputs.brightness.value.events.every.type===name.toLowerCase());
    previous=name;
  }
});


for (const preset of ["Pulse", "Color fade", "Chase"]) {
  test(`${preset} has no generic phase or amount controls`, {fixture:{clips:[clipOf("color@1",preset)]}},()=>{
    open();app.click(node("card","Color"));
    node("row","Events");
    const rows=app.snapshot().findAll({role:"row"}).map(n=>n.label);
    for (const extra of ["Phase","Amount","Level","Size"])
      assert(!rows.includes(extra),`${preset} exposes redundant ${extra}`);
  });
}
