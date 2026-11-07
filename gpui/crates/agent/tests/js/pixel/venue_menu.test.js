// The title bar's venue menu, which holds the account's rows too.
//
// Mostly a capture: whether the menu reads as an object hung off its trigger,
// no number answers. The assertion is the thing the old settings gear got
// wrong — a control that left the window rather than staying reachable.

fixture({ window: [1280, 800] });

test("the venue menu opens below its trigger and inside the window", () => {
  nav.venue("Test Venue");
  nav.venueMenu();
  until("the account rows", (s) => s.find({ role: "row", label: "Settings…" }) !== undefined);
  app.frames(12, { waitMs: 40 });
  const opened = app.snapshot();
  const settings = opened.find({ role: "row", label: "Settings…" }).bounds;
  const trigger = opened.find({ role: "button", label: "Test Venue" }).bounds;

  // Both halves matter: a menu that opened away from its trigger would be
  // snapped somewhere by the window edge and stop being attached to it.
  expect(settings.y).toBeGreaterThan(trigger.y + trigger.height - 1);
  expect(settings.x).toBeGreaterThan(-1);
  expect(settings.x + settings.width).toBeLessThan(1281);
  expect(settings.y + settings.height).toBeLessThan(801);

  image.keep(app.screenshot(), "venue-menu/menu");
});
