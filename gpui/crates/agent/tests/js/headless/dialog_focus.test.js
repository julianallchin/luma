// Focus and dismissal for the production gpui-component Root and its modal
// trap.

fixture({ seconds: 8, clips: [{ pattern: "pattern-strobe", name: "Strobe", start: 1, end: 4 }] });

const dialog = (s) => s.find({ role: "card", label: "Settings dialog" });
const search = (s) => s.find({ role: "input", label: "Search tracks" });

function focusSearchThenOpenSettings() {
  nav.venue("Test Venue");
  app.click(search(until("the venue track search", search)));
  expect(search(app.snapshot()).focused).toBe(true);
  app.action("luma::OpenSettings");
  return until("the settings dialog", dialog);
}

test("a modal traps both tab directions and restores the exact opener", () => {
  expect(dialog(focusSearchThenOpenSettings()).focused).toBe(true);
  const focusStays = (keys, what) => {
    app.key(keys);
    app.frames(2);
    const shot = app.snapshot();
    const focused = shot.nodes.filter((n) => n.focused).map((n) => `${n.role}:${n.label}`);
    assert(dialog(shot)?.focused === true, `${what} left the dialog; focused: ${focused}`);
  };
  focusStays("tab", "forward tab");
  focusStays("shift-tab", "reverse tab");
  focusStays("shift-tab", "a wrapping reverse tab");

  app.key("escape");
  app.frames(2);
  const after = app.snapshot();
  expect(dialog(after)).toBe(undefined);
  expect(search(after)?.focused).toBe(true);
});

test("an optional dialog's scrim dismisses it and gives focus back", () => {
  focusSearchThenOpenSettings();
  app.click(until("the optional scrim", (s) => s.find({ role: "button", label: "Dismiss dialog" }))
    .find({ role: "button", label: "Dismiss dialog" }));
  app.frames(2);
  const after = app.snapshot();
  expect(dialog(after)).toBe(undefined);
  expect(search(after)?.focused).toBe(true);
});

test("required venue onboarding offers no scrim to dismiss it", { fixture: { track: false, seconds: 1, clips: [] } }, () => {
  const shot = until("required venue onboarding", (s) => s.find({ role: "card", label: "Venue dialog" }));
  expect(shot.find({ role: "button", label: "Dismiss dialog" })).toBe(undefined);
});
