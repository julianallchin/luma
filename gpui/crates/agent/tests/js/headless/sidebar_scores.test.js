// The sidebar's second level, driven end to end with motion snapped.
//
// - a track row is a door to the track's documents: pressing it goes a level
//   deeper, not straight onto a timeline the sidebar guessed at;
// - the level lists every score on the `(track, venue)`, and choosing one
//   moves the timeline onto it without leaving the list;
// - `New score` mints another and opens it;
// - Back returns to the track list.

// Two further scores on the seeded pair, so the level lists three.
fixture({ seconds: 8, clips: [{ pattern: "pat-glow", name: "Glow", start: 1, end: 4 }], extra_scores: 2 });

test("the row opens a track's scores, and the level switches, mints and pops", () => {
  const ordinal = () => app.snapshot().findAll({ role: "text" }).find((n) => n.label.startsWith("Score #"))?.label;
  const rows = () => app.snapshot().findAll({ role: "row" }).filter((n) => n.label.startsWith("#"));
  const ordinals = (labels) => labels.map((l) => l.split(" ")[0]).sort();

  nav.trackEditor("Test Venue", "Aurora");
  until("the timeline on some score", () => ordinal() !== undefined);
  const opened = ordinal();

  // Back in, on the row itself.
  nav.scores("Aurora");
  const listed = rows().map((n) => n.label);
  expect(ordinals(listed)).toEqual(["#1", "#2", "#3"]);
  expect(listed.filter((l) => l.includes("· 1 clips ·")).length).toBe(1);
  // The track list is gone, not merely covered: a level parked off the edge
  // would still be a tab stop.
  expect(app.snapshot().find({ role: "input", label: "Search tracks" })).toBe(undefined);

  // Choosing is reading: the list stays while the timeline moves.
  const other = rows().find((n) => !n.label.startsWith(`${opened.slice("Score ".length)} `));
  app.click(other);
  until("the timeline on the other score", () => ordinal() !== opened);
  const switched = ordinal();
  expect(app.snapshot().find({ role: "card", label: "Scores level" }) !== undefined).toBe(true);

  // Mint another, which opens it: a new score, not a hand-back of one on the
  // pair.
  app.click(app.snapshot().find({ role: "button", label: "New score" }));
  until("a fourth score, open", () => rows().length === 4 && ordinal() !== switched);
  expect(ordinals(rows().map((n) => n.label))).toEqual(["#1", "#2", "#3", "#4"]);
  expect(ordinal()).toBe("Score #4");

  // …and back out.
  app.click(app.snapshot().find({ role: "button", label: "Back to tracks" }));
  until("the track list again", (s) =>
    s.find({ role: "input", label: "Search tracks" }) !== undefined && s.find({ role: "card", label: "Scores level" }) === undefined);
  expect(app.snapshot().find({ role: "row", label: "Aurora" }) !== undefined).toBe(true);
});

test("double-clicking a score's name renames it in place", () => {
  const names = () => app.snapshot().findAll({ role: "text" }).filter((n) => n.label.startsWith("Score name #"));
  const nameOf = (ordinal) => names().find((n) => n.label.startsWith(`Score name ${ordinal} = `));
  const field = (ordinal) => app.snapshot().find({ role: "input", label: `Score name ${ordinal}` });

  nav.trackEditor("Test Venue", "Aurora");
  nav.scores("Aurora");
  until("three names", () => names().length === 3);
  const before = nameOf("#1").label;

  // Escape drops the draft and writes nothing.
  app.click(nameOf("#1"), { count: 2 });
  until("the name is a field", () => field("#1") !== undefined);
  app.key("ctrl-a backspace");
  app.type(field("#1"), "Nope");
  app.key("escape");
  until("the field is gone", () => field("#1") === undefined);
  expect(nameOf("#1").label).toBe(before);
  // Escape ended the rename only; the level is still up.
  expect(app.snapshot().find({ role: "card", label: "Scores level" }) !== undefined).toBe(true);

  // An empty name is not a name.
  app.click(nameOf("#1"), { count: 2 });
  until("the name is a field", () => field("#1") !== undefined);
  app.key("ctrl-a backspace enter");
  until("the field is gone", () => field("#1") === undefined);
  app.frames(3);
  expect(nameOf("#1").label).toBe(before);

  // Enter saves, and the list shows the new name.
  app.click(nameOf("#1"), { count: 2 });
  until("the name is a field", () => field("#1") !== undefined);
  app.key("ctrl-a backspace");
  app.type(field("#1"), "Opening set");
  app.key("enter");
  until("the new name in the list", () => nameOf("#1")?.label === "Score name #1 = Opening set");

  // Blur saves too: a press on another row commits the draft.
  app.click(nameOf("#2"), { count: 2 });
  until("the name is a field", () => field("#2") !== undefined);
  app.key("ctrl-a backspace");
  app.type(field("#2"), "Encore");
  app.click(nameOf("#3"));
  until("the blurred name in the list", () => nameOf("#2")?.label === "Score name #2 = Encore");
});
