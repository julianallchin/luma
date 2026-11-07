// A song's scores unfolding on the song list, under a real renderer.
//
// - the song list is the resting picture, and a row is a door to its scores;
// - the unfolded scores list every score on the (track, venue);
// - folding puts the list back as it was, and the page never resizes.

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

test("a song unfolds its scores in place and folds back", { timeoutMs: 180000 }, () => {
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
  const open = app.screenshot({ node: page() });
  image.keep(open, "song-list-scores/2-unfolded");

  app.click(app.snapshot().find({ role: "row", label: "Aurora" }));
  until("the scores folded", (s) => !unfolded(s));
  app.frames(6, { waitMs: 16 });
  const refoldedBounds = page().bounds;
  const refolded = app.screenshot({ node: page() });
  image.keep(refolded, "song-list-scores/3-refolded");

  // The page never resizes: unfolding is a change of content.
  for (const other of [openBounds, refoldedBounds]) expect(other).toEqual(foldedBounds);
  // The two states are different pictures, and folding lands back on the
  // list as it was.
  expect(image.diff(folded, open)).toBeGreaterThan(0.02);
  expect(image.diff(folded, refolded)).toBeLessThan(0.02);
});
