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

// Leaving a room revokes its track as a subject without throwing the work
// away: the new-tab offer stops offering a track the current browser cannot
// open, while the tabs open under that track are parked under it and come
// back the moment it is picked again. Asserting only the first half would
// pass a shell that closed those tabs outright.
test("switching venues parks the track subject and revokes it from the new-tab offer", () => {
  nav.venue("Alpha Hall");
  until("Alpha's track", (s) => s.find({ role: "row", label: "Alpha Hall Track" }) !== undefined);
  nav.track("Alpha Hall Track");
  until("the selected Alpha tab", (s) => s.find({ role: "button", label: "Alpha Hall Track" }) !== undefined);

  nav.step("the venue switcher", "button", "Alpha Hall");
  nav.venue("Beta Room");
  until("Beta's track", (s) => s.find({ role: "row", label: "Beta Room Track" }) !== undefined);
  // Beta has nothing open, so the offer is the panel's empty state.
  const offer = until("the empty panel's offer", (s) => s.find({ role: "card", label: "Empty panel" }) !== undefined);
  expect(offer.find({ role: "button", label: "Track editor" }).enabled).toBe(false);
  expect(offer.find({ role: "text", label: "Select a track first" }) !== undefined).toBe(true);
  // The strip belongs to the picked track, so leaving Alpha parks its tabs.
  expect(offer.find({ role: "button", label: "Alpha Hall Track" })).toBe(undefined);

  // Parked is not closed: go back and re-pick the track.
  nav.step("the venue switcher", "button", "Beta Room");
  nav.venue("Alpha Hall");
  until("Alpha's track again", (s) => s.find({ role: "row", label: "Alpha Hall Track" }) !== undefined);
  // Its tabs are parked under the track, not the room.
  expect(app.snapshot().find({ role: "button", label: "Alpha Hall Track" })).toBe(undefined);
  nav.track("Alpha Hall Track");
  until("the restored Alpha tab", (s) => s.find({ role: "button", label: "Alpha Hall Track" }) !== undefined);
});
