// The Perform tab: the venue's pages of cells, played in Perform and
// arranged in Setup.
//
// The venue has two Perform patterns. "Front wash" has a fader node, `drop`,
// and a toggle button, `pad`, on its alpha: its pad. Its cell is a column
// two places tall, so it shows its pad and, under it, its level and its
// `drop` fader. "Hue wash", beside it and as tall, has a live color, `hue`
// (named "main"), straight into its output's color, shown under its pad.
// A color cell of two swatches, red and blue, fading at once, sets `hue`.
// A target on the left movers sits beside it, and "Bar wash", whose pad
// starts on the next bar, beside that. Tap and the master sit at the page's
// right. Nothing is followed, so the set runs on the tapped tempo.

const PATTERN = "8c1d2b8e-0000-4000-8000-000000000001";
const HUE = "8c1d2b8e-0000-4000-8000-000000000002";
const BAR = "8c1d2b8e-0000-4000-8000-000000000003";
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
const hued = JSON.stringify({
  version: 3,
  nodes: {
    hue: { kind: "control", settings: { name: "main", kind: "color" } },
    pad: { kind: "button", settings: { name: "pad", mode: "toggle" } },
    curve1: { kind: "curve", inputs: { x: { node: "pad" } } },
    color1: {
      kind: "color",
      inputs: { color: { node: "hue" }, alpha: { node: "curve1" } },
    },
  },
});
const onTheBar = JSON.stringify({
  version: 3,
  nodes: {
    pad: { kind: "button", settings: { name: "pad", mode: "toggle", start: "bar" } },
    curve1: { kind: "curve", inputs: { x: { node: "pad" } } },
    color1: { kind: "color", inputs: { alpha: { node: "curve1" } } },
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
        { id: "cell-hue", column: 1, row: 0, width: 1, height: 2,
          content: { kind: "pattern", patternId: HUE } },
        { id: "cell-colors", column: 2, row: 0, width: 2, height: 1,
          content: {
            kind: "color",
            label: "Colors",
            swatches: [[1, 0, 0], [0, 0, 1]],
            fade: 0,
            unit: "beats",
            targets: [{ kind: "node", patternId: HUE, node: "hue" }],
          } },
        { id: "cell-left", column: 4, row: 0, width: 1, height: 1,
          content: { kind: "target", label: "", group: "left_movers" } },
        { id: "cell-bar", column: 5, row: 0, width: 1, height: 1,
          content: { kind: "pattern", patternId: BAR } },
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
               '{"expression":"all"}', 0, 'replace', '7', '${graph}'),
              ('${HUE}', '$PRINCIPAL', 'venue-main', 'Hue wash',
               '{"expression":"all"}', 1, 'replace', '7', '${hued}'),
              ('${BAR}', '$PRINCIPAL', 'venue-main', 'Bar wash',
               '{"expression":"all"}', 2, 'replace', '7', '${onTheBar}')`,
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
  // The layout and the first read of the follow session land apart, and the
  // tempo strip the read brings moves the grid down: wait for both before
  // pointing at a cell.
  return until("the pad, under the tempo strip", (s) =>
    pad(s) && s.find((n) => n.role === "card" && n.label.startsWith("Tapped tempo: ")) ? s : undefined,
  );
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

test("a pad that starts on the bar waits for it, then lights", () => {
  const bar = (s) => pad(s, "Bar wash");
  expect(bar(open()).label).toBe("Pad: Bar wash");

  // A tap starts a bar, so the next is a whole bar, two seconds, away.
  nav.step("a tap", "button", "Tap");
  app.click(bar(app.snapshot()));
  until("waiting for the bar", (s) =>
    bar(s)?.label === "Pad: Bar wash (waiting)" ? s : undefined,
  );
  until("on at the bar", (s) => (bar(s)?.label === "Pad: Bar wash (on)" ? s : undefined));
});

test("a pad pressed with a target held plays its pattern on the target's group", () => {
  const target = (s) =>
    s.find((n) => n.role === "button" && n.label.startsWith("Target: Left movers"));
  expect(target(open()).label).toBe("Target: Left movers");

  // One pointer holds the target and lets go over the pad.
  const centre = (b) => ({ x: b.x + b.width / 2, y: b.y + b.height / 2 });
  const holdThenPad = () => {
    const shot = app.snapshot();
    const [from, to] = [centre(target(shot).bounds), centre(pad(shot).bounds)];
    app.drag(target(shot), { dx: to.x - from.x, dy: to.y - from.y }, { steps: 4 });
  };
  holdThenPad();
  until("on the left movers", (s) =>
    pad(s)?.label === "Pad: Front wash (on: Left movers)" ? s : undefined,
  );
  until("the target let go", (s) =>
    target(s)?.label === "Target: Left movers" ? s : undefined,
  );

  // Pressed again with the same target held, that instance turns off.
  holdThenPad();
  until("off", (s) => (pad(s)?.label === "Pad: Front wash" ? s : undefined));
});

test("a tall cell's faders move its pattern's level and its own fader", () => {
  const opened = open();
  const level = `perform_p${key}_level`;
  const drop = `perform_p${key}_n_fader`;
  expect(reading(opened, level)).toBe(1);
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
  nav.step("faster", "button", "BPM +1");
  until("121", (s) => s.find({ role: "card", label: "Tapped tempo: 121.0 BPM" }));
  until("the tap cell shows it too", (s) =>
    s.find((n) => n.role === "button" && n.label === "Tap pad, 121.0 BPM"),
  );
});

test("a source picked in the deck panel is followed, and disconnecting returns to the tapped tempo", () => {
  open();
  const source = (s) => s.find((n) => n.role === "text" && n.label.startsWith("Source: "));
  // Nothing followed: the panel offers a source, and the set runs on the tap.
  until("the offer", (s) => s.find({ role: "button", label: "Add source" }));
  const tapped = (s) => s.find((n) => n.role === "card" && n.label.startsWith("Tapped tempo: "));
  until("the tapped tempo", tapped);

  // The picker opens in the panel, with the latency, in the played view.
  nav.step("the offer", "button", "Add source");
  until("the picker", (s) =>
    s.find({ role: "card", label: "Source picker" }) && s.find({ role: "slider", label: "Latency" }));
  nav.step("the virtual deck", "toggle", "Virtual");
  const followed = until("the virtual deck followed", (s) => (source(s) ? s : undefined));
  expect(source(followed).label).toMatch(/^Source: Virtual deck/);
  assert(followed.find({ role: "card", label: "Source picker" }) === undefined, "the picker stayed open after a pick");
  assert(followed.find({ role: "button", label: "Add source" }) === undefined, "a followed source still offers Add source");
  until("the title bar says so", (s) => s.find((n) => n.role === "text" && n.label.startsWith("Virtual deck")));

  // Disconnecting puts the set back on the tapped tempo.
  nav.step("disconnect", "button", "Disconnect source");
  until("the offer again", (s) => s.find({ role: "button", label: "Add source" }) && !source(s));
  until("the tapped tempo again", tapped);
  until("not following", (s) => s.find((n) => n.role === "text" && n.label.startsWith("Not following a DJ")));

  // Edit layout arranges the grid and nothing else: no source chooser in it.
  nav.step("Edit layout", "button", "Edit layout");
  const arranging = until("the grid to arrange", (s) => (s.find({ role: "button", label: "Select Master" }) ? s : undefined));
  for (const label of ["Rekordbox", "Pro DJ Link", "StageLinQ", "Virtual", "Off"]) {
    assert(arranging.find({ role: "toggle", label }) === undefined, `Edit layout still offers ${label}`);
  }
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

// A swatch, published as `Colors Swatch 2, #0000FF` and ` (on)` once chosen.
const swatch = (s, number) =>
  s.find((n) => n.role === "button" && n.label.startsWith(`Colors Swatch ${number},`));
const hex = (label) => label?.match(/#[0-9A-F]{6}/)?.[0];
// The pattern's live color, published as `Hue wash main #RRGGBB`.
const live = (s) =>
  s.find((n) => n.role === "text" && n.label.startsWith("Hue wash main #"));

test("pressing a swatch sets the live colors it targets", () => {
  open();
  const opened = until("the swatches", (s) => (swatch(s, 2) && live(s) ? s : undefined));
  const blue = hex(swatch(opened, 2).label);
  assert(hex(live(opened).label) !== blue, "the live color starts unset, not blue");

  app.click(swatch(opened, 2));
  const chosen = until("the swatch on", (s) =>
    swatch(s, 2)?.label.endsWith("(on)") ? s : undefined,
  );
  expect(swatch(chosen, 1).label.endsWith("(on)")).toBe(false);
  until("the target blue", (s) => (hex(live(s)?.label) === blue ? s : undefined));

  // The other swatch takes it back.
  const red = hex(swatch(app.snapshot(), 1).label);
  app.click(swatch(app.snapshot(), 1));
  until("the target red", (s) => (hex(live(s)?.label) === red ? s : undefined));
});

test("Setup edits a color cell's swatches and lists the live colors it can set", () => {
  setup();
  const cell = (s) => s.find({ role: "button", label: "Select Colors" });
  app.click(cell(until("the color cell", (s) => (cell(s) ? s : undefined))));
  const target = (s) => s.find({ role: "checkbox", label: "Hue wash · main" });
  const opened = until("its targets", (s) => (target(s) ? s : undefined));
  expect(target(opened).focused).toBe(true);
  const chips = (s) => s.findAll((n) => n.role === "toggle" && n.label.startsWith("Edit swatch"));
  expect(chips(opened).length).toBe(2);

  app.click(opened.find({ role: "button", label: "Add swatch" }));
  const added = until("a third swatch, open", (s) =>
    chips(s).length === 3 && s.find({ role: "button", label: "Remove swatch 3" }) ? s : undefined,
  );
  app.click(added.find({ role: "button", label: "Remove swatch 3" }));
  until("two again", (s) => (chips(s).length === 2 ? s : undefined));
});
