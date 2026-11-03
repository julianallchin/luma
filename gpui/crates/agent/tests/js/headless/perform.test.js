// The Perform tab: a fader for every live control a clip of the venue names.
// The control here is `drop`, read by one clip of a second score; dragging its
// fader sets the venue's control bank, and a later read of the bank keeps it.

const graph = JSON.stringify({
  version: 3,
  nodes: {
    fader: { kind: "control", settings: { name: "drop" } },
    curve1: { kind: "curve", inputs: { x: { node: "fader" } } },
    color1: { kind: "color", inputs: { brightness: { node: "curve1" } } },
  },
});
fixture({
  seconds: 1,
  window: [1400, 1100],
  sql: [
    `INSERT INTO scores (id, uid, track_id, venue_id, name)
       VALUES ('score-live', '$PRINCIPAL', 'track-aurora', 'venue-main', 'Live')`,
    `INSERT INTO clips (id, uid, score_id, graph, graph_json, start, duration, seed,
                        selection_json, blend_mode)
       VALUES ('clip-live', '$PRINCIPAL', 'score-live', '', '${graph}', 0, 4, '1',
               '{"expression":"all"}', 'replace')`,
  ],
});

const fader = (s) => s.find({ role: "slider", label: "drop" });
function reading(shot) {
  const node = shot.find((n) => n.role === "text" && n.label.startsWith("perform-fader-drop = "));
  return node === undefined ? null : Number(node.label.split(" = ")[1]);
}

test("a clip's control is a fader that sets the venue's control", () => {
  nav.venue("Test Venue");
  nav.step("the Perform row", "row", "Perform");
  const opened = until("the drop fader", (s) => (fader(s) ? s : undefined));
  expect(reading(opened)).toBe(0);
  expect(opened.find({ role: "toggle", label: "Off" })?.focused).toBe(true);

  // From the middle, up a quarter of its height: about 0.75, as the fill is
  // the value. The border and the pixel grid make it not exactly that.
  const box = fader(opened).bounds;
  app.drag(fader(opened), { dx: 0, dy: -Math.round(box.height / 4) }, { steps: 6 });
  const moved = reading(until("the fader up", (s) => reading(s) > 0.7 && reading(s) < 0.82 ? s : undefined));

  // Past the fader's own hold and a read of the bank: the value is the bank's.
  app.frames(30, { waitMs: 60 });
  expect(reading(app.snapshot())).toBe(moved);

  // Learn waits for an input; pressing it again stops waiting.
  nav.step("Learn", "button", "Learn drop");
  until("learning", (s) => s.find({ role: "button", label: "Learn drop" })?.focused === true);
  nav.step("Learn again", "button", "Learn drop");
  until("not learning", (s) => s.find({ role: "button", label: "Learn drop" })?.focused === false);
});
