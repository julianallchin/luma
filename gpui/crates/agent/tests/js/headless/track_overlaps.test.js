// Clips that overlap exactly in one layer open as separate tracks, with
// separate hit targets, and an edit to one leaves the others alone.

// Three clips of one form, deliberately: identical spans are the case.
const chase = (pattern, seed) =>
  ({ pattern, name: "Chase", start: 1, end: 4, preset: "Chase", seed });

fixture({ seconds: 20, clips: [chase("a", 0), chase("b", 1), chase("c", 2)], window: [1400, 1000] });

test("overlapping clips have separate clickable rows and independent edits", () => {
  nav.trackEditor("Test Venue", "Aurora");
  nav.expand();
  nav.stageOff();
  const clips = () => app.snapshot().findAll({ role: "card", label: "Chase" }).sort((a, b) => a.bounds.y - b.bounds.y);
  until("three overlapping clips", (s) => s.findAll({ role: "card", label: "Chase" }).length === 3);
  app.action("luma::FitLanes");
  const initial = clips().map((n) => n.bounds);
  for (let i = 1; i < initial.length; i++) {
    assert(initial[i].y >= initial[i - 1].y + initial[i - 1].height, "overlapping clip hit targets");
  }
  for (let i = 0; i < 3; i++) {
    app.click(clips()[i]);
    until("the clip's inspector", (s) => s.find({ role: "card", label: "Clip graph" }));
  }
  // Removing the middle clip leaves both others intact.
  app.click(clips()[1]);
  app.key("backspace");
  until("only the selected clip removed", (s) => s.findAll({ role: "card", label: "Chase" }).length === 2);
  app.frames(10, { waitMs: 50 });
  nav.closeTab();
  nav.track("Aurora");
  until("the edit persisted", (s) => s.findAll({ role: "card", label: "Chase" }).length === 2);
});
