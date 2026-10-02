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
  // Beta has nothing open, and its `+` box lists Beta's songs only.
  until("the empty panel", (s) => s.find({ role: "card", label: "Empty panel" }) !== undefined);
  expect(app.snapshot().find({ role: "button", label: alphaTab })).toBe(undefined);
  app.action("luma::NewTab");
  const offer = until("the + box", (s) => s.find({ role: "card", label: "New tab" }) ? s : undefined);
  const box = offer.find({ role: "card", label: "New tab" }).bounds;
  const listed = offer.findAll((n) => n.role === "row" && n.bounds.y >= box.y && n.bounds.x >= box.x)
    .map((n) => n.label);
  expect(listed).toEqual(["Venue", "Beta Room Track"]);
  app.key("escape");

  // Parked is not closed: going back brings Alpha's tab with it.
  nav.step("the venue switcher", "button", "Beta Room");
  nav.venue("Alpha Hall");
  until("the restored Alpha tab", (s) => s.find({ role: "button", label: alphaTab }) !== undefined);
});
