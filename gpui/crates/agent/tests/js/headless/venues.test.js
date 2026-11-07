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
fixture({ track: false, seconds: 1, sql: [...room("alpha", "Alpha Hall"), ...room("beta", "Beta Room")] });

const titled = (s, label) => s.find({ role: "button", label }) !== undefined;

// Each venue keeps its own mode and open score: leaving a venue parks them,
// and coming back brings them back as they were. The song list in the other
// venue lists that venue's songs only.
test("each venue keeps its mode and open score across a switch", () => {
  const alphaScore = "Alpha Hall Track · #1";
  nav.venue("Alpha Hall");
  nav.track("Alpha Hall Track");
  until("Alpha's score in the title bar", (s) => titled(s, alphaScore));
  nav.mode("Rig");

  nav.switchVenue("Beta Room");
  // Beta has nothing open: Arrange, on the song list of Beta's songs.
  const beta = until("Beta's song list", (s) =>
    s.find({ role: "card", label: "Song list" }) && s.find({ role: "row", label: "Beta Room Track" }) ? s : undefined);
  expect(nav.shownMode(beta)).toBe("Arrange");
  expect(beta.find({ role: "row", label: "Alpha Hall Track" })).toBe(undefined);
  nav.track("Beta Room Track");
  until("Beta's score", (s) => titled(s, "Beta Room Track · #1"));

  // Parked is not closed: Alpha comes back in Rig, its score still open.
  nav.switchVenue("Alpha Hall");
  until("Alpha in Rig", (s) => nav.shownMode(s) === "Rig" && s.find({ role: "card", label: "Alpha Hall Venue" }));
  nav.mode("Arrange");
  until("Alpha's score again", (s) => titled(s, alphaScore) && s.find({ role: "card", label: "Song list" }) === undefined);

  // And Beta kept its own.
  nav.switchVenue("Beta Room");
  until("Beta's score again", (s) => titled(s, "Beta Room Track · #1") && nav.shownMode(s) === "Arrange");
});
