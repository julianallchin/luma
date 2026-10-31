// The add-track palette under a real renderer: an empty library is not a
// special layout, every route is one palette that does not move, and the
// routes are visibly different pictures.

// One source crate with one track in it, answered slowly enough that the
// route change is observable.
const ROW = {
  id: "pixel-source",
  uuid: "pixel-source-uuid",
  filePath: "/fixture/pixel-source.wav",
  filename: "pixel-source.wav",
  title: "Pixel Source Track",
  artist: "Pixel Artist",
  album: "Pixel Album",
  bpm: 126.0,
  durationSeconds: 180.0,
  fileSize: 1024,
  sampleRate: 44100,
};

fixture({ window: [1000, 720] });

const dialog = (s) => s.find({ role: "card", label: "Add tracks dialog" });
// A row of the palette's own list — the sidebar lists the same tracks.
const listed = (s, label) => s.findAll({ role: "row", label }).find((n) => inside(n.bounds, dialog(s).bounds));

// A box inside another, with the window's gutters as the outer one.
const inside = (inner, outer) =>
  inner.x >= outer.x && inner.y >= outer.y &&
  inner.x + inner.width <= outer.x + outer.width &&
  inner.y + inner.height <= outer.y + outer.height;

test("an empty library keeps the palette's header and says it is empty", { fixture: { seconds: 1, track: false } }, () => {
  nav.venue("Test Venue");
  nav.step("the add-track affordance", "button", "Add track");
  // The import chip is in the header on every route, so its presence does
  // not mean the list has loaded — wait for the empty body to name itself.
  const shot = until("the empty all-Luma route", (s) =>
    s.find({ role: "text", label: "No tracks in your library" }) &&
    // The dialog's own list — the sidebar behind it has rows of its own.
    s.findAll({ role: "row" }).filter((n) => inside(n.bounds, dialog(s).bounds)).length === 0 ? s : undefined);
  const card = dialog(shot);
  image.keep(app.screenshot(), "add-tracks/empty-browser");
  image.keep(app.screenshot({ node: card }), "add-tracks/empty-browser-card");

  const chip = shot.find({ role: "button", label: "Import tracks" }).bounds;
  const line = shot.find({ role: "text", label: "No tracks in your library" }).bounds;
  // The import affordance stays the header chip it is on every route, the
  // body says in words that it is empty, and both are inside the card.
  assert(inside(chip, card.bounds), "the import chip left the card");
  expect(line.y).toBeGreaterThan(chip.y + chip.height);
  assert(inside(line, card.bounds), "the empty line escaped the card");
});

test(
  "every route is one palette that holds still while its content morphs",
  {
    fixture: {
      seconds: 8,
      source_fixture: {
        library: { trackCount: 1 },
        playlists: [{ id: "pixel-crate", name: "Pixel crate", parentId: null, trackCount: 1 }],
        tracks: [ROW],
        playlist_tracks: { "pixel-crate": [ROW] },
      },
      source_fixture_delay_ms: 180,
      motion: true,
      // Stretched so before, mid and after land on distinct frames.
      motion_scale: 3.0,
    },
    timeoutMs: 120000,
  },
  () => {
    nav.venue("Test Venue");
    nav.step("the add-track affordance", "button", "Add track");
    let shot = until("the populated all-Luma route", (s) =>
      dialog(s) && listed(s, "Aurora") && s.find({ role: "button", label: "Import tracks" }) ? s : undefined);
    const browser = { shot: app.screenshot(), box: dialog(shot).bounds };
    const browserChip = shot.find({ role: "button", label: "Import tracks" }).bounds;
    const firstRow = listed(shot, "Aurora").bounds;

    app.click(shot.find({ role: "button", label: "Import tracks" }));
    shot = until("the import-source menu", (s) => s.find({ role: "row", label: "Rekordbox" })?.enabled === true ? s : undefined);
    const picker = { shot: app.screenshot(), box: dialog(shot).bounds };

    app.click(shot.find({ role: "row", label: "Rekordbox" }));
    // Mid-morph the palette shows BOTH routes' content — the outgoing source
    // choice and the incoming library — while the card has not moved.
    shot = until("a source-library morph frame", (s) =>
      s.find({ role: "row", label: "Rekordbox" }) && s.find({ role: "input", label: "Search source…" }) ? s : undefined);
    const midpoint = { shot: app.screenshot(), box: dialog(shot).bounds };

    shot = until("the committed source library", (s) =>
      s.find({ role: "row", label: "Pixel Source Track" }) &&
      s.find({ role: "input", label: "Search source…" })?.focused === true ? s : undefined);
    const library = { shot: app.screenshot(), box: dialog(shot).bounds };
    const sourceChip = shot.find({ role: "button", label: "Import selected" }).bounds;
    const sourceRow = listed(shot, "Pixel Source Track").bounds;

    image.keep(browser.shot, "add-tracks/track-browser");
    image.keep(picker.shot, "add-tracks/source-picker");
    image.keep(midpoint.shot, "add-tracks/source-morph-midpoint");
    image.keep(library.shot, "add-tracks/source-library");

    // Every route is the SAME palette: one size, one place, inside the
    // window. The tolerance covers the entrance's 2px rise, which can still
    // be settling when the first route is captured.
    const window_ = { x: 0, y: 0, width: 1000, height: 720 };
    for (const [name, route] of Object.entries({ picker, midpoint, library })) {
      for (const key of ["x", "y", "width", "height"]) {
        assert(Math.abs(route.box[key] - browser.box[key]) <= 3,
          `${name} is not the browser's palette on ${key}: ${JSON.stringify(route.box)} vs ${JSON.stringify(browser.box)}`);
      }
      assert(inside(route.box, window_), `${name} escaped the window`);
    }

    // The committing action rides in the header, above the list it commits.
    for (const [chip, row, box] of [[browserChip, firstRow, browser.box], [sourceChip, sourceRow, library.box]]) {
      assert(inside(chip, box), `a submit chip left its card: ${JSON.stringify(chip)}`);
      expect(chip.y + chip.height).toBeLessThan(row.y + 0.5);
    }

    expect(image.diff(browser.shot, picker.shot)).toBeGreaterThan(0.08);
    expect(image.diff(midpoint.shot, library.shot)).toBeGreaterThan(0.04);
  },
);

