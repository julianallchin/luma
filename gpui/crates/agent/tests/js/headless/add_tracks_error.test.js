// A malformed adapter answer surfaces as an operable source error route: one
// plain line where the list would be, with the adapter's own text kept for
// the log.

fixture({
  seconds: 1,
  source_fixture: { library: { trackCount: "not-a-number" }, playlists: [], tracks: [] },
});

test("an adapter shape failure is visible, and escape dismisses from the field", () => {
  nav.venue("Test Venue");
  nav.step("the add-track affordance", "button", "Add track");
  const browser = until("the browser", (s) => s.find({ role: "button", label: "Import tracks" }) !== undefined);
  app.click(browser.find({ role: "button", label: "Import tracks" }));
  const picker = until("the import-source menu", (s) => s.find({ role: "row", label: "Rekordbox" }) !== undefined);
  app.click(picker.find({ role: "row", label: "Rekordbox" }));
  const failed = until("the source error", (s) =>
    s.find({ role: "text", label: "Could not read this library" }) !== undefined ? s : undefined);
  // The adapter's words ("invalid type…") are for the log, not the screen.
  expect(failed.findAll({ role: "text" }).filter((n) => n.label.includes("invalid"))).toEqual([]);
  expect(failed.find({ role: "button", label: "Retry" })).toBeTruthy();

  app.click(failed.find({ role: "input", label: "Search source…" }));
  app.key("escape");
  until("escape to dismiss the dialog from the source search", (s) =>
    s.find({ role: "button", label: "Close" }) === undefined);
});
