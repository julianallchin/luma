// The sidebar's two levels and the push between them, under a real renderer.
//
// - the track list is the resting level, and a row is a door deeper;
// - the push is a *push*: for real frames in the middle of it both levels are
//   on screen at once and the column is neither;
// - the arriving level lists every score on the (track, venue);
// - the pop puts the list back where it was, and the column never resizes.

// Two further scores on the seeded pair, so the level has a list and `#2` is
// a row somebody can be sent to. Two clips on the seeded score, so its row
// tells it apart from the empty ones. Motion on and stretched: the push is
// 270ms, and a burst of shots only samples its middle at 10x.
fixture({
  seconds: 20,
  clips: [
    { pattern: "pat-glow-a", name: "Glow", start: 2, end: 5 },
    { pattern: "pat-glow-b", name: "Glow", start: 8, end: 11 },
  ],
  extra_scores: 2,
  motion: true,
  motion_scale: 10.0,
});

const sidebar = () => app.snapshot().find({ role: "card", label: "Sidebar" });
const scoreRows = () => app.snapshot().findAll({ role: "row" }).filter((n) => n.label.startsWith("#"));

// Wide, because the sampling is coarse against an eased travel. Anything
// strictly inside the column's width is part-way across; a settled frame
// reads 0 or the full width. The scores level is the ruler both ways: it
// arrives from +column-width on the push and leaves to it on the pop.
const NEAR = 20;
const FAR = 220;

// One burst across a level change, and the mid-flight frame picked out.
function burst(name) {
  const seen = [];
  let mid = null;
  let midBounds = null;
  for (let i = 0; i < 8; i += 1) {
    const s = app.snapshot();
    const node = s.find({ role: "card", label: "Sidebar" });
    const shot = app.screenshot({ node });
    image.keep(shot, `sidebar-push/${name}-${String(i).padStart(2, "0")}`);
    const x = s.find({ role: "button", label: "New score" })?.bounds.x;
    const both = s.find({ role: "input", label: "Search tracks" }) !== undefined;
    seen.push(`${both ? "both" : "one"}@${x === undefined ? "-" : Math.round(x)}`);
    if (mid === null && both && x >= NEAR && x <= FAR) {
      mid = shot;
      midBounds = node.bounds;
    }
    app.frames(1, { waitMs: 300 });
  }
  assert(mid !== null, `no frame of the ${name} caught both levels part-way across: ${seen}`);
  return { mid, midBounds };
}

test("the sidebar pushes to a track's scores and pops back", { timeoutMs: 180000 }, () => {
  nav.venue("Test Venue");
  // In and out once first, which puts the selection ring on the row before
  // the baseline shot.
  nav.track("Aurora");
  until("the sidebar to finish opening", (s) => s.find({ role: "card", label: "Sidebar" }).bounds.width >= 255.5, { timeoutMs: 15000 });
  app.frames(6, { waitMs: 16 });

  const tracksBounds = sidebar().bounds;
  const tracks = app.screenshot({ node: sidebar() });
  image.keep(tracks, "sidebar-scores/1-tracks");

  // Into the scores — not `nav.scores`, which waits for the push to land.
  app.click(app.snapshot().find({ role: "row", label: "Aurora" }));
  const push = burst("push");
  image.keep(push.mid, "sidebar-scores/2-midway");

  until("the settled scores level", (s) =>
    s.find({ role: "button", label: "New score" }) && s.find({ role: "input", label: "Search tracks" }) === undefined, { timeoutMs: 15000 });
  app.frames(4, { waitMs: 16 });
  const labels = scoreRows().map((n) => n.label);

  // Every score on the pair, once each, with the clips telling them apart.
  expect(labels.map((l) => l.split(" ")[0]).sort()).toEqual(["#1", "#2", "#3"]);
  expect(labels.filter((l) => l.includes("· 2 clips ·")).length).toBe(1);
  assert(labels.every((l) => l.includes("· You ·")), `a fixture has one principal: ${labels}`);

  app.click(scoreRows().find((n) => n.label.startsWith("#2 ")));
  until("the timeline on #2", (s) => s.findAll({ role: "text" }).some((n) => n.label === "Score #2"), { timeoutMs: 15000 });
  app.frames(6, { waitMs: 16 });
  const scoresBounds = sidebar().bounds;
  const scores = app.screenshot({ node: sidebar() });
  image.keep(scores, "sidebar-scores/3-scores");

  // The pop is the entrance reversed: the same burst, the same claim.
  app.click(app.snapshot().find({ role: "button", label: "Back to tracks" }));
  const pop = burst("pop");
  image.keep(pop.mid, "sidebar-scores/5-pop-midway");
  until("the track list again", (s) =>
    s.find({ role: "input", label: "Search tracks" }) && s.find({ role: "card", label: "Scores level" }) === undefined, { timeoutMs: 15000 });
  app.frames(6, { waitMs: 16 });
  const poppedBounds = sidebar().bounds;
  const popped = app.screenshot({ node: sidebar() });
  image.keep(popped, "sidebar-scores/4-popped");

  // The column never resizes: a level change is a change of subject.
  for (const other of [push.midBounds, scoresBounds, poppedBounds]) expect(other).toEqual(tracksBounds);

  // The two levels are different pictures (most of the column is empty
  // ground on both, so this is a floor, not a fraction of anything)…
  expect(image.diff(tracks, scores)).toBeGreaterThan(0.05);
  // …the mid-push frame is neither of them…
  expect(image.diff(push.mid, tracks)).toBeGreaterThan(0.02);
  expect(image.diff(push.mid, scores)).toBeGreaterThan(0.02);
  // …and the pop lands back on the track list.
  expect(image.diff(tracks, popped)).toBeLessThan(0.02);
});
