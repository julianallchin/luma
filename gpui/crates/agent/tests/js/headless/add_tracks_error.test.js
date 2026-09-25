// A malformed adapter answer surfaces as an operable source error route.

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
  const isError = (n) => n.role === "text" && n.label.startsWith("Source error:");
  const failed = until("the source error", (s) => s.find(isError) !== undefined);
  expect(failed.find(isError).label).toContain("invalid");

  app.click(failed.find({ role: "input", label: "Search source…" }));
  app.key("escape");
  until("escape to dismiss the dialog from the source search", (s) =>
    s.find({ role: "button", label: "Close" }) === undefined);
});
