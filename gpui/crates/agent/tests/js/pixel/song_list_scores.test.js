// A song's scores filling the song list's scores column, under a real
// renderer.
//
// - the song list is the resting picture, and a row is a door to its scores;
// - the scores column lists every score on the (track, venue);
// - stepping back out (Left) puts the page back as it was, and the page
//   never resizes.

// Two further scores on the seeded pair, so the song has a list. Two clips on
// the seeded score, so its row tells it apart from the empty ones.
fixture({
  seconds: 20,
  clips: [
    { pattern: "pat-glow-a", name: "Glow", start: 2, end: 5 },
    { pattern: "pat-glow-b", name: "Glow", start: 8, end: 11 },
  ],
  extra_scores: 2,
});

const page = () => app.snapshot().find({ role: "card", label: "Song list" });
const scoreRows = () => app.snapshot().findAll({ role: "row" }).filter((n) => n.label.startsWith("#"));
const unfolded = (s) => s.find({ role: "card", label: "Scores of Aurora" }) !== undefined;

test("a song fills the scores column and steps back out", { timeoutMs: 180000 }, () => {
  nav.venue("Test Venue");
  nav.songs();
  until("the song", (s) => s.find({ role: "row", label: "Aurora" }));
  app.frames(6, { waitMs: 16 });
  const foldedBounds = page().bounds;
  const folded = app.screenshot({ node: page() });
  image.keep(folded, "song-list-scores/1-folded");

  nav.scores("Aurora");
  app.frames(6, { waitMs: 16 });
  const labels = scoreRows().map((n) => n.label);
  // Every score on the pair, once each, with the clips telling them apart.
  expect(labels.map((l) => l.split(" ")[0]).sort()).toEqual(["#1", "#2", "#3"]);
  expect(labels.filter((l) => l.includes("· 2 patterns ·")).length).toBe(1);
  assert(labels.every((l) => l.includes("· You ·")), `a fixture has one principal: ${labels}`);
  const openBounds = page().bounds;
  // Where the scores came in: the column's width, from its first score to
  // New score.
  const card = app.snapshot().find({ role: "card", label: "Scores of Aurora" }).bounds;
  const first = nav.scoreRows(app.snapshot(), "Aurora")[0].bounds;
  const last = nav.inScores(app.snapshot(), "Aurora", "button", "New score")[0].bounds;
  // In the shot's own coordinates: the shot is of the page.
  const column = {
    x: card.x - openBounds.x,
    y: first.y - openBounds.y,
    width: card.width,
    height: last.y + last.height - first.y,
  };
  const open = app.screenshot({ node: page() });
  image.keep(open, "song-list-scores/2-unfolded");

  app.key("left");
  until("the scores put away", (s) => !unfolded(s));
  // The click leaves the pointer on the row, which wears its hover fill; the
  // folded picture had the pointer off the list, so take it off again.
  const rest = page().bounds;
  app.scroll({ x: rest.x + rest.width / 2, y: rest.y + rest.height - 20 }, { dy: 0 });
  app.frames(6, { waitMs: 16 });
  const refoldedBounds = page().bounds;
  const refolded = app.screenshot({ node: page() });
  image.keep(refolded, "song-list-scores/3-refolded");

  // The page never resizes: unfolding is a change of content.
  for (const other of [openBounds, refoldedBounds]) expect(other).toEqual(foldedBounds);
  // The two states are different pictures — the change is the scores
  // column filling, where its band of rows turns from ground to lit text —
  // and stepping back out lands on the list as it was. (The old full-width
  // expansion changed 2% of the page; a column of rows changes less of it, so
  // the claim is the relation, not that fraction.)
  expect(image.diff(folded, open, { rect: column }))
    .toBeGreaterThan(10 * image.diff(folded, refolded, { rect: column }));
  expect(image.stats(open, column).meanLuma).toBeGreaterThan(image.stats(folded, column).meanLuma);
  expect(image.diff(folded, refolded)).toBeLessThan(0.02);
});
