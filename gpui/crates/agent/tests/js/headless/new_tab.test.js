// The tab strip's `+` is a combo box (docs/specs/venue-tabs.md rule 9).
//
// - ⌘T presses the `+`: the box opens with the caret in its filter;
// - the root lists "Venue" and every song in the venue, and typing filters;
// - ↓ and ↵ walk into a song's scores, ↵ on a score opens its tab;
// - ← goes back to the list as it was left, Escape closes;
// - the pointer does the same: a click picks.

// A second song in the venue, so the filter has something to drop.
fixture({
  seconds: 8,
  clips: [{ pattern: "pat-glow", name: "Glow", start: 1, end: 4 }],
  extra_scores: 1,
  sql: [
    `INSERT INTO tracks (id, uid, track_hash, title, artist, duration_seconds, file_path)
       VALUES ('track-borealis', '$PRINCIPAL', 'hash-borealis', 'Borealis', 'Night Shift', 60.0, '/fixture/borealis.wav')`,
    `INSERT INTO scores (id, uid, track_id, venue_id, name)
       VALUES ('score-borealis', '$PRINCIPAL', 'track-borealis', 'venue-main', 'Borealis score')`,
  ],
});

// The box as a reader sees it: its rows, top to bottom, and its field.
function box(s) {
  const card = s.find({ role: "card", label: "New tab" });
  if (card === undefined) return undefined;
  const b = card.bounds;
  const inside = (n) =>
    n.bounds.width > 0 && n.bounds.height > 0
    && n.bounds.x >= b.x && n.bounds.y >= b.y
    && n.bounds.x + n.bounds.width <= b.x + b.width + 1
    && n.bounds.y + n.bounds.height <= b.y + b.height + 1;
  return {
    rows: s.findAll((n) => n.role === "row" && inside(n)).map((n) => n.label),
    row: (label) => s.find((n) => n.role === "row" && n.label === label && inside(n)),
    field: s.find((n) => n.role === "input" && inside(n)),
  };
}

const tabs = (s) => s.findAll({ role: "button" })
  .filter((n) => n.label.startsWith("Close ") && n.label !== "Close next tab")
  .map((n) => n.label.slice("Close ".length));

function open() {
  app.action("luma::NewTab");
  return box(until("the box, its field focused", (s) => {
    const b = box(s);
    return b !== undefined && b.field?.focused && b.rows.length > 0;
  }));
}

test("the keyboard walks from a song to its score and opens it", () => {
  nav.venue("Test Venue");
  until("the songs", (s) => s.find({ role: "row", label: "Borealis" }) !== undefined);

  const root = open();
  // "Venue" leads; the songs follow in the sidebar's order.
  expect(root.rows[0]).toBe("Venue");
  expect(root.rows.slice(1).sort()).toEqual(["Aurora", "Borealis"]);

  // Typed straight into the field: the caret is there without a click. The
  // match is the add-track dialog's, so an artist finds its song.
  app.key("s h i f t");
  until("the filtered list", (s) => box(s)?.rows.join() === "Borealis");
  app.key("ctrl-a backspace a u r");
  until("Aurora alone", (s) => box(s)?.rows.join() === "Aurora");

  // ↵ goes a level in, to the song's scores in this venue.
  app.key("enter");
  const scores = box(until("Aurora's scores", (s) => {
    const rows = box(s)?.rows ?? [];
    return rows.length === 2 && rows.every((r) => r.startsWith("#"));
  })).rows;
  expect(scores.map((r) => r.split(" ")[0]).sort()).toEqual(["#1", "#2"]);

  // ← is the way back, to the list as it was left.
  app.key("left");
  until("the filtered root again", (s) => box(s)?.rows.join() === "Aurora");
  app.key("right");
  until("the scores again", (s) => box(s)?.rows.join() === scores.join());

  // ↓ moves the cursor, ↵ opens the score under it and closes the box.
  const second = scores[1];
  app.key("down enter");
  until("the box closed", (s) => box(s) === undefined);
  const opened = tabs(until("the score's tab", (s) => tabs(s).length === 1));
  expect(opened[0]).toBe(`Aurora · ${second.split(" ")[0]}`);

  // ⌘T on an open score reveals its tab rather than minting a second.
  open();
  app.key("a u r enter");
  until("the scores", (s) => box(s)?.rows.join() === scores.join());
  app.key("down enter");
  until("the box closed", (s) => box(s) === undefined);
  app.frames(4);
  expect(tabs(app.snapshot())).toEqual(opened);
});

test("typing venue and enter opens the venue tab; escape closes", () => {
  nav.venue("Test Venue");
  until("the songs", (s) => s.find({ role: "row", label: "Borealis" }) !== undefined);

  // Escape closes from any level.
  open();
  app.key("down enter");
  until("a level in", (s) => {
    const rows = box(s)?.rows ?? [];
    return rows.length > 0 && rows.every((r) => r.startsWith("#"));
  });
  app.key("escape");
  until("the box closed", (s) => box(s) === undefined);

  open();
  app.key("v e n u e enter");
  until("the box closed", (s) => box(s) === undefined);
  until("the venue tab", (s) => s.find({ role: "card", label: "Test Venue Venue" }) !== undefined);
  expect(tabs(app.snapshot())).toEqual(["Venue"]);
});

test("the pointer picks: a click on a song, then on a score", () => {
  nav.venue("Test Venue");
  until("the songs", (s) => s.find({ role: "row", label: "Borealis" }) !== undefined);

  // The `+` is there with no tab open; it is the same box ⌘T opens.
  nav.step("the + button", "button", "new-tab");
  const root = box(until("the box", (s) => box(s)?.rows.length > 0));
  app.click(root.row("Borealis"));
  const level = box(until("Borealis's score", (s) => box(s)?.rows.length === 1 && box(s).rows[0].startsWith("#")));
  app.click(level.row(level.rows[0]));
  until("the box closed", (s) => box(s) === undefined);
  until("the score's tab", (s) => tabs(s).join() === "Borealis · #1");
});
