// The window's two layouts, and Direct's graph page, as pictures. Kept for
// looking at (`luma-shots/view-mode/`); what is asserted is the half the
// headless test cannot see: that each switch changed what is drawn, and
// that the pill shows the change.

fixture({
  seconds: 20,
  rig: 4,
  clips: [{ pattern: "mode-clip", name: "Chase", start: 2, end: 6, preset: "Chase" }],
  window: [1440, 900],
});

const BAND = 38;

function shoot(name) {
  app.frames(12, { waitMs: 40 });
  const shot = app.screenshot();
  image.keep(shot, `view-mode/${name}`);
  return shot;
}

test("Direct, Program and the graph page each draw their own layout", { timeoutMs: 180000 }, () => {
  nav.trackEditor("Test Venue", "Aurora");
  until("the timeline", (s) => s.find({ role: "card", label: "Chase" }) !== undefined);
  const direct = shoot("direct");

  app.click(app.snapshot().find({ role: "card", label: "Chase" }), { count: 2, restale: "match" });
  until("the graph page", (s) => s.find({ role: "button", label: "Back to timeline" }) !== undefined);
  const page = shoot("direct-graph");

  app.click(app.snapshot().find({ role: "button", label: "Back to timeline" }), { restale: "match" });
  until("the timeline again", (s) => s.find({ role: "card", label: "Waveform" }) !== undefined);
  app.click(app.snapshot().find({ role: "toggle", label: "Program" }), { restale: "match" });
  until("Program", (s) => s.find({ role: "slider", label: "Score editor width" }) !== undefined);
  const program = shoot("program");

  // Below the band the three are different pictures…
  const body = { x: 0, y: BAND, width: 1440, height: 900 - BAND };
  for (const [a, b, what] of [[direct, page, "the graph page"], [direct, program, "Program"]]) {
    assert(image.diff(a, b, { rect: body }) > 0.05, `${what} drew the same window as Direct`);
  }
  // …and the pill itself shows which one is chosen.
  const pill = app.snapshot().find({ role: "card", label: "View mode" }).bounds;
  assert(image.diff(direct, program, { rect: pill }) > 0, "the pill did not change with the mode");
});
