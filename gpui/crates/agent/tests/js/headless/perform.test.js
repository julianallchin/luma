// The Perform tab: the venue's pages of cells, played in Perform and
// arranged in Setup.
//
// The venue has one Perform pattern, "Front wash", whose graph has a fader
// node, `drop`, and a toggle button, `pad`, on its alpha: its pad. Its cell
// is a column two places tall, so it shows its pad and, under it, its level
// and its `drop` fader. Tap and the master sit at
// the page's right. Nothing is followed, so the set runs on the tapped
// tempo.

const PATTERN = "8c1d2b8e-0000-4000-8000-000000000001";
const graph = JSON.stringify({
  version: 3,
  nodes: {
    fader: { kind: "control", settings: { name: "drop" } },
    curve1: { kind: "curve", inputs: { x: { node: "fader" } } },
    pad: { kind: "button", settings: { name: "pad", mode: "toggle" } },
    curve2: { kind: "curve", inputs: { x: { node: "pad" } } },
    color1: {
      kind: "color",
      inputs: { brightness: { node: "curve1" }, alpha: { node: "curve2" } },
    },
  },
});
const layout = JSON.stringify({
  pages: [
    {
      id: "page-main",
      name: "Main",
      columns: 8,
      rows: 4,
      oneAtATime: [],
      cells: [
        { id: "cell-wash", column: 0, row: 0, width: 1, height: 2,
          content: { kind: "pattern", patternId: PATTERN } },
        { id: "cell-tap", column: 7, row: 0, width: 1, height: 1, content: { kind: "tap" } },
        { id: "cell-master", column: 7, row: 1, width: 1, height: 3, content: { kind: "master" } },
      ],
    },
  ],
});
fixture({
  seconds: 1,
  rig: 2,
  window: [1400, 1100],
  sql: [
    `INSERT INTO perform_patterns (id, uid, venue_id, name, selection_json, z_index,
                                   blend_mode, seed, graph_json)
       VALUES ('${PATTERN}', '$PRINCIPAL', 'venue-main', 'Front wash',
               '{"expression":"all"}', 0, 'replace', '7', '${graph}')`,
    `INSERT INTO perform_layouts (id, uid, venue_id, layout_json)
       VALUES ('venue-main', '$PRINCIPAL', 'venue-main', '${layout}')`,
  ],
});

const pad = (s, name = "Front wash") =>
  s.find((n) => n.role === "button" && n.label.startsWith(`Pad: ${name}`));
// A fader's reading, published as `perform-fader-<control>-<cell> = 0.50`.
function reading(shot, control) {
  const prefix = `perform-fader-${control}-`;
  const node = shot.find((n) => n.role === "text" && n.label.startsWith(prefix));
  return node === undefined ? null : Number(node.label.split(" = ")[1]);
}
const key = PATTERN.replaceAll("-", "");

function open() {
  nav.venue("Test Venue");
  nav.live();
  return until("the pad", (s) => (pad(s) ? s : undefined));
}

function setup() {
  open();
  nav.step("Edit layout", "button", "Edit layout");
  return until("the grid to arrange", (s) => s.find({ role: "button", label: "Select Master" }));
}

test("a pattern's pad turns it on and off", () => {
  const opened = open();
  expect(pad(opened).label).toBe("Pad: Front wash");

  app.click(pad(opened));
  until("on", (s) => (pad(s)?.label === "Pad: Front wash (on)" ? s : undefined));
  // A toggle stays on after its release, past a read of the bank.
  app.frames(30, { waitMs: 60 });
  expect(pad(app.snapshot()).label).toBe("Pad: Front wash (on)");

  app.click(pad(app.snapshot()));
  until("off", (s) => (pad(s)?.label === "Pad: Front wash" ? s : undefined));
});

