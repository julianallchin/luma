// The heatmap in a clip's body is a picture of the pattern, not a tint.
//
// The measured clip plays Rainbow, which walks every fixture through the hue
// wheel every four beats, so the evidence that the body paints the heatmap —
// and not the flat fallback fill — is a row that is saturated and holds many
// colours, where the flat fill is one colour broken only by beat lines.

// Three lit clips over two lanes. Taller than the default window so the
// bottom-anchored lane stack clears the window edge.
fixture({
  seconds: 8,
  rig: 4,
  window: [1480, 1000],
  clips: [
    { pattern: "pattern-rainbow", name: "Rainbow", start: 0.5, end: 4.5, preset: ["color@1", "Rainbow"] },
    { pattern: "pattern-wash", name: "Wash", start: 5.0, end: 7.5, preset: ["color.noise@1", "Drift"] },
    { pattern: "pattern-chase", name: "Chase", start: 1.5, end: 6.0, lane: 1, preset: ["color.sparkle@1", "Random heads"] },
  ],
});
// Clips are labelled by their form.
const MEASURED = "Color";

test("the clip body paints the heatmap rather than a flat fill", () => {
  nav.trackEditor("Test Venue", "Aurora");
  nav.expand();
  nav.stageOff();
  until("the timeline", (s) => s.find({ role: "card", label: "Waveform" }) !== undefined);
  for (const label of ["Color preview", "Noise preview", "Sparkle preview"]) {
    until(label, (s) => s.find({ role: "card", label }) !== undefined);
  }
  app.frames(8, { waitMs: 30 });
  // One clip selected, so the shot holds an opaque selected body beside the
  // translucent rest.
  app.click(app.snapshot().find({ role: "card", label: "Sparkle" }));
  app.frames(8, { waitMs: 30 });
  const shot = app.screenshot({ node: app.snapshot().find({ role: "card", label: `${MEASURED} preview` }) });

  // The centre row, one logical pixel at a time: the edges carry the clip
  // border and the header hairline, the middle is heatmap over the lane bed.
  const width = Math.floor(shot.width / shot.scale);
  const y = Math.floor(shot.height / shot.scale / 2);
  let saturated = 0;
  const levels = new Set();
  for (let x = 0; x < width; x++) {
    const [r, g, b] = image.stats(shot, { x, y, width: 1, height: 1 }).mean;
    levels.add(`${Math.round(r)},${Math.round(g)},${Math.round(b)}`);
    if (Math.max(r, g, b) - Math.min(r, g, b) > 16) saturated += 1;
  }
  // The lane bed is grey; the heatmap is hue wherever the rainbow is lit.
  expect(saturated).toBeGreaterThan(width * 0.2);
  // The rainbow passes through many colours per cycle; the flat fill is a handful.
  expect(levels.size).toBeGreaterThan(12);
});
