// The title bar's trail in Arrange: `venue / song / score`. The song crumb
// reveals the song in the song list; the score crumb names the open score and
// opens a menu of every score of the song — this venue's opened on the spot,
// another venue's after asking, and then in that venue.

// Aurora, scored twice here and once in the Warehouse; Borealis filed in a
// folder Aurora is not in.
fixture({
  seconds: 4,
  extra_scores: 1,
  clips: [{ pattern: "switcher-clip", name: "Chase", start: 1, end: 3, preset: "Chase" }],
  sql: [
    "INSERT INTO venues (id, uid, name) VALUES ('venue-warehouse', '$PRINCIPAL', 'Warehouse')",
    `INSERT INTO scores (id, uid, track_id, venue_id, name)
       VALUES ('score-warehouse', '$PRINCIPAL', 'track-aurora', 'venue-warehouse', 'Warehouse cut')`,
    `INSERT INTO tracks (id, uid, track_hash, title, artist, duration_seconds, file_path)
       VALUES ('track-borealis', '$PRINCIPAL', 'hash-borealis', 'Borealis', 'Fixture', 60.0, '/fixture/borealis.wav')`,
    `INSERT INTO scores (id, uid, track_id, venue_id, name)
       VALUES ('score-borealis', '$PRINCIPAL', 'track-borealis', 'venue-main', 'Borealis cut')`,
    "INSERT INTO folders (id, uid, venue_id, name) VALUES ('folder-openers', '$PRINCIPAL', 'venue-main', 'Openers')",
    "INSERT INTO folder_tracks (folder_id, track_id, uid, venue_id) VALUES ('folder-openers', 'track-borealis', '$PRINCIPAL', 'venue-main')",
  ],
});

const trigger = (s) => s.findAll({ role: "button" }).find((n) => n.label.startsWith("Aurora · #"));
const songCrumb = (s) => {
  const bar = s.find({ role: "card", label: "Title bar" })?.bounds;
  return bar && s.findAll({ role: "button", label: "Aurora" }).find((n) => within(n.bounds, bar));
};
const menu = (s) => s.find({ role: "card", label: "Scores menu" });
const scoreRows = (s) => s.findAll({ role: "row" }).filter((n) => n.label.startsWith("#"));
const openMenu = () => {
  app.click(until("the score crumb", trigger) && trigger(app.snapshot()));
  return until("the menu's scores", (s) => menu(s) && scoreRows(s).length === 3 ? s : undefined);
};
const within = (inner, outer) => inner.y >= outer.y - 1 && inner.y + inner.height <= outer.y + outer.height + 1;
// What the score crumb says: the text inside it.
const crumbText = (s) => {
  const box = trigger(s)?.bounds;
  return box && s.findAll({ role: "text" }).find((n) => n.bounds.x >= box.x && n.bounds.x < box.x + box.width
    && within(n.bounds, box))?.label;
};

test("the title bar reads venue / song / score, and the stage carries no title", () => {
  nav.trackEditor("Test Venue", "Aurora");
  const shot = until("the crumbs", (s) => trigger(s) && songCrumb(s) ? s : undefined);
  const venue = shot.find({ role: "button", label: "Test Venue" }).bounds;
  const song = songCrumb(shot).bounds;
  const score = trigger(shot).bounds;
  const bar = shot.find({ role: "card", label: "Title bar" }).bounds;
  const stage = shot.find({ role: "card", label: "Stage" }).bounds;
  assert(within(score, bar) && within(venue, bar),
    `the crumbs are not in the title bar: ${JSON.stringify({ venue, song, score, bar })}`);
  assert(venue.x + venue.width <= song.x && song.x + song.width <= score.x,
    `the crumbs are not in trail order: ${JSON.stringify({ venue, song, score })}`);
  assert(score.y + score.height <= stage.y, `the score crumb is over the stage: ${JSON.stringify({ score, stage })}`);
});

test("the score crumb names the score, or numbers it when it has no name", () => {
  nav.trackEditor("Test Venue", "Aurora");
  const shot = openMenu();
  const named = scoreRows(shot).find((n) => n.label.endsWith("Fixture Score"));
  const open = trigger(shot).label.split(" · ")[1];
  if (!named.label.startsWith(`${open} `)) {
    // The open one is unnamed: it reads as its number.
    expect(crumbText(shot)).toBe(open);
    app.click(named);
  } else {
    nav.dismiss();
  }
  until("the named score's crumb", (s) => crumbText(s) === "Fixture Score" && !menu(s));
});

test("in Rig and Live the venue stands alone", () => {
  nav.trackEditor("Test Venue", "Aurora");
  until("the score crumb", trigger);
  for (const key of ["1", "3"]) {
    app.key(key);
    until(`the venue alone after ${key}`, (s) => !trigger(s) && !songCrumb(s)
      && s.find({ role: "button", label: "Test Venue" }));
  }
  app.key("2");
  until("the crumbs back in Arrange", (s) => trigger(s) && songCrumb(s));
});

