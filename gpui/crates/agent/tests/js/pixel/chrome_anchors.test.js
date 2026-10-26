// What the window's top corners look like in each state — the half the
// geometry test (`chrome_anchors`) cannot answer: the mode switch's chosen
// cell reads as *pressed* and its other cell does not, the left toggle reads
// as pressed while the sidebar is up, and nothing paints into the gap beside
// the left one.
//
// Every read is confined to the 38px head band (`luma_app::chrome::HEIGHT`):
// a whole frame diffed for a 24px control is mostly waveform.

fixture({
  seconds: 20,
  clips: [{ pattern: "pattern-strobe", name: "Strobe", start: 2, end: 6 }],
  window: [1280, 800],
});

const BAND = 38;

// Mean luma of a column span of the band, from a node's own box.
const lit = (shot, x, width) => image.stats(shot, { x, y: 0, width, height: BAND }).meanLuma;

// Let the state settle, then shoot it with the controls where they are now.
function state() {
  app.frames(8, { waitMs: 40 });
  const frame = app.snapshot();
  return {
    shot: app.screenshot(),
    left: frame.find({ role: "button", label: "sidebar-toggle" }).bounds,
    direct: frame.find({ role: "toggle", label: "Direct" }).bounds,
    program: frame.find({ role: "toggle", label: "Program" }).bounds,
  };
}

test("the chosen mode reads as pressed and the open sidebar's toggle does", { timeoutMs: 180000 }, () => {
  nav.trackEditor("Test Venue", "Aurora");
  until("the timeline", (s) => s.find({ role: "card", label: "Waveform" }) !== undefined);
  const direct = state();
  image.keep(direct.shot, "chrome-anchors/direct");

  app.action("luma::ProgramMode");
  const program = state();
  image.keep(program.shot, "chrome-anchors/program");

  app.action("luma::ToggleSidebar");
  const sidebarClosed = state();

  const cell = (s, which) => lit(s.shot, s[which].x, s[which].width);
  expect(cell(direct, "direct")).toBeGreaterThan(cell(direct, "program") + 1);
  expect(cell(program, "program")).toBeGreaterThan(cell(program, "direct") + 1);
  const left = (s) => lit(s.shot, s.left.x, s.left.width);
  expect(left(program)).toBeGreaterThan(left(sidebarClosed) + 1);

  // The band yields the corner right of the left toggle in every state.
  for (const s of [direct, program, sidebarClosed]) {
    expect(lit(s.shot, s.left.x + s.left.width, 10)).toBeLessThan(70);
  }
});
