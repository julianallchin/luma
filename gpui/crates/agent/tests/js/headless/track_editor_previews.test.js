// A clip whose pattern renders a heatmap reports a preview surface.
//
// The clip body is inert to the pointer, so the only way a script can tell a
// heatmap-bearing body from the flat fallback fill is the "<clip> preview"
// node the canvas registers exactly when a decoded preview exists. The
// preview arrives on its own schedule — the seam evaluates the pattern over
// the clip's span after the timeline is up — so the test waits for it.

// A rigged venue: the heatmap's rows are the pattern's primitives, and an
// empty preview is indistinguishable from a slow one. The window is the one
// the pixel sibling uses; at the default size the toolbar wraps under
// headless text metrics.
fixture({
  seconds: 8,
  clips: [{ pattern: "pattern-pulse", name: "Pulse", start: 0.5, end: 4.5, preset: "Pulse" }],
  rig: 4,
  window: [1480, 1000],
});

// A clip is labelled by its name; the fixture's clip is a Pulse.
const CLIP = "Pulse";

test("a lit clip reports a preview surface under its header", () => {
  nav.trackEditor("Test Venue", "Aurora");
  until("the timeline", (s) => s.find({ role: "card", label: "Waveform" }) !== undefined);
  // The clip is up as soon as the clip list lands; its preview when the
  // seam's render does.
  until("the clip", (s) => s.find({ role: "card", label: CLIP }) !== undefined);
  const shot = until("the preview surface", (s) => s.find({ role: "card", label: `${CLIP} preview` }) !== undefined);
  const clip = shot.find({ role: "card", label: CLIP }).bounds;
  const preview = shot.find({ role: "card", label: `${CLIP} preview` }).bounds;

  // The preview is the clip's body: the same span, directly under the
  // header the pointer owns (the clip's node is that header).
  expect(preview.x).toBe(clip.x);
  expect(preview.width).toBe(clip.width);
  expect(preview.y).toBe(clip.y + clip.height);
  expect(preview.height).toBeGreaterThan(0);
});
