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
fixture({ track: false, seconds: 1, sql: [...room("alpha", "Alpha Hall"), ...room("beta", "Beta Room"), ...shared] });

const titled = (s, label) => s.find({ role: "button", label }) !== undefined;

const songList = (s) => s.find({ role: "card", label: "Song list" }) !== undefined;
const selected = (s, track) => s.find({ role: "card", label: `Scores of ${track}` }) !== undefined;

// A switch changes the venue, not the page: Rig stays Rig, and Arrange —
// the song list or a score — lands on the other venue's song list. What the
// venue left had open stays parked, not closed.
test("a venue switch keeps the page, and each venue its own open score", () => {
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

  // From a score, the switch lands on Alpha's song list; Beta's song is not
  // Alpha's, so nothing is selected.
  nav.switchVenue("Alpha Hall");
  const alpha = until("Alpha's song list", (s) =>
    songList(s) && s.find({ role: "row", label: "Alpha Hall Track" }) ? s : undefined);
  expect(nav.shownMode(alpha)).toBe("Arrange");
  expect(selected(alpha, "Beta Room Track")).toBe(false);
  // Parked is not closed: under the list, Alpha's score is still open.
  app.action("luma::ToggleSongList");
  until("Alpha's score again", (s) => titled(s, alphaScore) && !songList(s));

  // And Beta kept its own.
  nav.switchVenue("Beta Room");
  app.action("luma::ToggleSongList");
  until("Beta's score again", (s) => titled(s, "Beta Room Track · #1") && !songList(s));
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

test("Live stays Live across a switch", () => {
  nav.venue("Alpha Hall");
  nav.live();
  nav.switchVenue("Beta Room");
  until("Beta in Live", (s) => nav.shownMode(s) === "Live");
});
