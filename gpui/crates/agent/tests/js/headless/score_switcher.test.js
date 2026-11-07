// The stage's song / score menu in Arrange: the open song and score named
// over the stage, and a menu that goes to the song list or to any other
// score of the song — this venue's opened on the spot, another venue's after
// asking, and then in that venue.

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
  app.click(until("the stage's score trigger", trigger) && trigger(app.snapshot()));
  return until("the menu's scores", (s) => menu(s) && scoreRows(s).length === 3 ? s : undefined);
};

test("the title bar's middle is empty and the stage names the open score", () => {
  nav.trackEditor("Test Venue", "Aurora");
  const shot = until("the score on the stage", (s) => trigger(s) ? s : undefined);
  const named = trigger(shot).bounds;
  const stage = shot.find({ role: "card", label: "Stage" }).bounds;
  const bar = shot.find({ role: "card", label: "Title bar" }).bounds;
  // Over the stage's top edge, centred on it.
  assert(named.y >= stage.y && named.y + named.height <= stage.y + stage.height / 2,
    `the trigger is not over the stage's top: ${JSON.stringify({ named, stage })}`);
  assert(Math.abs(named.x + named.width / 2 - (stage.x + stage.width / 2)) <= 1,
    `the trigger is not centred on the stage: ${JSON.stringify({ named, stage })}`);
  // Nothing in the title bar but the venue menu, the agent toggle, the modes
  // and the window's own corner.
  const inBar = shot.nodes.filter((n) => n.bounds.width > 0 && n.bounds.y >= bar.y
    && n.bounds.y + n.bounds.height <= bar.y + bar.height && n.bounds.x > bar.x + bar.width * 0.3
    && n.bounds.x + n.bounds.width < bar.x + bar.width * 0.7);
  expect(inBar.map((n) => `${n.role}:${n.label}`)).toEqual([]);
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
