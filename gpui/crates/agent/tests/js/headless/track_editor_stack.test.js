// Where a multi-clip vertical drag puts every clip it is holding.
//
// `rowToZ` is a function of the layer ladder as it stood when the pointer
// took hold: a selection dragged up mints a new layer above the top one,
// which renumbers the ladder, and a drag that re-read it would walk the rest
// of the selection a further lane on every mouse move. That is only
// observable when two clips share a layer, which is why this fixture has one.

// The editor labels a clip by its form, so each clip here plays a different
// form to be told apart on screen.
const FORMS = {
  alpha: { preset: "Wash", label: "Wash" },
  bravo: { preset: "Random heads", label: "Random heads" },
  charlie: { preset: "Clouds", label: "Clouds" },
  cap: { preset: "Strobe", label: "Strobe" },
};
const clip = (name, start, end, lane) =>
  ({ pattern: `pattern-${name}`, name, start, end, lane, preset: FORMS[name].preset });

// Three layers over four lanes, with Charlie and Cap sharing the top one.
// Alpha is on the floor and Charlie on the roof: dragging the pair up one lane
// asks both halves of `rowToZ` at once.
fixture({
  seconds: 20,
  clips: [clip("alpha", 2, 6, 0), clip("bravo", 2, 6, 1), clip("charlie", 2, 6, 2), clip("cap", 10, 14, 2)],
});

const node = (role, label) => app.snapshot().find({ role, label });
const card = (name) => node("card", FORMS[name].label);

// Which lane each clip is drawn in, by its header's top edge, counting from
// the topmost occupied one. Lanes, not pixels.
function stack() {
  const tops = {};
  for (const name of Object.keys(FORMS)) tops[name] = card(name)?.bounds.y;
  const order = [...new Set(Object.values(tops))].sort((a, b) => a - b);
  const out = {};
  for (const [name, y] of Object.entries(tops)) out[name] = order.indexOf(y);
  return out;
}

function open() {
  nav.track("Aurora");
  until("the timeline", (s) => s.find({ role: "card", label: "Waveform" }) !== undefined);
  nav.expand();
  nav.stageOff();
}

test("a group dragged up takes one lane each, however many moves it took", () => {
  nav.venue("Test Venue");
  app.frames(8);
  open();
  const opened = stack();
  // Charlie and Cap share the top layer; Alpha is on the floor.
  expect([opened.charlie, opened.cap, opened.bravo, opened.alpha]).toEqual([0, 0, 1, 2]);

  app.click(card("alpha"));
  app.frames(2);
  app.click(card("charlie"), { modifiers: ["shift"] });
  app.frames(2);
  expect(app.snapshot().findAll({ role: "text" }).map((n) => n.label)).toContain("2 selected");
  // One lane up, off Alpha's header. A lane's height is read, not assumed.
  const lane = app.snapshot().findAll({ role: "row" }).find((n) => n.label === "Lane 0").bounds.height;
  app.drag(card("alpha"), { dx: 0, dy: -lane });
  app.frames(20);
  const lifted = stack();
  // Outwait the debounce and the write's round trip.
  app.frames(20, { waitMs: 40 });
  nav.closeTab();
  app.frames(6);
  open();
  const stored = stack();

  // One lane's drag is one lane each: Alpha joins Bravo's layer, and Charlie
  // mints a layer above everything, leaving Cap behind. The failure this
  // pins moved Alpha two lanes, because the new layer renumbered the ladder
  // under the next mouse move.
  for (const [when, reading] of Object.entries({ lifted, stored })) {
    expect([reading.charlie, reading.cap]).toEqual([0, 1]);
    assert([reading.alpha, reading.bravo].sort().join() === "2,3",
      `${when}: same-priority overlaps need separate editable rows: ${JSON.stringify(reading)}`);
  }
  // The extra visible row does not change the layer the drag saved.
  const layers = {};
  for (const row of library.query("SELECT substr(id, instr(id, ':pattern-') + 9) AS name, z_index FROM clips")) {
    layers[row.name] = row.z_index;
  }
  expect(layers).toEqual({ alpha: 1, bravo: 1, charlie: 3, cap: 2 });
});

// Shift-selected clips copy and paste as a group, not just the last one
// pressed.
test(
  "a shift selection copies and pastes every clip",
  { fixture: { clips: [clip("alpha", 1, 3, 0), clip("bravo", 4, 5, 1)] } },
  () => {
    nav.venue("Test Venue");
    app.frames(8);
    open();
    until("the clips", () => card("bravo") !== undefined);
    const count = (name) => app.snapshot().findAll({ role: "card", label: FORMS[name].label }).length;
    app.click(card("alpha"));
    app.frames(2);
    app.click(card("bravo"), { modifiers: ["shift"] });
    app.frames(2);
    app.key("ctrl-c");
    // An empty spot well after both clips: the lane's middle.
    app.click(node("row", "Lane 0"));
    app.frames(2);
    app.key("ctrl-v");
    app.frames(20);
    expect({ alpha: count("alpha"), bravo: count("bravo") }).toEqual({ alpha: 2, bravo: 2 });
  },
);