// Source rows on the fixture's real audio, every other one with its library's
// thumbnail, so the screenshot shows covers beside placeholders.
const previewRow = (index) => ({
  uuid: `preview-${index}`,
  filePath: "$LIBRARY/zulu.wav",
  filename: "zulu.wav",
  title: ["Night Drive", "Low Tide", "Glass Hours", "Afterimage", "Signal Fire", "Northbound"][index],
  artist: ["Kessler", "Mara Lune", "Okkervil", "Yume", "Sable", "Teo Park"][index],
  album: null,
  bpm: 124.0,
  durationSeconds: 40.0,
  artworkPath: index % 2 === 0 ? "$LIBRARY/padding-art.png" : null,
});
const PREVIEW_ROWS = [0, 1, 2, 3, 4, 5].map(previewRow);

test(
  "source rows show their covers, and the previewing row its scrub bar",
  {
    fixture: {
      seconds: 8,
      equal_timestamp_track: true,
      second_track_seconds: 40,
      album_art: 0,
      source_fixture: {
        library: { trackCount: PREVIEW_ROWS.length },
        playlists: [{ id: "warmup", name: "Warmup", parentId: null, trackCount: PREVIEW_ROWS.length }],
        tracks: PREVIEW_ROWS,
        playlist_tracks: { warmup: PREVIEW_ROWS },
      },
    },
    timeoutMs: 120000,
  },
  () => {
    nav.venue("Test Venue");
    nav.step("the add-track affordance", "button", "Add track");
    let shot = until("the browser", (s) => s.find({ role: "button", label: "Import tracks" }) ? s : undefined);
    app.click(shot.find({ role: "button", label: "Import tracks" }));
    shot = until("the source menu", (s) => s.find({ role: "row", label: "Rekordbox" })?.enabled ? s : undefined);
    app.click(shot.find({ role: "row", label: "Rekordbox" }));
    shot = until("every cover decoded", (s) =>
      ["Night Drive", "Glass Hours", "Signal Fire"].every((t) => s.find({ role: "text", label: `Cover of ${t}` })) ? s : undefined);
    image.keep(app.screenshot(), "add-tracks/source-covers");

    app.click(shot.find({ role: "button", label: "Play preview of Glass Hours" }));
    shot = until("Glass Hours previewing", (s) =>
      s.find({ role: "button", label: "Pause preview of Glass Hours" }) &&
      s.findAll({ role: "slider" }).some((n) => n.label.startsWith("Preview position:")) ? s : undefined);
    app.frames(4, { waitMs: 50 });
    shot = app.snapshot();
    image.keep(app.screenshot(), "add-tracks/source-previewing");
    image.keep(app.screenshot({ node: dialog(shot) }), "add-tracks/source-previewing-card");

    // The scrub bar is the subtitle line of its own row, the text column's
    // width, and the row did not grow to hold it.
    const row = listed(shot, "Glass Hours").bounds;
    const neighbour = listed(shot, "Low Tide").bounds;
    const bar = shot.findAll({ role: "slider" }).find((n) => n.label.startsWith("Preview position:")).bounds;
    assert(inside(bar, row), "the scrub bar left its row");
    expect(bar.width).toBeGreaterThan(row.width / 3);
    expect(Math.abs(row.height - neighbour.height)).toBeLessThan(0.5);
  },
);
