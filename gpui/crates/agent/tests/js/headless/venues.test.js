// Venue switching through the production app tree. The launch, restore and
// stale-read tests stay in `tests/headless/venues.rs`: they inject read
// delays and relaunch the app on one library.

// Two more venues beside the fixture's, each with a track and a score.
const room = (id, name) => [
  `INSERT INTO venues (id, uid, name) VALUES ('${id}', '$PRINCIPAL', '${name}')`,
  `INSERT INTO tracks (id, uid, track_hash, title, artist, duration_seconds, file_path)
     VALUES ('track-${id}', '$PRINCIPAL', 'hash-${id}', '${name} Track', 'Fixture', 60.0, '/fixture/${id}.wav')`,
  `INSERT INTO scores (id, uid, track_id, venue_id, name)
     VALUES ('score-${id}', '$PRINCIPAL', 'track-${id}', '${id}', 'Fixture Score')`,
];
// And one song in both rooms, scored in each.
const shared = [
  `INSERT INTO tracks (id, uid, track_hash, title, artist, duration_seconds, file_path)
     VALUES ('track-both', '$PRINCIPAL', 'hash-both', 'Both Rooms', 'Fixture', 60.0, '/fixture/both.wav')`,
  ...["alpha", "beta"].map((venue) => `INSERT INTO scores (id, uid, track_id, venue_id, name)
     VALUES ('score-both-${venue}', '$PRINCIPAL', 'track-both', '${venue}', 'Both')`),
];
// And another person's song in both rooms.
const theirs = [
  `INSERT INTO tracks (id, uid, track_hash, title, artist, duration_seconds, file_path)
     VALUES ('track-theirs', 'another-principal', 'hash-theirs', 'Their Song', 'Guest', 60.0, '/fixture/theirs.wav')`,
  ...["alpha", "beta"].flatMap((venue) => [
    `INSERT INTO scores (id, uid, track_id, venue_id, name)
       VALUES ('score-theirs-${venue}', '$PRINCIPAL', 'track-theirs', '${venue}', 'Theirs')`,
    `INSERT INTO clips (id, uid, score_id, graph, start, duration, seed, selection_json, blend_mode)
       VALUES ('clip-theirs-${venue}', '$PRINCIPAL', 'score-theirs-${venue}', 'pattern', 0.0, 4.0, '0', '{"all":true}', 'replace')`,
  ]),
];
fixture({
  track: false,
  seconds: 1,
  sql: [...room("alpha", "Alpha Hall"), ...room("beta", "Beta Room"), ...shared, ...theirs],
});

const titled = (s, label) => s.find({ role: "button", label }) !== undefined;

const songList = (s) => s.find({ role: "card", label: "Song list" }) !== undefined;
const selected = (s, track) => s.find({ role: "card", label: `Scores of ${track}` }) !== undefined;

// A switch changes the venue, not the page: Rig stays Rig, and Arrange —
// the song list or a score — lands on the other venue's song list, with
// nothing open under it.
test("a venue switch keeps the page, and a score lands on the other venue's list", () => {
  const alphaScore = "Alpha Hall Track · #1";
  nav.venue("Alpha Hall");
  nav.track("Alpha Hall Track");
  until("Alpha's score in the title bar", (s) => titled(s, alphaScore));
  nav.mode("Rig");

  nav.switchVenue("Beta Room");
  until("Beta in Rig", (s) => nav.shownMode(s) === "Rig" && s.find({ role: "card", label: "Beta Room Venue" }));
  // Beta has nothing open: Arrange is its song list, of Beta's songs.
  nav.mode("Arrange");
  const beta = until("Beta's song list", (s) =>
    songList(s) && s.find({ role: "row", label: "Beta Room Track" }) ? s : undefined);
  expect(beta.find({ role: "row", label: "Alpha Hall Track" })).toBe(undefined);
  nav.track("Beta Room Track");
  until("Beta's score", (s) => titled(s, "Beta Room Track · #1"));

  // From a score, the switch lands on Alpha's song list with no score open
  // (Alpha's own score is not kept under it); Beta's song is not Alpha's, so
  // nothing is selected.
  nav.switchVenue("Alpha Hall");
  const alpha = until("Alpha's song list", (s) =>
    songList(s) && s.find({ role: "row", label: "Alpha Hall Track" }) ? s : undefined);
  app.frames(4);
  const settled = app.snapshot();
  expect(nav.shownMode(settled)).toBe("Arrange");
  expect(nav.openOrdinal(settled)).toBe(undefined);
  expect(selected(settled, "Beta Room Track")).toBe(false);
  // ⌘P on the list stays on it: there is no score under it.
  app.action("luma::ShowSongList");
  app.frames(4);
  assert(songList(app.snapshot()) && nav.openOrdinal(app.snapshot()) === undefined, "a score was under the list");
  expect(alpha.find({ role: "row", label: "Beta Room Track" })).toBe(undefined);
});

test("the song list stays the song list across a switch, with a song both venues have", () => {
  nav.venue("Alpha Hall");
  nav.scores("Both Rooms");
  until("Both Rooms selected in Alpha", (s) => selected(s, "Both Rooms"));
  nav.switchVenue("Beta Room");
  until("Beta's song list, Both Rooms still selected", (s) =>
    songList(s) && s.find({ role: "row", label: "Beta Room Track" }) && selected(s, "Both Rooms")
      && nav.openOrdinal(s) === undefined);

  // A song Beta does not have leaves nothing selected.
  nav.scores("Beta Room Track");
  nav.switchVenue("Alpha Hall");
  const alpha = until("Alpha's song list", (s) =>
    songList(s) && s.find({ role: "row", label: "Alpha Hall Track" }) ? s : undefined);
  app.frames(4);
  expect(selected(app.snapshot(), "Beta Room Track")).toBe(false);
  expect(nav.shownMode(alpha)).toBe("Arrange");
});

test("another person's song in hand is carried to the next venue", () => {
  nav.venue("Alpha Hall");
  nav.songs();
  nav.scores("Their Song");
  until("Their Song selected", (s) => selected(s, "Their Song"));
  nav.switchVenue("Beta Room");
  until("Beta's list on Their Song", (s) =>
    songList(s) && s.find({ role: "row", label: "Beta Room Track" }) && selected(s, "Their Song")
      && s.find({ role: "row", label: "Their Song" }));
});

test("Live stays Live across a switch", () => {
  nav.venue("Alpha Hall");
  nav.live();
  nav.switchVenue("Beta Room");
  until("Beta in Live", (s) => nav.shownMode(s) === "Live");
});