test("a tall cell's faders move its pattern's level and its own fader", () => {
  const opened = open();
  const level = `perform_p${key}_level`;
  const drop = `perform_p${key}_n_fader`;
  until("the level read from the bank", (s) => (reading(s, level) === 1 ? s : undefined));
  expect(reading(opened, drop)).toBe(0);

  // From the middle, up a quarter of its height: about 0.75, as the fill is
  // the value.
  const fader = (s) => s.find({ role: "slider", label: "Front wash drop" });
  const shot = app.snapshot();
  const box = fader(shot).bounds;
  app.drag(fader(shot), { dx: 0, dy: -Math.round(box.height / 4) }, { steps: 6 });
  const moved = reading(
    until("the fader up", (s) => {
      const value = reading(s, drop);
      return value > 0.65 && value < 0.85 ? s : undefined;
    }),
    drop,
  );

  // Past the fader's own hold and a read of the bank: the value is the bank's.
  app.frames(30, { waitMs: 60 });
  expect(reading(app.snapshot(), drop)).toBe(moved);
  expect(reading(app.snapshot(), level)).toBe(1);
});

test("with nothing followed, the set runs on the tapped tempo", () => {
  open();
  until("the tempo strip", (s) => s.find({ role: "card", label: "Tapped tempo: 120.0 BPM" }));
  nav.step("faster", "button", "BPM +1");
  until("121", (s) => s.find({ role: "card", label: "Tapped tempo: 121.0 BPM" }));
  until("the tap cell shows it too", (s) =>
    s.find((n) => n.role === "button" && n.label === "Tap pad, 121.0 BPM"),
  );
});

test("Setup's + makes a pattern in that place, lighting the group chosen", () => {
  setup();
  nav.step("an empty place", "button", "New cell, row 3, column 1");
  nav.step("a group", "toggle", "Group Left movers");
  until("the group chosen", (s) =>
    s.find({ role: "toggle", label: "Group Left movers" })?.focused ? s : undefined,
  );
  nav.step("a preset", "button", "Wash");

  // The new cell is selected, and the inspector holds its pattern.
  until("the new cell selected", (s) =>
    s.find({ role: "button", label: "Select Wash" })?.focused ? s : undefined,
  );
  until("its group", (s) => s.find({ role: "button", label: "Group: Left movers" }));

  nav.step("Done", "button", "Done");
  until("its pad", (s) => pad(s, "Wash"));
  // Below Front wash's two places, in the same column.
  const wash = pad(app.snapshot(), "Wash");
  const front = pad(app.snapshot()).bounds;
  expect(wash.bounds.y).toBeGreaterThan(front.y + front.height);
  expect(Math.abs(wash.bounds.x - front.x)).toBeLessThan(2);
});

test("a row set to play one at a time is marked in Perform", () => {
  setup();
  nav.step("row 1's switch", "toggle", "One at a time, row 1");
  until("on", (s) =>
    s.find({ role: "toggle", label: "One at a time, row 1" })?.focused ? s : undefined,
  );
  nav.step("Done", "button", "Done");
  until("the bracket", (s) => s.find({ role: "text", label: "Row 1 plays one at a time" }));
  expect(app.snapshot().find({ role: "text", label: "Row 2 plays one at a time" })).toBe(undefined);
});

test("Learn waits for an input for the cell's control; Cancel stops it", () => {
  setup();
  nav.step("the master", "button", "Select Master");
  until("not mapped", (s) => s.find((n) => n.role === "text" && n.label.startsWith("Not mapped")));
  nav.step("Learn", "button", "Learn Master");
  until("learning", (s) =>
    s.find((n) => n.role === "text" && n.label.startsWith("Press a pad or move a fader")),
  );
  nav.step("Cancel", "button", "Cancel learning");
  until("not learning", (s) =>
    s.find((n) => n.role === "text" && n.label.startsWith("Not mapped")) ? s : undefined,
  );
  expect(app.snapshot().find({ role: "button", label: "Learn Master" }) !== undefined).toBe(true);
});
