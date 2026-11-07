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

test("every other row is striped, under hover and under both selections", { timeoutMs: 180000 }, () => {
  nav.venue("Test Venue");
  nav.scores("Aurora");
  app.frames(6, { waitMs: 16 });
  const shell = page().bounds;
  // A sliver at a row's right end, inside its padding: only its fill shows.
  const sliver = (node) => ({
    x: node.bounds.x + node.bounds.width - 6 - shell.x,
    y: node.bounds.y + 4 - shell.y,
    width: 3,
    height: node.bounds.height - 8,
  });
  const luma = (shot, node) => image.stats(shot, sliver(node)).meanLuma;
  // The scores column's run under its heading: three scores, then New score.
  const rows = () => {
    const s = app.snapshot();
    return [...nav.scoreRows(s, "Aurora"), nav.inScores(s, "Aurora", "button", "New score")[0]]
      .sort((a, b) => a.bounds.y - b.bounds.y);
  };
  const foot = { x: shell.x + shell.width / 2, y: shell.y + shell.height - 20 };
  // The pointer off the rows.
  app.scroll(foot, { dy: 0 });
  app.frames(6, { waitMs: 16 });
  const rest = app.screenshot({ node: page() });
  const run = rows();
  expect(run.length).toBe(4);
  const fills = run.map((row) => luma(rest, row));
  // Neighbours differ, alternate rows match, and the striped ones are lighter.
  for (let i = 1; i < fills.length; i++) {
    assert(Math.abs(fills[i] - fills[i - 1]) > 0.2, `rows ${i - 1} and ${i} are not striped apart: ${fills}`);
  }
  for (let i = 2; i < fills.length; i++) {
    assert(Math.abs(fills[i] - fills[i - 2]) < 0.2, `rows ${i - 2} and ${i} do not match: ${fills}`);
  }
  assert(fills[1] > fills[0] && fills[3] > fills[2], `the odd rows are not the striped ones: ${fills}`);
  const stripe = fills[1];

  // Hover over a striped row lifts it past the stripe.
  const centre = (row) => ({ x: row.bounds.x + row.bounds.width / 2, y: row.bounds.y + row.bounds.height / 2 });
  app.scroll(centre(run[1]), { dy: 0 });
  app.frames(20, { waitMs: 16 });
  const hovered = luma(app.screenshot({ node: page() }), run[1]);
  assert(hovered > stripe, `hover does not read over the stripe: ${hovered} vs ${stripe}`);

  // Both selection strengths read over it: the keyboard's row in the scores
  // column, a striped one, and the selected song in the column it left.
  app.scroll(foot, { dy: 0 });
  app.key("right");
  app.key("down");
  app.frames(6, { waitMs: 16 });
  const keyed = app.screenshot({ node: page() });
  const focused = luma(keyed, rows()[1]);
  const path = luma(keyed, app.snapshot().find({ role: "row", label: "Aurora" }));
  assert(focused > stripe && path > stripe,
    `a selection does not read over the stripe: ${JSON.stringify({ focused, path, stripe })}`);
  assert(focused > path, `the keyboard's column is not the stronger: ${focused} vs ${path}`);
});
