// A song's scores on the song list, driven end to end with motion snapped.
//
// - a song row is a door to the song's scores: pressing it lists them in the
//   scores column, not straight onto a timeline the list guessed at;
// - the scores list every score on the `(track, venue)`, and choosing one
//   opens it and closes the list;
// - `New score` mints another and opens it;
// - Left from the song puts the scores away again.

// Two further scores on the seeded pair, so the song lists three.
fixture({ seconds: 8, clips: [{ pattern: "pat-glow", name: "Glow", start: 1, end: 4 }], extra_scores: 2 });

const songList = (s) => s.find({ role: "card", label: "Song list" }) !== undefined;
const unfolded = (s) => s.find({ role: "card", label: "Scores of Aurora" }) !== undefined;

test("the row lists a song's scores, and they switch, mint and go away", () => {
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
  expect(listed.filter((l) => l.includes("· 1 patterns ·")).length).toBe(1);
  // The list covers the timeline rather than sitting beside it.
  expect(app.snapshot().find({ role: "card", label: "Waveform" })).toBe(undefined);

  // Choosing opens the score and closes the list over it.
  const other = rows().find((n) => !n.label.startsWith(`${opened.slice("Score ".length)} `));
  app.click(other);
  until("the timeline on the other score", (s) => ordinal() !== opened && !songList(s));

  // Mint another, which opens it: a new score, not a hand-back of one on the
  // pair.
  nav.scores("Aurora");
  app.click(app.snapshot().find({ role: "button", label: "New score" }));
  until("a fourth score, open", (s) => ordinal() === "Score #4" && !songList(s));
  nav.scores("Aurora");
  until("four scores listed", () => rows().length === 4);
  expect(ordinals(rows().map((n) => n.label))).toEqual(["#1", "#2", "#3", "#4"]);

  // …and put away again: Left from the song steps back out of it.
  app.click(app.snapshot().find({ role: "row", label: "Aurora" }));
  app.key("left");
  until("the scores put away", (s) => songList(s) && !unfolded(s) && rows().length === 0);
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
  // Escape ended the rename only; the scores are still up.
  expect(unfolded(app.snapshot())).toBe(true);

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
