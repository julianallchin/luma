// A score someone else wrote, in a venue the reader can open.
//
// - Its menu offers a copy and nothing that would change it.
// - Open, it says it is read-only and adds nothing; the copy the editor
//   offers is the reader's own, and takes edits.

const OTHER = "99999999-8888-7777-6666-555555555555";

fixture({
  clips: [{ pattern: "pat-glow", name: "Glow", start: 1, end: 4 }],
  sql: [
    `INSERT INTO scores (id, uid, track_id, venue_id, name)
       VALUES ('score-theirs', '${OTHER}', 'track-aurora', 'venue-main', 'Theirs')`,
  ],
});

const rows = (s) => s.findAll({ role: "row" }).filter((n) => n.label.startsWith("#"));
// A row leads with its author: "You", or the start of someone else's id.
const mine = (s) => rows(s).find((n) => n.label.includes("· You ·"));
const theirs = (s) => rows(s).find((n) => !n.label.includes("· You ·"));
const menu = (s) => s.find({ role: "card", label: "Context menu" });
const button = (s, label) => s.find({ role: "button", label });

function toScores() {
  nav.venue("Test Venue");
  nav.scores("Aurora");
  return until("both scores listed", (s) => (theirs(s) && mine(s) ? s : undefined));
}

function openMenu(row) {
  app.click(row, { button: "right" });
  return until("the score menu", (s) => (menu(s) !== undefined ? s : undefined));
}

test(`someone else's score can be duplicated from its menu, and not renamed or deleted`, () => {
  const level = toScores();
  const own = openMenu(mine(level));
  for (const label of ["Rename", "Duplicate", "Delete score"]) {
    expect(button(own, label) !== undefined).toBe(true);
  }
  app.key("escape");
  until("the menu to close", (s) => menu(s) === undefined);

  const other = openMenu(theirs(app.snapshot()));
  expect(button(other, "Duplicate") !== undefined).toBe(true);
  expect(button(other, "Rename")).toBe(undefined);
  expect(button(other, "Delete score")).toBe(undefined);
});

test(`someone else's score opens read-only, and its copy is the reader's to edit`, () => {
  app.click(theirs(toScores()));
  const open = until("the read-only score open", (s) =>
    button(s, "Duplicate to edit") !== undefined ? s : undefined);
  expect(button(open, "Add pattern").enabled).toBe(false);

  app.click(button(open, "Duplicate to edit"));
  const copy = until("the copy open", (s) =>
    button(s, "Add pattern")?.enabled && button(s, "Duplicate to edit") === undefined ? s : undefined);
  expect(button(copy, "Add pattern").enabled).toBe(true);
});
