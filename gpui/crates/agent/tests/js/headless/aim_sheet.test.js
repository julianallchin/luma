// An aim clip from the outside: placed from the preset browser, its sheet
// shows the base's rows and the motion's rows that apply, direction as turn
// and tilt, and a blend of Replace or Offset. A motion of none hides the
// shape's rows; Offset hides the base's rows.

fixture({ seconds: 20, graph_score: { clips: {} }, rig: 4, window: [1400, 1000] });

const node = (role, label) => until(label, (s) => s.find({ role, label })).find({ role, label });
const inSheet = (role) => {
  const p = node("card", "Clip inputs").bounds;
  const inside = (n) => n.bounds.x >= p.x && n.bounds.x < p.x + p.width;
  return app.snapshot().findAll({ role }).filter(inside).map((n) => n.label);
};
const settle = () => app.frames(16, { waitMs: 60 });
const onlyClip = () => {
  const clips = Object.values(library.score().clips);
  expect(clips.length).toBe(1);
  return clips[0];
};

// Aim's own Wave: the search matches the form's name, not Chase's.
function placeAimWave() {
  nav.venue("Test Venue");
  nav.track("Aurora");
  nav.expand();
  nav.stageOff();
  until("the waveform", (s) => s.find({ role: "card", label: "Waveform" }));
  app.type(node("input", "Search presets…"), "aim");
  app.frames(2);
  app.click(node("row", "Wave"));
  until("the aim inputs", (s) => s.find({ role: "row", label: "Motion" }));
  settle();
}

const AIM_ROWS = ["Blend", "Base", "Direction", "Point", "Fan", "Axis", "Motion", "Shape", "Size", "Every", "Spread", "Speed", "Alpha"];
const aimRows = () => inSheet("row").filter((label) => AIM_ROWS.includes(label));

test("an aim sheet shows the rows its base and motion use", () => {
  placeAimWave();
  // A direction base with a shape.
  expect(aimRows()).toEqual(["Blend", "Base", "Direction", "Fan", "Axis", "Motion", "Shape", "Size", "Every", "Spread"]);
  // Direction reads as turn and tilt, with the stored vector under them.
  const sliders = inSheet("slider");
  assert(sliders.some((l) => l.startsWith("Direction: Turn = ")) && sliders.some((l) => l.startsWith("Direction: Tilt = ")),
    `direction is not turn and tilt: ${sliders}`);
  assert(inSheet("text").some((l) => /^U -?[\d.]+ · V [−-]?[\d.]+ · Z [−-]?[\d.]+$/.test(l)), "no stored vector under turn and tilt");

  const r = node("row", "Motion").bounds;
  const motion = app.snapshot().findAll({ role: "select", label: "Shape" })
    .find((n) => n.bounds.y >= r.y && n.bounds.y < r.y + r.height);
  assert(motion, "no Shape select in the Motion row");
  app.click(motion);
  app.click(node("button", "None"));
  until("motion none", (s) => s.find({ role: "select", label: "None" }));
  settle();
  expect(aimRows()).toEqual(["Blend", "Base", "Direction", "Fan", "Axis", "Motion"]);

  const clip = onlyClip();
  expect(clip.graph).toBe("aim@1");
  expect(clip.blend_mode).toBe("replace");
  expect(clip.inputs.motion).toEqual({ type: "choice", value: "none" });
  // A hidden input keeps its value.
  expect(clip.inputs.shape).toEqual({ type: "choice", value: "swing_up_down" });
});

