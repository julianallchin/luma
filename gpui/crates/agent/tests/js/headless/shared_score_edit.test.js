// A member opens a score another person wrote, in the venue they share.
//
// The venue's rig is every member's to edit; a score is its author's. The
// member reads it, and changes a copy of their own. The fixture seeds a venue
// another account owns, this account's membership in it, and that account's
// score on the fixture's song.

const OTHER = "99999999-8888-7777-6666-555555555555";
fixture({
  seconds: 20,
  window: [1400, 900],
  sql: [
    `INSERT INTO venues (id, uid, name) VALUES ('warehouse', '${OTHER}', 'Warehouse')`,
    `INSERT INTO venue_nodes (id, uid, venue_id, kind) VALUES ('warehouse:venue', '${OTHER}', 'warehouse', 'venue')`,
    "INSERT INTO venue_members (id, uid, venue_id, role) VALUES ('warehouse:$PRINCIPAL', '$PRINCIPAL', 'warehouse', 'member')",
    `INSERT INTO scores (id, uid, track_id, venue_id, name) VALUES ('their-score', '${OTHER}', 'track-aurora', 'warehouse', 'Their set')`,
  ],
});

const CARD = "Wash";
const theirs = (s) => nav.scoreRows(s, "Aurora").find((n) => n.label.startsWith("#1 "));
const nameOf = (s) => s.findAll({ role: "text" }).find((n) => n.label.startsWith("Score name #1 = "));
const copyButton = (s) => s.find({ role: "button", label: "Duplicate to edit" });

test("a member reads a score another person wrote, and edits a copy of it", () => {
  // Their score is the venue's only one, so the song opens on it.
  nav.trackEditor("Warehouse", "Aurora");
  until("their score, open", (s) => nav.openScore(s, "Aurora") === "Aurora · #1");

  // The timeline offers no pattern: it is theirs.
  until("the timeline", () => peek("row", "Lane 0"));
  const open = until("the score says it is read-only", (s) => copyButton(s) ? s : undefined);
  expect(open.find({ role: "button", label: "Add pattern" }).enabled).toBe(false);
  app.click(peek("row", "Lane 0"), { button: "right" });
  app.frames(3);
  expect(peek("card", "Insert pattern dialog")).toBe(undefined);

  // The copy is the member's own: it takes a pattern, and keeps it.
  app.click(copyButton(app.snapshot()));
  until("the copy, open", (s) => nav.openScore(s, "Aurora") === "Aurora · #2" && !copyButton(s));
  until("the copy's timeline", () => peek("row", "Lane 0"));
  app.click(peek("row", "Lane 0"), { button: "right" });
  until("the pattern dialog", () => peek("card", "Insert pattern dialog"));
  app.type(peek("input", "Search patterns…"), "Wash");
  app.frames(3);
  app.key("enter");
  until("the inserted clip", (s) => s.findAll({ role: "card", label: CARD }).length === 1);
  nav.reopen("Aurora");
  until("the clip, stored", (s) => s.findAll({ role: "card", label: CARD }).length === 1);

  // On the list their row still says whose it is, and its menu only copies.
  nav.scores("Aurora");
  const listed = until("their score", (s) => theirs(s) ? s : undefined);
  expect(theirs(listed).label).toContain(OTHER.slice(0, 6));
  app.click(nameOf(listed), { button: "right" });
  const menu = until("the score menu", (s) => s.find({ role: "button", label: "Duplicate" }) ? s : undefined);
  expect(menu.find({ role: "button", label: "Rename" })).toBe(undefined);
  expect(menu.find({ role: "button", label: "Delete score" })).toBe(undefined);
});
