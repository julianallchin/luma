// What the window's top corners look like in each state — the half the
// geometry test (`chrome_anchors`) cannot answer: the mode switch's chosen
// cell reads as *pressed* and the others do not, the agent toggle reads as
// pressed while the panel is up, and nothing paints into the gap beside it.
//
// Every read is confined to the title bar's band: a whole frame diffed for a
// 24px control is mostly waveform.

fixture({
  seconds: 20,
  clips: [{ pattern: "pattern-strobe", name: "Strobe", start: 2, end: 6 }],
  window: [1280, 800],
});

// Mean luma of a column span of the band, from a node's own box.
const lit = (shot, box, x, width) => image.stats(shot, { x, y: box.y, width, height: box.height }).meanLuma;

// Let the state settle, then shoot it with the controls where they are now.
function state() {
  app.frames(8, { waitMs: 40 });
  const frame = app.snapshot();
  return {
    shot: app.screenshot(),
    left: (frame.find({ role: "button", label: "Hide agent" }) ?? frame.find({ role: "button", label: "Show agent" })).bounds,
    rig: frame.find({ role: "toggle", label: "Rig" }).bounds,
    arrange: frame.find({ role: "toggle", label: "Arrange" }).bounds,
  };
}

test("the chosen mode reads as pressed and the open panel's toggle does", { timeoutMs: 180000 }, () => {
  nav.trackEditor("Test Venue", "Aurora");
  until("the timeline", (s) => s.find({ role: "card", label: "Waveform" }) !== undefined);
  const arrange = state();
  image.keep(arrange.shot, "chrome-anchors/arrange");

  app.action("luma::ShowRig");
  const rig = state();
  image.keep(rig.shot, "chrome-anchors/rig");

  app.action("luma::ToggleAgent");
  const agentClosed = state();
  image.keep(agentClosed.shot, "chrome-anchors/agent-closed");

  const cell = (s, which) => lit(s.shot, s[which], s[which].x, s[which].width);
  expect(cell(arrange, "arrange")).toBeGreaterThan(cell(arrange, "rig") + 1);
  expect(cell(rig, "rig")).toBeGreaterThan(cell(rig, "arrange") + 1);
  const left = (s) => lit(s.shot, s.left, s.left.x, s.left.width);
  expect(left(rig)).toBeGreaterThan(left(agentClosed) + 1);

  // The band yields the corner right of the agent toggle in every state.
  for (const s of [arrange, rig, agentClosed]) {
    expect(lit(s.shot, s.left, s.left.x + s.left.width, 10)).toBeLessThan(70);
  }
});
