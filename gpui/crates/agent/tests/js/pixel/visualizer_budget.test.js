// What a frame of the stage costs the UI thread, against the same run's own
// frames. `app.timings()` is the CPU half of a frame (see `api.d.ts`); an
// absolute number here differs by an order of magnitude between debug and
// release, so every gate is a ratio within one run: a steady gesture has no
// frame an order of magnitude off its neighbours.

// A venue with a rig and no score: the unlit stage over the patch tab.
fixture({ seconds: 20, rig: 4 });

const sorted = (values) => [...values].sort((a, b) => a - b);
const at = (draw, q) => draw[Math.min(draw.length - 1, Math.floor(draw.length * q))];
const drawSince = (from) => sorted(app.timings().frames.filter((f) => f.frame >= from).map((f) => f.drawMs));

function openUnlit() {
  nav.patch("Test Venue");
  app.frames(4, { waitMs: 60 });
  nav.expand();
  app.frames(8, { waitMs: 60 });
  const stage = app.snapshot().find({ role: "card", label: "Stage" });
  assert(stage, "the viewport is not on screen");
  return stage;
}

// A venue-sized rig and twelve overlapping lit clips, playing. Every cost this
// is about — cluster occupancy, shadow passes, the score's per-clip work —
// scales on these numbers, and four movers measure none of them.
const RIG = 120;
// Clips play Rainbow, labelled by its form: saturated light that changes
// every frame, so every fixture's colour is dirty on every frame.
const CLIP = "Color";
const busy = (seconds, clips, window) => ({
  seconds,
  rig: RIG,
  ...(window ? { window } : {}),
  clips: Array.from({ length: clips }, (_, lane) => ({
    pattern: `pattern-pulse-${lane}`,
    name: clips === 1 ? "Pulse" : `Pulse ${lane}`,
    start: 0,
    end: seconds,
    lane,
    preset: ["color@1", "Rainbow"],
  })),
});

function openPlaying(clip) {
  nav.trackEditor("Test Venue", "Aurora");
  until("the clip", (s) => s.find({ role: "card", label: clip }) !== undefined);
  nav.expand();
  app.frames(10, { waitMs: 60 });
  // Proof the measurement is of the rig it claims: a fallback to a handful of
  // movers, or an unlit scene, would make every number here meaningless.
  expect(library.query("select count(*) as n from fixtures")[0].n).toBe(RIG);
  const slider = app.snapshot().find({ role: "slider", label: "House lights" });
  app.drag(slider, { dx: 0, dy: 140 }, { steps: 12, restale: "match" });
  until("the house lights down", (s) => s.find({ role: "text", label: "House lights = 0%" }) !== undefined);
  app.frames(10, { waitMs: 60 });
  // With the house lights down, anything bright in the middle of the stage
  // (clear of the corner controls) can only be lit fixtures.
  const stage = app.snapshot().find({ role: "card", label: "Stage" }).bounds;
  const middle = { x: stage.width * 0.2, y: stage.height * 0.2, width: stage.width * 0.6, height: stage.height * 0.6 };
  const shot = app.screenshot({ node: app.snapshot().find({ role: "card", label: "Stage" }) });
  expect(image.stats(shot, middle).max).toBeGreaterThan(20);
  nav.step("the Play button", "button", "Play");
  // Past the first frames, which carry the score's cold caches.
  app.frames(20, { waitMs: 55 });
  return app.snapshot().find({ role: "card", label: "Stage" });
}

test("an orbit has no frame far past its neighbours", () => {
  const stage = openUnlit();
  const from = app.snapshot().frame;
  // Strokes, with the camera left where each ended: still one orbit.
  for (let stroke = 0; stroke < 6; stroke += 1) {
    app.drag(stage, { dx: 40, dy: 0 }, { steps: 20, restale: "match" });
  }
  const draw = drawSince(from);
  expect(draw.length).toBeGreaterThan(59);
  expect(draw[draw.length - 1]).toBeLessThan(at(draw, 0.5) * 10);
});

// Paced so a new renderer frame lands for nearly every settle: an unpaced loop
// measures repaints of an already published frame, which are free.
test("publishing stage frames has no frame far past its neighbours", () => {
  openUnlit();
  const from = app.snapshot().frame;
  app.frames(120, { waitMs: 20 });
  const draw = drawSince(from);
  expect(draw.length).toBeGreaterThan(59);
  expect(draw[draw.length - 1]).toBeLessThan(at(draw, 0.5) * 20);
});

// Stall versus slowdown: a renderer that cannot keep up gives a low, even
// frame rate; a UI-thread stall gives a median inside the budget and a max
// many times it. Only the second reads as a freeze.
test("playback has no frame far past its neighbours", { fixture: busy(30, 12) }, () => {
  openPlaying(CLIP);
  const from = app.snapshot().frame;
  app.frames(200, { waitMs: 16 });
  const draw = drawSince(from);
  expect(draw.length).toBeGreaterThan(99);
  expect(draw[draw.length - 1]).toBeLessThan(at(draw, 0.5) * 10);
});

// The reported freeze was "while zooming in". A wheel sends a stream of
// dollies, each a notify and a frame rebuild; this compares that against the
// same stage playing untouched, full-screen sized.
test("zooming costs no cliff over holding still", { fixture: busy(60, 1, [2560, 1440]) }, () => {
  const stage = openPlaying(CLIP);
  const idleFrom = app.snapshot().frame;
  app.frames(120, { waitMs: 8 });
  const idle = drawSince(idleFrom);

  // Many small steps, the way a wheel does it. The camera clamps at its near
  // margin, so repeated inward scrolls settle against the stop.
  const zoomFrom = app.snapshot().frame;
  for (let burst = 0; burst < 6; burst += 1) {
    app.scroll(stage, { dy: 40, steps: 20, restale: "match" });
  }
  const zoom = drawSince(zoomFrom);
  expect(zoom.length).toBeGreaterThan(59);
  // A moved camera does more work, so not equal — but no cliff.
  expect(at(zoom, 0.5)).toBeLessThan(at(idle, 0.5) * 3);
  expect(zoom[zoom.length - 1]).toBeLessThan(at(idle, 0.5) * 10);
});
