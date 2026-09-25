// Empty all-Luma and empty normalized-source states through the real dialog.

const engineTrack = {
  id: 42,
  path: "/fixture/engine.wav",
  filename: "engine.wav",
  title: "Engine normalized",
  artist: "Engine artist",
  album: null,
  bpmAnalyzed: 124.0,
  length: 180.0,
};

fixture({ track: false, seconds: 1, source_fixture_delay_ms: 150 });

function openImportMenu(source) {
  nav.venue("Test Venue");
  nav.step("the add-track affordance", "button", "Add track");
  const browser = until("the empty all-Luma browser", (s) =>
    s.find({ role: "button", label: "Import tracks" }) !== undefined);
  app.click(browser.find({ role: "button", label: "Import tracks" }));
  const picker = until("the import-source menu", (s) => s.find({ role: "row", label: source }) !== undefined);
  app.click(picker.find({ role: "row", label: source }));
  return browser;
}

test(
  "an empty library centres import, and the Engine source normalizes its rows",
  {
    fixture: {
      source_fixture: {
        library: { databaseUuid: "engine-fixture", trackCount: 1 },
        playlists: [{ id: 7, title: "Engine crate", parentId: null, trackCount: 1 }],
        tracks: [engineTrack],
        playlist_tracks: { 7: [engineTrack] },
      },
    },
  },
  () => {
    const browser = openImportMenu("Engine DJ");
    expect(browser.findAll({ role: "row" }).length).toBe(0);
    // The held read shows its loading state first.
    expect(app.snapshot().find({ role: "text", label: "Loading source library…" }) !== undefined).toBe(true);
    const engine = until("the normalized Engine source", (s) =>
      s.find({ role: "row", label: "Engine crate" }) !== undefined &&
      s.find({ role: "row", label: "Engine normalized" }) !== undefined);
    // Nothing is selected yet, so there is nothing to import.
    expect(engine.find({ role: "button", label: "Import selected" })?.enabled).toBe(false);
  },
);

test(
  "an empty source says so instead of showing an empty list",
  { fixture: { source_fixture: { library: { trackCount: 0 }, playlists: [], tracks: [] } } },
  () => {
    openImportMenu("Rekordbox");
    expect(app.snapshot().find({ role: "text", label: "Loading source library…" }) !== undefined).toBe(true);
    until("the explicit empty source route", (s) =>
      s.find({ role: "text", label: "No source tracks" }) !== undefined);
  },
);
