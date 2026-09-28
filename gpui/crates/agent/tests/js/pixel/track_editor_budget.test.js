// What a width tween (⌘B) costs the timeline, against the same run holding
// still.
//
// Scrolling and scrubbing move the view over a canvas of fixed width. ⌘B
// changes the canvas width a pixel at a time for the length of the slide, and
// the paths keyed on width are the ones that go quadratic there: a heatmap
// resample rebuilt per frame, a fine-waveform round trip re-asked per frame.
//
// The stage is off, so `drawMs` is the timeline plus the shell's chrome. The
// clips are lit because an unlit clip has no heatmap to resample. Stated as a
// ratio against this run's own still stretch: only the ratio survives a
// loaded host. (The absolute scroll/scrub budget stays in
// `app_pixel/track_editor_budget.rs`, ignored.)

const CLIP_SECONDS = 12;
fixture({
  seconds: 300,
  rig: 24,
  // The subject is the slide, so it has to slide: motion is snapped by
  // default. Stretched, because at 1x the sampling walk steps over a 270ms
  // sweep and reads two settled widths.
  motion: true,
  motion_scale: 4,
  window: [2560, 1440],
  clips: [0, 1, 2].flatMap((lane) =>
    Array.from({ length: 15 }, (_, index) => {
      const start = index * 18 + lane * 2;
      return { pattern: `pattern-${lane}-${index}`, name: `Clip ${lane}-${index}`, start, end: start + CLIP_SECONDS, lane };
    }),
  ),
});

const canvas = () => app.snapshot().find({ role: "card", label: "Waveform" });
const summarise = (values) => {
  const sorted = [...values].sort((a, b) => a - b);
  const at = (q) => sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * q))];
  return { n: sorted.length, median: at(0.5), p95: at(0.95) };
};

// On demand, as the Rust test was (`#[ignore]`): the gates were set from a
// release build. A debug pixel build on a loaded Linux host (load 35) reads
// the median ratio at 1.85-2.2x, so run it by changing `test.skip` to `test`
// on a quiet machine before judging the canvas.
test.skip("a sidebar slide costs the timeline no cliff over holding still", () => {
  nav.trackEditor("Test Venue", "Aurora");
  until("the timeline", (s) => s.find({ role: "card", label: "Waveform" }) !== undefined);
  nav.expand();
  nav.stageOff();
  until("a decoded heatmap", (s) => s.findAll({ role: "card" }).some((n) => n.label.endsWith(" preview")));

  // Zoomed in, where a resample is expensive: a clip wider than the canvas is
  // cut by both edges, so its stretched image is a canvas-width picture. The
  // extra turns put a clip's body across both edges, not one.
  const widest = () =>
    Math.max(0, ...app.snapshot().findAll({ role: "card" }).filter((n) => n.label === "Color").map((n) => n.bounds.width));
  const zoomIn = () => app.scroll(canvas(), { dy: 120, steps: 5, modifiers: ["secondary"] });
  for (let i = 0; i < 40 && widest() < canvas().bounds.width * 0.99; i += 1) zoomIn();
  for (let i = 0; i < 8; i += 1) zoomIn();
  app.frames(10, { waitMs: 40 });
  assert(
    app.snapshot().findAll({ role: "card" }).some((n) => n.label.endsWith(" preview")),
    "no clip on screen has a heatmap, so this would measure the flat fill",
  );
  expect(widest()).toBeGreaterThan(canvas().bounds.width * 0.99 - 1);

  // One frame, no wait: the frames it drew, and the canvas width after them.
  const step = () => {
    const from = app.snapshot().frame;
    app.frames(1, { waitMs: 0 });
    return {
      width: Math.round(canvas().bounds.width),
      draw: app.timings().frames.filter((f) => f.frame > from).map((f) => f.drawMs),
    };
  };
  const still = [];
  for (let i = 0; i < 60; i += 1) still.push(...step().draw);

  // Keep only frames on which the canvas actually changed width.
  const widths = new Set();
  const slide = [];
  for (let round = 0; round < 6; round += 1) {
    app.action("luma::ToggleSidebar");
    let previous = null;
    for (let i = 0; i < 40; i += 1) {
      const sample = step();
      widths.add(sample.width);
      if (previous !== null && sample.width !== previous) slide.push(...sample.draw);
      previous = sample.width;
    }
  }
  assert(widths.size >= 4, `the canvas never slid, so the sidebar toggle was snapped: widths ${[...widths]}`);
  const calm = summarise(still);
  const moving = summarise(slide);
  expect(moving.n).toBeGreaterThan(7);
  expect(calm.median).toBeGreaterThan(0);
  // The median is the per-frame resample every slide frame used to pay; the
  // p95 is the rebuild a chunk boundary still forces.
  expect(moving.median / calm.median).toBeLessThan(1.25);
  expect(moving.p95 / calm.p95).toBeLessThan(5);
});
