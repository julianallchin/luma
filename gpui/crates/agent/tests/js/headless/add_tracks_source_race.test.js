// Source row intent is one generation across all/search/playlist requests: a
// slow answer to an older request never replaces a newer one.

const row = (id, title) => ({
  uuid: id,
  filePath: null,
  filename: `${id}.wav`,
  title,
  artist: null,
  album: null,
  bpm: null,
  durationSeconds: null,
});

fixture({
  seconds: 1,
  source_fixture: {
    library: { trackCount: 1 },
    playlists: [{ id: "crate", name: "Crate", parentId: null, trackCount: 1 }],
    tracks: [row("all", "All row")],
    playlist_tracks: { crate: [row("playlist", "Playlist row")] },
    searches: { a: [row("global-a", "Global A")] },
  },
  // `a`, then `ab`, then `a` again: the first `a` answers last.
  source_search_responses: [
    { query: "a", delay_ms: 300, rows: [row("stale-a", "Stale A")] },
    { query: "ab", delay_ms: 20, rows: [row("ab", "AB")] },
    { query: "a", delay_ms: 60, rows: [row("current-a", "Current A")] },
  ],
});

test("a repeated query cannot admit an older response", () => {
  nav.venue("Test Venue");
  nav.step("the add-track affordance", "button", "Add track");
  const browser = until("the browser", (s) => s.find({ role: "button", label: "Import tracks" }) !== undefined);
  app.click(browser.find({ role: "button", label: "Import tracks" }));
  const picker = until("the import-source menu", (s) => s.find({ role: "row", label: "Rekordbox" }) !== undefined);
  app.click(picker.find({ role: "row", label: "Rekordbox" }));
  const source = until("the source", (s) => s.find({ role: "row", label: "All row" }) !== undefined);
  app.click(source.find({ role: "input", label: "Search source…" }));
  app.key("a");
  app.key("b");
  app.key("backspace");
  until("the latest repeated A intent", (s) => s.find({ role: "row", label: "Current A" }) !== undefined);
  // Past the stale answer's 300 ms.
  app.frames(40, { waitMs: 10 });
  const afterStale = app.snapshot();
  expect(afterStale.find({ role: "row", label: "Current A" }) !== undefined).toBe(true);
  expect(afterStale.find({ role: "row", label: "Stale A" })).toBe(undefined);

  app.click(afterStale.find({ role: "row", label: "Crate" }));
  const playlist = until("playlist scope clears search", (s) =>
    s.find({ role: "row", label: "Playlist row" }) !== undefined);
  app.click(playlist.find({ role: "input", label: "Search source…" }));
  app.key("a");
  until("search scope clears playlist", (s) => s.find({ role: "row", label: "Global A" }) !== undefined);
});
