// Focus and dismissal for the production gpui-component Root and its modal
// trap.

fixture({ seconds: 8, clips: [{ pattern: "pattern-strobe", name: "Strobe", start: 1, end: 4 }] });

// Chat history is the optional dialog here. Settings, which this used to
// open with ⌘, from a focused field, is a page now; that it hands the
// keyboard back to the exact field that opened it is `settings.test.js`'s.
const dialog = (s) => s.find({ role: "card", label: "Chat history dialog" });

function openDialog() {
  nav.trackEditor("Test Venue", "Aurora");
  app.click(node("button", "Chat history"));
  return until("the chat history dialog", dialog);
}

test("a modal traps both tab directions and Escape closes it", () => {
  expect(dialog(openDialog()).focused).toBe(true);
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
  expect(dialog(app.snapshot())).toBe(undefined);
});

test("an optional dialog's scrim dismisses it", () => {
  openDialog();
  app.click(until("the optional scrim", (s) => s.find({ role: "button", label: "Dismiss dialog" }))
    .find({ role: "button", label: "Dismiss dialog" }));
  app.frames(2);
  expect(dialog(app.snapshot())).toBe(undefined);
});

test("required venue onboarding offers no scrim to dismiss it", { fixture: { track: false, seconds: 1, clips: [] } }, () => {
  const shot = until("required venue onboarding", (s) => s.find({ role: "card", label: "Venue dialog" }));
  expect(shot.find({ role: "button", label: "Dismiss dialog" })).toBe(undefined);
});
