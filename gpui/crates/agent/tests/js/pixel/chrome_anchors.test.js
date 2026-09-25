// What the window's top corners look like in each panel state — the half the
// geometry test (`chrome_anchors`) cannot answer: the right anchor reads as
// *pressed* while its panel is up, the left one as pressed while the sidebar
// is, and nothing paints into the gap beside the left one.
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

// Let the state settle, then shoot it with the toggles where they are now.
function state() {
  app.frames(8, { waitMs: 40 });
  const frame = app.snapshot();
  return {
    shot: app.screenshot(),
    left: frame.find({ role: "button", label: "sidebar-toggle" }).bounds,
    right: frame.find({ role: "button", label: "panel-toggle" }).bounds,
  };
}

test("the right anchor reads as pressed only while its panel is up", { timeoutMs: 180000 }, () => {
  nav.trackEditor("Test Venue", "Aurora");
  until("the timeline", (s) => s.find({ role: "card", label: "Waveform" }) !== undefined);
  const bothOpen = state();

  app.action("luma::ToggleWorkspace");
  const panelClosed = state();

  app.action("luma::ToggleWorkspace");
  app.action("luma::ToggleSidebar");
  const sidebarClosed = state();

  const right = (s) => lit(s.shot, s.right.x, s.right.width);
  const left = (s) => lit(s.shot, s.left.x, s.left.width);
  expect(right(bothOpen)).toBeGreaterThan(right(panelClosed) + 1);
  expect(left(bothOpen)).toBeGreaterThan(left(sidebarClosed) + 1);

  // The band yields the corner right of the left toggle in every state.
  for (const s of [bothOpen, panelClosed, sidebarClosed]) {
    expect(lit(s.shot, s.left.x + s.left.width, 10)).toBeLessThan(70);
  }
});