test("the song crumb opens the song list with the song selected", () => {
  nav.trackEditor("Test Venue", "Aurora");
  // Leave the list on a folder Aurora is not in, so the crumb has to move it.
  nav.songs();
  nav.step("the folder", "row", "Folder Openers");
  until("the folder", (s) => s.find({ role: "row", label: "Borealis" }) && !s.find({ role: "row", label: "Aurora" }));
  app.key("escape");
  until("the score again", (s) => !s.find({ role: "card", label: "Song list" }));
  app.click(until("the song crumb", songCrumb) && songCrumb(app.snapshot()));
  const shot = until("the song list on Aurora", (s) =>
    s.find({ role: "card", label: "Song list" }) && nav.scoreRows(s, "Aurora").length > 0 ? s : undefined);
  assert(shot.find({ role: "row", label: "Aurora" }), "Aurora is not listed in the songs column");
  expect(menu(shot)).toBe(undefined);
});

test("the menu lists every score of the song, other venues' by venue", () => {
  nav.trackEditor("Test Venue", "Aurora");
  const shot = openMenu();
  const labels = scoreRows(shot).map((n) => n.label);
  expect(labels.filter((l) => l.endsWith(" · Warehouse")).length).toBe(1);
  expect(labels.filter((l) => !l.includes(" · ")).length).toBe(2);
});

test("a score of this venue opens from the menu", () => {
  nav.trackEditor("Test Venue", "Aurora");
  const open = trigger(until("the trigger", trigger)).label;
  const shot = openMenu();
  const other = scoreRows(shot).find((n) => !n.label.includes(" · ") && !open.endsWith(n.label.split(" ")[0]));
  app.click(other);
  until("the other score open", (s) => trigger(s)?.label === `Aurora · ${other.label.split(" ")[0]}` && !menu(s));
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
  expect(crumbText(there)).toBe("Warehouse cut");
  // And the menu there lists the Test Venue's two as the other venue's.
  const back = (() => {
    app.click(trigger(app.snapshot()));
    return until("the menu's scores", (s) => menu(s) && scoreRows(s).length === 3 ? s : undefined);
  })();
  expect(scoreRows(back).filter((n) => n.label.endsWith(" · Test Venue")).length).toBe(2);
});

test("New score makes an empty score of the song here and opens it", () => {
  nav.trackEditor("Test Venue", "Aurora");
  openMenu();
  nav.step("New score", "row", "New score");
  until("the new score open", (s) => trigger(s)?.label === "Aurora · #3" && crumbText(s) === "#3" && !menu(s));
  app.click(trigger(app.snapshot()));
  until("four scores listed", (s) => menu(s) && scoreRows(s).length === 4);
});

test("Rename names the open score in place", () => {
  nav.trackEditor("Test Venue", "Aurora");
  const open = trigger(openMenu()).label.split(" · ")[1];
  nav.step("Rename", "row", "Rename");
  const nameField = (s) => s.find({ role: "input", label: `Score name ${open}` });
  app.type(nameField(until("the name field", (s) => nameField(s) ? s : undefined)), "Opener");
  app.key("enter");
  until("the renamed crumb", (s) => crumbText(s) === "Opener" && !menu(s));
  const listed = openMenu();
  assert(scoreRows(listed).some((n) => n.label === `${open} Opener`),
    `the menu does not list the new name: ${scoreRows(listed).map((n) => n.label)}`);
});

test("Duplicate copies the open score and its clips, and opens the copy", () => {
  nav.trackEditor("Test Venue", "Aurora");
  const clip = (s) => s.find({ role: "card", label: "Chase" });
  // Stand on whichever of the two scores holds the fixture's clip.
  const first = until("the timeline", (s) => s.find({ role: "card", label: "Waveform" }) ? s : undefined);
  if (!clip(first)) {
    const open = trigger(first).label.split(" · ")[1];
    app.click(scoreRows(openMenu()).find((n) => !n.label.includes(" · ") && !n.label.startsWith(`${open} `)));
  }
  const source = crumbText(until("the clip", (s) => clip(s) && !menu(s) ? s : undefined));
  openMenu();
  nav.step("Duplicate", "row", "Duplicate");
  until("the copy open with the clip", (s) => crumbText(s) === `${source} copy`
    && trigger(s)?.label === "Aurora · #3" && clip(s) && !menu(s));
});

test("Escape closes the menu", () => {
  nav.trackEditor("Test Venue", "Aurora");
  openMenu();
  app.key("escape");
  until("the menu closed", (s) => !menu(s) && trigger(s));
});
