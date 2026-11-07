// The song list as Finder's column view: places, then the songs in the
// selected place, then the selected song's scores (`docs/specs/comet-shell.md`
// §4).
//
// - picking a folder fills the songs column with that folder's songs, and
//   All songs lists every song, filed or not;
// - picking a song fills the scores column;
// - Right, Down and Enter walk the columns and open a score, closing the page;
// - the search lists matches from every place, under All songs;
// - another venue's score asks before the window switches venue.

// Aurora (the fixture's) and Borealis in the Test Venue; Borealis filed in
// "Openers", Aurora in no folder. Aurora is scored once more in the Warehouse.
fixture({
  seconds: 4,
  extra_scores: 1,
  sql: [
    `INSERT INTO tracks (id, uid, track_hash, title, artist, duration_seconds, file_path)
       VALUES ('track-borealis', '$PRINCIPAL', 'hash-borealis', 'Borealis', 'Fixture', 60.0, '/fixture/borealis.wav')`,
    `INSERT INTO scores (id, uid, track_id, venue_id, name)
       VALUES ('score-borealis', '$PRINCIPAL', 'track-borealis', 'venue-main', 'Borealis cut')`,
    "INSERT INTO folders (id, uid, venue_id, name) VALUES ('folder-openers', '$PRINCIPAL', 'venue-main', 'Openers')",
    "INSERT INTO folder_tracks (folder_id, track_id, uid, venue_id) VALUES ('folder-openers', 'track-borealis', '$PRINCIPAL', 'venue-main')",
    "INSERT INTO venues (id, uid, name) VALUES ('venue-warehouse', '$PRINCIPAL', 'Warehouse')",
    `INSERT INTO scores (id, uid, track_id, venue_id, name)
       VALUES ('score-warehouse', '$PRINCIPAL', 'track-aurora', 'venue-warehouse', 'Warehouse cut')`,
  ],
});

const page = (s) => s.find({ role: "card", label: "Song list" });
const column = (s, label) => s.find({ role: "slider", label })?.bounds;
// The songs listed in the songs column: rows between its two seams.
const songs = (s) => {
  const left = column(s, "Folders column width");
  const right = column(s, "Songs column width");
  if (left === undefined || right === undefined) return [];
  return s.findAll({ role: "row" })
    .filter((n) => n.bounds.x > left.x && n.bounds.x + n.bounds.width < right.x + right.width
      && !n.label.startsWith("#"))
    .map((n) => n.label);
};
const scoresOf = (s, track) => s.find({ role: "card", label: `Scores of ${track}` });

test("a folder fills the songs column, and All songs lists every song", () => {
  nav.venue("Test Venue");
  nav.songs();
  const all = until("every song under All songs, and the folder", (s) =>
    songs(s).length === 2 && s.find({ role: "row", label: "Folder Openers" }) ? s : undefined);
  // A filed song and an unfiled one, both under All songs.
  expect(songs(all).sort()).toEqual(["Aurora", "Borealis"]);
  expect(texts(all)).toContain("All songs songs: 2");
  expect(texts(all)).toContain("Folder Openers songs: 1");

  nav.step("the folder", "row", "Folder Openers");
  const filed = until("the folder's songs", (s) =>
    songs(s).length === 1 ? s : undefined);
  expect(songs(filed)).toEqual(["Borealis"]);
  // Nothing is selected in it yet, so there are no scores to show.
  expect(scoresOf(filed, "Borealis")).toBe(undefined);

  nav.step("All songs", "row", "All songs");
  until("both songs again", (s) => songs(s).length === 2);
});

