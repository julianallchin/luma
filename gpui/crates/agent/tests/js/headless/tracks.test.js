// The track browser, driven end to end against a seeded library.
//
// The point is the filters: `list_tracks_enriched` returns the whole visible
// library and decorates it with one venue's score and clip counts, so every
// number on this screen is the view's arithmetic over that. A fixture with
// known counts is the only way to tell a working filter from a missing one —
// the bug this was written for, where opening a venue listed the library.
//
// The rows are written in SQL: the seam cannot create a track without an
// audio file to import, and this screen only counts them.

// Tracks that are somebody else's, visible only through a clip in a venue
// this host can read.
const OTHER_USER = "another-principal";
// Enough rows that a screenful is a small fraction: the list must build a
// window, not a library.
const FILLER = 300;

// Newest first, the order the browser shows. `clipsIn` is the venue whose
// score carries the track's one clip.
const SEEDS = [
  { id: "track-aurora", title: "Aurora", artist: "Nightliner", clipsIn: "venue-main" },
  { id: "track-basslines", title: "Basslines", artist: "Nightliner", clipsIn: "venue-main" },
  // An intentionally empty score: membership is score existence.
  { id: "track-cascade", title: "Cascade", artist: "Sundial", emptyScoreIn: "venue-main" },
  { id: "track-drift", title: "Drift", artist: "Sundial", clipsIn: "venue-other" },
  { id: "track-echoes", title: "Echoes", artist: "Guest", uid: OTHER_USER, clipsIn: "venue-main" },
];

// What each filter admits, derived from the seeds.
const inMain = SEEDS.filter((s) => s.clipsIn === "venue-main" || s.emptyScoreIn === "venue-main");
const MINE = inMain.filter((s) => s.uid === undefined).length + FILLER;
const ALL = inMain.length + FILLER;

const track = (id, title, artist, uid, createdAt) =>
  `('${id}', '${uid ?? "$PRINCIPAL"}', '${id}-hash', '${title}', '${artist}', 240.0, '/fixture/${id}.mp3', '${createdAt}')`;
const score = (trackId, venue) => `('score-${trackId}-${venue}', '$PRINCIPAL', '${trackId}', '${venue}', 'Score')`;
const clip = (trackId, venue) =>
  `('clip-score-${trackId}-${venue}', '$PRINCIPAL', 'score-${trackId}-${venue}', 'pattern', 0.0, 120.0, '0', '{"expression":"all"}', 'replace')`;

const tracks = [
  ...SEEDS.map((s, i) => track(s.id, s.title, s.artist, s.uid, `2026-08-19T12:${String(59 - i).padStart(2, "0")}:00Z`)),
  ...Array.from({ length: FILLER }, (_, i) => {
    const n = String(i).padStart(3, "0");
    return track(`track-filler-${n}`, `Filler ${n}`, "Filler", undefined, `2020-01-01T00:${String(i % 60).padStart(2, "0")}:00Z`);
  }),
];
const withClips = [
  ...SEEDS.filter((s) => s.clipsIn).map((s) => [s.id, s.clipsIn]),
  ...Array.from({ length: FILLER }, (_, i) => [`track-filler-${String(i).padStart(3, "0")}`, "venue-main"]),
];
const scores = [...withClips, ...SEEDS.filter((s) => s.emptyScoreIn).map((s) => [s.id, s.emptyScoreIn])];

fixture({
  track: false,
  seconds: 1,
  sql: [
    // Older than the fixture's venue, so the picker lists it second.
    "INSERT INTO venues (id, uid, name, updated_at) VALUES ('venue-other', '$PRINCIPAL', 'Other Venue', '2020-01-01T00:00:00.000Z')",
    `INSERT INTO tracks (id, uid, track_hash, title, artist, duration_seconds, file_path, created_at) VALUES ${tracks.join(", ")}`,
    `INSERT INTO scores (id, uid, track_id, venue_id, name) VALUES ${scores.map(([t, v]) => score(t, v)).join(", ")}`,
    `INSERT INTO clips (id, uid, score_id, graph, start, duration, seed, selection_json, blend_mode) VALUES ${withClips.map(([t, v]) => clip(t, v)).join(", ")}`,
  ],
});

test("the browser filters a seeded library by venue, ownership and search", () => {
  // Every reading is the count the toolbar claims and the rows the
  // virtualized list built, so a filter that only changed the label fails.
  const read = () => {
    const shot = app.snapshot();
    return {
      count: shot.find((node) => /^\d+ tracks$/.test(node.label)).label,
      // The songs; the venue's own row leads them and is not a track.
      rows: shot.findAll({ role: "row" }).map((node) => node.label).filter((label) => label !== "Venue setup"),
    };
  };
  const press = (role, label) => {
    app.click(app.snapshot().find({ role, label }));
    app.frames(2);
    return read();
  };

  // The app enters on the venue dialog, not on a track table.
  const home = until("the venue picker", (s) => s.find({ role: "card", label: "Test Venue" }) !== undefined);
  expect(home.find({ role: "card", label: "Venue dialog" }) !== undefined).toBe(true);
  expect(home.findAll({ role: "row" }).length).toBe(0);
  expect(home.findAll({ role: "card" }).map((n) => n.label).filter((l) => l === "Test Venue" || l === "Other Venue"))
    .toEqual(["Test Venue", "Other Venue"]);

  // Opening a venue lists that venue's tracks, not the whole library.
  nav.venue("Test Venue");
  app.frames(6);
  const opened = read();
  expect(opened.count).toBe(`${MINE} tracks`);
  expect(opened.rows.slice(0, 3)).toEqual(["Aurora", "Basslines", "Cascade"]);

  // Each filter moves the count by exactly the rows it admits.
  expect(press("toggle", "All").count).toBe(`${ALL} tracks`);
  expect(press("toggle", "Mine").count).toBe(`${MINE} tracks`);

  // Search narrows to what it matches, and escape puts it back.
  app.type(app.snapshot().find({ role: "input" }), "bassl");
  app.frames(2);
  const searched = read();
  expect(searched.count).toBe("1 tracks");
  expect(searched.rows).toEqual(["Basslines"]);
  app.key("escape");
  app.frames(2);
  expect(read().count).toBe(`${MINE} tracks`);

  // The list is virtualized: a screenful of rows, not a library of them.
  expect(opened.rows.length).toBeGreaterThan(0);
  expect(opened.rows.length).toBeLessThan(MINE / 4);
});
