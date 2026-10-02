// A press outside an open float closes it, and lands nowhere else.
//
// Dismissal lives in the floating layer (`luma_ui::float::Dismiss`). gpui
// reports every hitbox under the pointer, so a dismissal that did not take
// the press would also act on whatever was underneath: one gesture, two
// effects. Same ownership rule as a seam grip, one layer up.

fixture({ clips: [{ pattern: "pat-glow", name: "Glow", start: 1, end: 4 }] });

// The workspace's `+` box, hung at a window point. The sidebar's track row
// sits under the press — pressing it pushes the column to that track's
// scores, which is loud and easy to notice.
test("a press outside a menu closes it and does not reach what is under it", () => {
  // The `+` lives in the workspace panel's band, so a tab must be open first.
  // `nav.track` leaves the sidebar on the track list.
  nav.trackEditor("Test Venue", "Aurora");
  const menu = (s) => s.find({ role: "card", label: "New tab" });
  const level = (s) => s.find({ role: "card", label: "Scores level" });

  app.action("luma::NewTab");
  const opened = until("the + box", (s) => menu(s) !== undefined);
  expect(level(opened)).toBe(undefined);

  app.click(opened.find({ role: "row", label: "Aurora" }));
  app.frames(2);
  const after = app.snapshot();
  assert(menu(after) === undefined, "the menu stayed up through a press outside it");
  assert(level(after) === undefined, "the dismissing press also pushed the sidebar — the float shares its press");
});
