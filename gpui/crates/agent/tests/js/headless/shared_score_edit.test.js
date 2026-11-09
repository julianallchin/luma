// A member edits a score another person wrote, in the venue they share.
//
// There is no permission model inside a venue: whoever can open it can edit
// all of it. The fixture seeds a venue another account owns, this account's
// membership in it, and that account's score on the fixture's song.

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

test("a member edits and renames a score another person wrote", () => {
  // Their score is the venue's only one, so the song opens on it.
  nav.trackEditor("Warehouse", "Aurora");
  until("their score, open", (s) => nav.openScore(s, "Aurora") === "Aurora · #1");

  // The timeline takes a pattern, and it survives a reopen.
  until("the timeline", () => peek("row", "Lane 0"));
  app.click(peek("row", "Lane 0"), { button: "right" });
  until("the pattern dialog", () => peek("card", "Insert pattern dialog"));
  app.type(peek("input", "Search patterns…"), "Wash");
  app.frames(3);
  app.key("enter");
  until("the inserted clip", (s) => s.findAll({ role: "card", label: CARD }).length === 1);
  nav.reopen("Aurora");
  until("the clip, stored", (s) => s.findAll({ role: "card", label: CARD }).length === 1);

  // On the list the row still says whose it is, and its name takes a rename.
  nav.scores("Aurora");
  const listed = until("their score", (s) => theirs(s) ? s : undefined);
  expect(theirs(listed).label).toContain(OTHER.slice(0, 6));
  app.click(nameOf(listed), { count: 2 });
  const field = () => app.snapshot().find({ role: "input", label: "Score name #1" });
  until("the name is a field", () => field() !== undefined);
  app.key("ctrl-a backspace");
  app.type(field(), "Our set");
  app.key("enter");
  until("the new name", (s) => nameOf(s)?.label === "Score name #1 = Our set");
});
