// AddTracks' focus handoff during a live morph: the keyboard stays inside the
// dialog while the route changes, then lands on the new route's field.

fixture({
  seconds: 1,
  motion: true,
  source_fixture: { library: { trackCount: 0 }, playlists: [], tracks: [] },
  source_fixture_delay_ms: 400,
});

test("focus moves to the modal scope in flight, then commits to the target route", () => {
  const focused = (s) => s.findAll((node) => node.focused);
  const modalFocused = (s) =>
    s.findAll({ role: "card", label: "Add tracks dialog" }).some((card) => card.focused);

  nav.venue("Test Venue");
  // Motion is on, so the sidebar's entrance has to finish before its clipped
  // Add button is a valid target.
  app.frames(20, { waitMs: 10 });
  nav.step("the add-track affordance", "button", "Add track");
  const browser = until("focused all-Luma search", (s) =>
    s.find({ role: "input", label: "Search all tracks…" })?.focused === true &&
    s.find({ role: "button", label: "Import tracks" }) !== undefined);
  // Choosing a source is a menu, not a route: it opens in place and moves no
  // focus, so the browser behind it is still the mounted route.
  app.click(browser.find({ role: "button", label: "Import tracks" }));
  const menu = until("the import-source menu", (s) => s.find({ role: "row", label: "Rekordbox" }) !== undefined);
  expect(menu.find({ role: "input", label: "Search all tracks…" }) !== undefined).toBe(true);

  app.click(menu.find({ role: "row", label: "Rekordbox" }));
  app.frames(1);
  const flight = app.snapshot();
  assert(modalFocused(flight), "the dialog lost the keyboard mid-morph");
  assert(focused(flight).every((node) => node.role === "card"),
    `a control held focus mid-morph: ${focused(flight).map((n) => `${n.role}:${n.label}`)}`);
  until("committed source focus", (s) => s.find({ role: "input", label: "Search source…" })?.focused === true);
});
