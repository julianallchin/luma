// The title bar's song crumb in Arrange: `venue / song`, the song's crumb a
// menu that goes to the song list or to any other score of the song — this
// venue's opened on the spot, another venue's after asking, and then in that
// venue.

// Aurora, scored twice here and once in the Warehouse.
fixture({
  seconds: 4,
  extra_scores: 1,
  sql: [
    "INSERT INTO venues (id, uid, name) VALUES ('venue-warehouse', '$PRINCIPAL', 'Warehouse')",
    `INSERT INTO scores (id, uid, track_id, venue_id, name)
       VALUES ('score-warehouse', '$PRINCIPAL', 'track-aurora', 'venue-warehouse', 'Warehouse cut')`,
  ],
});

const trigger = (s) => s.findAll({ role: "button" }).find((n) => n.label.startsWith("Aurora · #"));
const menu = (s) => s.find({ role: "card", label: "Scores menu" });
const scoreRows = (s) => s.findAll({ role: "row" }).filter((n) => n.label.startsWith("#"));
const openMenu = () => {
  app.click(until("the song crumb", trigger) && trigger(app.snapshot()));
  return until("the menu's scores", (s) => menu(s) && scoreRows(s).length === 3 ? s : undefined);
};
const within = (inner, outer) => inner.y >= outer.y - 1 && inner.y + inner.height <= outer.y + outer.height + 1;
// The number of the open score, as the crumb shows it beside the song.
const shownNumber = (s) => texts(s).find((t) => /^#\d+$/.test(t));

test("the title bar reads venue / song, and the stage carries no title", () => {
  nav.trackEditor("Test Venue", "Aurora");
  const shot = until("the song crumb", (s) => trigger(s) ? s : undefined);
  const song = trigger(shot).bounds;
  const venue = shot.find({ role: "button", label: "Test Venue" }).bounds;
  const bar = shot.find({ role: "card", label: "Title bar" }).bounds;
  const stage = shot.find({ role: "card", label: "Stage" }).bounds;
  assert(within(song, bar) && within(venue, bar),
    `the crumbs are not in the title bar: ${JSON.stringify({ venue, song, bar })}`);
  assert(song.x >= venue.x + venue.width, `the song does not follow the venue: ${JSON.stringify({ venue, song })}`);
  assert(song.y + song.height <= stage.y, `the song crumb is over the stage: ${JSON.stringify({ song, stage })}`);
  // Two scores here: the open one's number rides beside the song.
  expect(shownNumber(shot)).toBe(trigger(shot).label.split(" · ")[1]);
});

test("in Rig and Live the venue stands alone", () => {
  nav.trackEditor("Test Venue", "Aurora");
  until("the song crumb", trigger);
  for (const key of ["1", "3"]) {
    app.key(key);
    until(`the venue alone after ${key}`, (s) => !trigger(s) && s.find({ role: "button", label: "Test Venue" }));
  }
  app.key("2");
  until("the song crumb back in Arrange", trigger);
});

test("the menu lists every score of the song, other venues' by venue", () => {
  nav.trackEditor("Test Venue", "Aurora");
  const shot = openMenu();
  const labels = scoreRows(shot).map((n) => n.label);
  expect(labels.filter((l) => l.endsWith(" · Warehouse")).length).toBe(1);
  expect(labels.filter((l) => !l.includes(" · ")).length).toBe(2);
  expect(shot.find({ role: "row", label: "Song list" }) !== undefined).toBe(true);
});

test("a score of this venue opens from the menu", () => {
  nav.trackEditor("Test Venue", "Aurora");
  const open = trigger(until("the trigger", trigger)).label;
  const shot = openMenu();
  const other = scoreRows(shot).find((n) => !n.label.includes(" · ") && !open.endsWith(n.label.split(" ")[0]));
  app.click(other);
  until("the other score open", (s) => trigger(s)?.label === `Aurora · ${other.label.split(" ")[0]}` && !menu(s));
});

test("the song list row opens the song list", () => {
  nav.trackEditor("Test Venue", "Aurora");
  openMenu();
  nav.step("the song list row", "row", "Song list");
  until("the song list", (s) => s.find({ role: "card", label: "Song list" }) && !menu(s));
});

test("another venue's score asks, then switches venue and opens it", () => {
  nav.trackEditor("Test Venue", "Aurora");
  const shot = openMenu();
  app.click(scoreRows(shot).find((n) => n.label.endsWith(" · Warehouse")));
  const asked = until("the question", (s) => s.find({ role: "card", label: "Confirm dialog" }) ? s : undefined);
  expect(texts(asked)).toContain("Switch to Warehouse?");

  // Cancel leaves the window where it was.
  nav.step("cancel", "button", "Cancel");
  until("still here", (s) => !s.find({ role: "card", label: "Confirm dialog" })
    && s.find({ role: "button", label: "Test Venue" }));

  app.click(scoreRows(openMenu()).find((n) => n.label.endsWith(" · Warehouse")));
  nav.step("switch", "button", "Switch");
  const there = until("the Warehouse's score, open in Arrange", (s) =>
    s.find({ role: "button", label: "Warehouse" })?.bounds.y < 44
      && trigger(s)?.label === "Aurora · #1"
      && s.find({ role: "card", label: "Waveform" })
      && !s.find({ role: "card", label: "Song list" }) ? s : undefined);
  expect(nav.shownMode(there)).toBe("Arrange");
  // The song's only score there needs no number.
  expect(shownNumber(there)).toBe(undefined);
  // And the menu there lists the Test Venue's two as the other venue's.
  const back = openMenu();
  expect(scoreRows(back).filter((n) => n.label.endsWith(" · Test Venue")).length).toBe(2);
});

test("Escape closes the menu", () => {
  nav.trackEditor("Test Venue", "Aurora");
  openMenu();
  app.key("escape");
  until("the menu closed", (s) => !menu(s) && trigger(s));
});
