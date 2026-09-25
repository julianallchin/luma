// Right-clicking a score, and what the menu can do to it.
//
// - A right-click on a score row raises the one context menu, and its rows
//   are findable by role and label like every other control.
// - Deleting a score that holds clips asks first, quoting the row's count,
//   and the row survives a cancel.
// - Confirming removes it; when it was the score on the timeline, the editor
//   lands in its defined `No score` state.
// - An empty score goes without a dialog: a confirmation that can only be
//   answered one way is one nobody reads.

// Two extra scores: an empty one to delete without a dialog, and something
// left on the list afterwards.
fixture({
  clips: [{ pattern: "pat-glow", name: "Glow", start: 1, end: 4 }],
  extra_scores: 2,
});

const rows = (s) => s.findAll({ role: "row" }).filter((n) => n.label.startsWith("#"));
const labels = (s) => rows(s).map((n) => n.label);
const withClips = (s) => rows(s).find((n) => n.label.includes("· 1 clips ·"));
const empty = (s) => rows(s).find((n) => n.label.includes("· 0 clips ·"));
const menu = (s) => s.find({ role: "card", label: "Context menu" });
const dialog = (s) => s.find({ role: "card", label: "Confirm dialog" });
// The menu's row and the dialog's confirm button share a label; the dialog's
// is painted last.
const lastDelete = (s) => s.findAll({ role: "button", label: "Delete score" }).at(-1);

function openMenu(row) {
  app.click(row, { button: "right" });
  return until("the score menu", (s) => menu(s) !== undefined);
}

function toScores() {
  nav.venue("Test Venue");
  nav.scores("Aurora");
  return app.snapshot();
}

test(`a right-click on a score raises a findable menu and escape takes it away`, () => {
  const level = toScores();
  expect(menu(level)).toBe(undefined);
  const row = withClips(level);
  const shot = openMenu(row);
  const card = menu(shot);
  const item = shot.find({ role: "button", label: "Delete score" });

  // A node's bounds are clipped to its content mask, so a card cut off by
  // the sidebar's clip would have no area — and nothing could click it.
  expect(card.bounds.width * card.bounds.height).toBeGreaterThan(0);
  expect(item.bounds.width * item.bounds.height).toBeGreaterThan(0);
  // It hangs at the pointer, not at the top of the column.
  expect(card.bounds.y).toBeGreaterThan(row.bounds.y - 1);

  // Escape is the other door out.
  app.key("escape");
  until("the menu to close", (s) => menu(s) === undefined);
});

test(`deleting a score with clips asks first and the answer decides`, () => {
  const level = toScores();
  const listed = labels(level);

  app.click(openMenu(withClips(level)).find({ role: "button", label: "Delete score" }));
  const asked = until("the confirmation", (s) => dialog(s) !== undefined);
  // The sentence quotes what the row said, so the two cannot disagree.
  expect(asked.findAll({ role: "text" }).map((n) => n.label).join(" | ")).toContain("1 clip");

  app.click(asked.find({ role: "button", label: "Cancel" }));
  const cancelled = until("the dialog gone", (s) => dialog(s) === undefined);
  expect(labels(cancelled)).toEqual(listed);

  app.click(openMenu(withClips(cancelled)).find({ role: "button", label: "Delete score" }));
  app.click(lastDelete(until("the confirmation again", (s) => dialog(s) !== undefined)));
  const after = until("the score gone from the list", (s) => withClips(s) === undefined && dialog(s) === undefined);
  expect(labels(after).length).toBe(listed.length - 1);
});

test(`deleting the open score leaves the editor with no score`, () => {
  const ordinal = (s) => s.findAll({ role: "text" }).find((n) => n.label.startsWith("Score #"));
  const level = toScores();
  const before = labels(level);

  // Choosing a score from this level opens it and stays here: the list and
  // the editor on the same document.
  app.click(withClips(level));
  const open = until("the timeline on the chosen score", (s) => ordinal(s) !== undefined);

  // `#N` is a position and renumbers on delete; the clip count names the
  // score across the deletion — the fixture's one clip is on this one.
  app.click(openMenu(withClips(open)).find({ role: "button", label: "Delete score" }));
  const next = app.snapshot();
  // Either door: a score with clips asks, an empty one does not.
  if (dialog(next) !== undefined) app.click(lastDelete(next));
  // The editor lets go at once; the list is re-read when the seam answers.
  const gone = until("the editor off the score and the list re-read", (s) =>
    s.findAll({ role: "text" }).some((n) => n.label === "No score")
      && rows(s).length === before.length - 1);

  expect(ordinal(gone)).toBe(undefined);
  assert(withClips(gone) === undefined, `the deleted score is still listed: ${labels(gone)}`);
});

test(`deleting an empty score does not ask`, () => {
  const level = toScores();
  const before = labels(level);
  app.click(openMenu(empty(level)).find({ role: "button", label: "Delete score" }));
  const after = until("one fewer score", (s) => rows(s).length === before.length - 1);
  assert(dialog(after) === undefined, "an empty score raised a confirmation");
});
