// The account at the foot of the sidebar, and the menu it opens.
//
// Mostly a capture: whether the foot reads as the end of the sidebar's column
// and whether the menu reads as an object above it, no number answers. The
// assertion is the thing the old settings gear got wrong — a control that
// left the window rather than staying reachable.

fixture({ window: [1280, 800] });

test("the account menu opens above its foot and inside the window", () => {
  nav.venue("Test Venue");
  nav.step("the account foot", "button", "Account");
  until("the account menu", (s) => s.find({ role: "row", label: "Settings" }) !== undefined);
  app.frames(12, { waitMs: 40 });
  const opened = app.snapshot();
  const settings = opened.find({ role: "row", label: "Settings" }).bounds;
  const foot = opened.find({ role: "button", label: "Account" }).bounds;

  // Both halves matter: a menu that opened downward from a bottom-docked
  // control would be snapped somewhere by the window edge and stop being
  // attached to anything.
  expect(settings.y).toBeLessThan(foot.y);
  expect(settings.x).toBeGreaterThan(-1);

  image.keep(app.screenshot(), "account-foot/menu");
});