test("a song fills the scores column with this venue's scores, then the others'", () => {
  nav.venue("Test Venue");
  nav.songs();
  nav.step("the song", "row", "Aurora");
  const shot = until("Aurora's scores", (s) =>
    nav.scoreRows(s, "Aurora").length === 3 ? s : undefined);
  const rows = nav.scoreRows(shot, "Aurora");
  const newScore = nav.inScores(shot, "Aurora", "button", "New score")[0];
  // This venue's two, then New score, then the Warehouse's under its name.
  const here = rows.filter((n) => !n.label.endsWith(" · Warehouse"));
  const there = rows.filter((n) => n.label.endsWith(" · Warehouse"));
  expect(here.length).toBe(2);
  expect(there.length).toBe(1);
  assert(here.every((n) => n.bounds.y < newScore.bounds.y) && there[0].bounds.y > newScore.bounds.y,
    `the scores are out of order: ${JSON.stringify(rows.map((n) => [n.label, n.bounds.y]))}`);
  // The scores column is right of the songs column.
  const song = shot.find({ role: "row", label: "Aurora" }).bounds;
  assert(rows[0].bounds.x > song.x + song.width, "the scores are not in a column right of the songs");

  // Another song replaces them.
  nav.step("the other song", "row", "Borealis");
  until("Borealis's scores", (s) => scoresOf(s, "Borealis") && !scoresOf(s, "Aurora")
    && nav.scoreRows(s, "Borealis").length === 1);
});

test("Right, Down and Enter open a score and close the page", () => {
  nav.venue("Test Venue");
  nav.songs();
  until("the songs", (s) => songs(s).length === 2);
  // A press on All songs gives the columns the keyboard.
  nav.step("All songs", "row", "All songs");
  const order = songs(app.snapshot());
  app.key("right");
  until("the first song's scores", (s) => scoresOf(s, order[0]) && nav.scoreRows(s, order[0]).length > 0);
  app.key("down");
  const second = order[1];
  until("the second song's scores", (s) => scoresOf(s, second) && nav.scoreRows(s, second).length > 0);
  app.key("right");
  app.key("enter");
  until("the score open, the page gone", (s) =>
    !page(s) && nav.openScore(s, second) !== undefined && s.find({ role: "card", label: "Waveform" }));

  // Reopening the page lands on the song it was left on.
  nav.songs();
  until("the song still selected", (s) => scoresOf(s, second));
});

test("the search lists matches from every place under All songs", () => {
  nav.venue("Test Venue");
  nav.songs();
  nav.step("the folder", "row", "Folder Openers");
  until("the folder's song", (s) => songs(s).length === 1);
  // A query searches past the folder: Aurora is in no folder.
  app.click(app.snapshot().find({ role: "input", label: "Search tracks" }));
  app.type(app.snapshot().find({ role: "input", label: "Search tracks" }), "auro");
  const found = until("the match", (s) => songs(s).length === 1 && songs(s)[0] === "Aurora" ? s : undefined);
  expect(found.find({ role: "input", label: "auro" }) !== undefined).toBe(true);
  // A query that matches nothing says so.
  app.type(app.snapshot().find({ role: "input", label: "auro" }), "zzz");
  until("no match", (s) => songs(s).length === 0 && texts(s).includes("No matching tracks"));
});

test("another venue's score asks before the window switches", () => {
  nav.venue("Test Venue");
  nav.scores("Aurora");
  const shot = until("the Warehouse's score", (s) =>
    nav.scoreRows(s, "Aurora").find((n) => n.label.endsWith(" · Warehouse")) ? s : undefined);
  app.click(nav.scoreRows(shot, "Aurora").find((n) => n.label.endsWith(" · Warehouse")));
  const asked = until("the question", (s) => s.find({ role: "card", label: "Confirm dialog" }) ? s : undefined);
  expect(texts(asked)).toContain("Switch to Warehouse?");

  // Cancel leaves the page where it was.
  nav.step("cancel", "button", "Cancel");
  until("still on the list", (s) => !s.find({ role: "card", label: "Confirm dialog" })
    && page(s) && scoresOf(s, "Aurora"));

  app.click(nav.scoreRows(app.snapshot(), "Aurora").find((n) => n.label.endsWith(" · Warehouse")));
  nav.step("switch", "button", "Switch");
  until("the Warehouse's score, open", (s) =>
    s.find({ role: "button", label: "Warehouse" })?.bounds.y < 44
      && nav.openScore(s, "Aurora") === "Aurora · #1"
      && !page(s));
});
