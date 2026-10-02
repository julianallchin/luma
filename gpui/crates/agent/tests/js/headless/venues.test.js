// Venue selection through the production app tree. The launch, restore and
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

// A venue is a project: leaving it parks its whole tab set, and coming back
// brings the set back as it was. The new-tab offer in the other room cannot
// open a track from the room that was left.
test("switching venues parks the venue's tabs and brings them back", () => {
  const alphaTab = "Alpha Hall Track · #1";
  nav.venue("Alpha Hall");
  until("Alpha's track", (s) => s.find({ role: "row", label: "Alpha Hall Track" }) !== undefined);
  nav.track("Alpha Hall Track");
  until("the Alpha tab", (s) => s.find({ role: "button", label: alphaTab }) !== undefined);

  nav.step("the venue switcher", "button", "Alpha Hall");
  nav.venue("Beta Room");
  until("Beta's track", (s) => s.find({ role: "row", label: "Beta Room Track" }) !== undefined);
  // Beta has nothing open, so the offer is the panel's empty state.
  const offer = until("the empty panel's offer", (s) => s.find({ role: "card", label: "Empty panel" }) !== undefined);
  expect(offer.find({ role: "button", label: "Track editor" }).enabled).toBe(false);
  expect(offer.find({ role: "text", label: "Select a track first" }) !== undefined).toBe(true);
  expect(offer.find({ role: "button", label: alphaTab })).toBe(undefined);

  // Parked is not closed: going back brings Alpha's tab with it.
  nav.step("the venue switcher", "button", "Beta Room");
  nav.venue("Alpha Hall");
  until("the restored Alpha tab", (s) => s.find({ role: "button", label: alphaTab }) !== undefined);
});
