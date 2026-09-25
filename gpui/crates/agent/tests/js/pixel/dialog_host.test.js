// The production dialog host in a small window: a modal is clamped inside
// the window's gutters, sits below the titlebar with the window's own
// controls still live above it, and visibly changes the shell behind it.

fixture({
  clips: [{ pattern: "pattern-strobe", name: "Strobe", start: 1, end: 4 }],
  window: [640, 480],
});

test("a dialog is clamped below live window controls and changes the shell", () => {
  nav.venue("Test Venue");
  until("the track browser", (s) => s.find({ role: "input", label: "Search tracks" }) !== undefined);
  const base = app.screenshot();

  app.action("luma::OpenSettings");
  until("the settings dialog", (s) => s.find({ role: "card", label: "Settings dialog" }) !== undefined);
  app.frames(2);
  const shot = app.snapshot();
  const card = shot.find({ role: "card", label: "Settings dialog" });
  const overlay = app.screenshot();
  const cardShot = app.screenshot({ node: card });
  image.keep(base, "dialog-host/base");
  image.keep(overlay, "dialog-host/settings");
  image.keep(cardShot, "dialog-host/settings-card");

  const box = card.bounds;
  const lights = ["close", "minimize", "maximize"].map((label) => shot.find({ role: "button", label }));
  // Inside the window, with a gutter on every side.
  expect(box.x).toBeGreaterThan(0);
  expect(box.x + box.width).toBeLessThan(640);
  expect(box.y + box.height).toBeLessThan(480);
  // Clamped, not collapsed: the route keeps most of a small window.
  expect(box.width).toBeGreaterThan(640 / 2);
  expect(box.height).toBeGreaterThan(480 / 2);
  for (const light of lights) {
    // The window's controls are still there, above the card, not under it.
    expect(light.bounds.width * light.bounds.height).toBeGreaterThan(0);
    expect(light.bounds.y + light.bounds.height).toBeLessThan(box.y + 0.5);
  }

  // The modal plane visibly transforms the shell…
  expect(image.diff(base, overlay)).toBeGreaterThan(0.2);
  // …and the card carries the route's content, not a flat placeholder.
  const content = image.stats(cardShot);
  expect(content.max - content.min).toBeGreaterThan(24);
});