// An aim offers two blends. Offset turns the aim under the clip, so the
// base's rows go; Replace brings them back. Base and direction keep their
// values while hidden.
test("an aim blends replace or offset", () => {
  placeAimWave();
  const blend = () => {
    const r = node("row", "Blend").bounds;
    return app.snapshot().findAll({ role: "select" })
      .find((n) => n.bounds.y >= r.y && n.bounds.y < r.y + r.height);
  };
  expect(blend().label).toBe("Replace");
  app.click(blend());
  until("the blend menu", (s) => s.find({ role: "button", label: "Offset" }));
  const modes = app.snapshot().findAll({ role: "button" }).map((n) => n.label)
    .filter((l) => ["Replace", "Offset", "Add", "Multiply", "Screen"].includes(l));
  expect(modes).toEqual(["Replace", "Offset"]);
  const before = onlyClip();
  app.click(node("button", "Offset"));
  until("offset", (s) => s.find({ role: "select", label: "Offset" }));
  settle();
  expect(aimRows()).toEqual(["Blend", "Fan", "Axis", "Motion", "Shape", "Size", "Every", "Spread"]);
  const offset = onlyClip();
  expect(offset.blend_mode).toBe("offset");
  expect(offset.inputs.base).toEqual(before.inputs.base);
  expect(offset.inputs.direction).toEqual(before.inputs.direction);

  app.click(blend());
  until("the blend menu", (s) => s.find({ role: "button", label: "Replace" }));
  app.click(node("button", "Replace"));
  until("replace", (s) => s.find({ role: "select", label: "Replace" }));
  settle();
  expect(aimRows()).toEqual(["Blend", "Base", "Direction", "Fan", "Axis", "Motion", "Shape", "Size", "Every", "Spread"]);
  expect(onlyClip().blend_mode).toBe("replace");
});

// The axis rows offer the Mirror control of the old mapping editor: Off,
// Left–right, Front–back, Up–down and Custom plane, with the plane's normal
// for a custom plane and its offset whenever there is a mirror. Only a
// spatial axis along a line takes a mirror: order and random hide the row,
// and picking Random drops the mirror. Spread shows in degrees.
test("an aim axis offers the mirror control", () => {
  const fields = () => inSheet("slider").concat(inSheet("input"));
  const field = (list, prefix) => list.some((l) => l.startsWith(prefix));
  placeAimWave();
  assert(!inSheet("text").includes("Mirror"), "an order axis offered a mirror");
  // The spread the preset stored, shown in degrees.
  const spread = onlyClip().inputs.spread.value;
  assert(fields().concat(inSheet("text")).some((l) => l.includes(String(Math.round(spread)))),
    `the stored spread ${spread} is not shown in degrees`);

  app.click(node("select", "Order"));
  until("the axis menu", (s) => s.find({ role: "button", label: "X" }));
  app.click(node("button", "X"));
  until("the x axis", (s) => s.find({ role: "select", label: "X" }));
  settle();
  expect(inSheet("text")).toContain("Mirror");
  assert(!inSheet("text").includes("Offset"), "an offset showed without a mirror");

  app.click(node("select", "Off"));
  until("the mirror menu", (s) => s.find({ role: "button", label: "Custom plane" }));
  const mirrors = app.snapshot().findAll({ role: "button" }).map((n) => n.label)
    .filter((l) => ["Off", "Left–right", "Front–back", "Up–down", "Custom plane"].includes(l));
  expect(mirrors).toEqual(["Off", "Left–right", "Front–back", "Up–down", "Custom plane"]);
  app.click(node("button", "Custom plane"));
  until("the custom plane", (s) => s.find({ role: "select", label: "Custom plane" }));
  settle();
  expect(inSheet("text")).toContain("Normal · U, V, Z");
  expect(inSheet("text")).toContain("Offset");
  assert(field(fields(), "Axis: Mirror U") && field(fields(), "Axis: Mirror offset"), `custom plane fields: ${fields()}`);

  app.click(node("select", "Custom plane"));
  until("the mirror menu", (s) => s.find({ role: "button", label: "Front–back" }));
  app.click(node("button", "Front–back"));
  until("front–back", (s) => s.find({ role: "select", label: "Front–back" }));
  settle();
  // A fixed plane shows only its offset.
  assert(!inSheet("text").includes("Normal · U, V, Z"), "a fixed plane showed a normal");
  expect(inSheet("text")).toContain("Offset");
  assert(field(fields(), "Axis: Mirror offset"), `fixed plane fields: ${fields()}`);

  app.click(node("select", "X"));
  until("the axis menu", (s) => s.find({ role: "button", label: "Random" }));
  app.click(node("button", "Random"));
  until("random", (s) => s.find({ role: "select", label: "Random" }));
  settle();
  assert(!inSheet("text").includes("Mirror"), "a random axis kept its Mirror row");

  const axis = onlyClip().inputs.axis.value;
  expect(axis.source.kind).toBe("random");
  expect(axis.mirror).toBe(undefined);
});
