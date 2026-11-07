// A press outside an open float closes it, and lands nowhere else.
//
// Dismissal lives in the floating layer (`luma_ui::float::Dismiss`). gpui
// reports every hitbox under the pointer, so a dismissal that did not take
// the press would also act on whatever was underneath: one gesture, two
// effects. Same ownership rule as a seam grip, one layer up.

fixture({ clips: [{ pattern: "pat-glow", name: "Glow", start: 1, end: 4 }] });

// The title bar's venue menu, open over the song list. A song row sits under
// the press — pressing it expands that song's scores, which is loud and easy
// to notice.
test("a press outside a menu closes it and does not reach what is under it", () => {
  nav.venue("Test Venue");
  nav.songs();
  const menu = (s) => s.find({ role: "row", label: "New venue…" });
  const expanded = (s) => s.find({ role: "card", label: "Scores of Aurora" });

  nav.venueMenu();
  const opened = until("the venue menu", (s) => menu(s) !== undefined ? s : undefined);
  expect(expanded(opened)).toBe(undefined);

  // A point on the song's row clear of the menu.
  const row = opened.find({ role: "row", label: "Aurora" }).bounds;
  const box = opened.find({ role: "row", label: "New venue…" }).bounds;
  app.click({ x: Math.max(row.x + row.width - 40, box.x + box.width + 40), y: row.y + row.height / 2 });
  app.frames(2);
  const after = app.snapshot();
  assert(menu(after) === undefined, "the menu stayed up through a press outside it");
  assert(expanded(after) === undefined, "the dismissing press also expanded the song — the float shares its press");
});
