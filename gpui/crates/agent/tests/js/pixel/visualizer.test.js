// The 3D stage view draws, redraws when the camera moves, and rests when
// nothing does.
//
// Pixel-only twice over: headless has no renderer, and the thing under test is
// a picture. A viewport that painted nothing, or the same thing whatever the
// camera did, would pass every node-tree assertion and still be broken.
//
// Every acting call carries `restale: "match"`: the viewport asks for an
// animation frame at the top of every render, so a frame is always one behind
// by the time a script acts on it. That is the redraw working, not a stale
// click.

// A venue with a rig and no score: the patch tab raises the unlit stage.
fixture({ seconds: 20, rig: 4 });

function openViewport() {
  nav.patch("Test Venue");
  app.frames(4, { waitMs: 60 });
  nav.expand();
  app.frames(8, { waitMs: 60 });
}

const text = (prefix) => app.snapshot().findAll({ role: "text" }).find((n) => n.label.startsWith(prefix));
const idle = () => app.snapshot().find((n) => n.role === "text" && n.label === "FPS IDLE") !== undefined;

// All three matter and none implies the others. A viewport wired to a stale
// image passes "non-black" forever; one that renders a fresh frame with the
// camera ignored passes "not blank" and fails the orbit.
//
// The haze is marched and accumulated temporally, so two shots of the same
// untouched stage already differ over a large share of the window. Every frame
// diff is therefore judged against `churn`, measured here over the same
// settling span, not assumed. Mean luma is the stable half: the churn wobbles
// it by a fraction of a unit while the house lights halve it.
test("orbiting and the house lights change what is drawn", () => {
  openViewport();
  const settle = () => app.frames(24, { waitMs: 30 });
  const luma = (shot) => image.stats(shot).meanLuma;

  const before = app.screenshot();
  expect(luma(before)).toBeGreaterThan(1);
  settle();
  const churn = image.diff(before, app.screenshot());

  const slider = () => {
    const node = app.snapshot().find({ role: "slider", label: "House lights" });
    assert(node, "the stage has no house-light slider");
    return node;
  };
  const level = () => text("House lights = ")?.label ?? null;

  // The fraction is clamped, so a drag past either end of the slider is "all
  // the way". The restoring drag carries the pointer off the widget, so the
  // knob's hover tooltip is not a bubble in the restored frame.
  app.drag(slider(), { dx: 0, dy: 54 }, { steps: 12, restale: "match" });
  until("the house lights down", () => level() === "House lights = 0%");
  settle();
  const dark = app.screenshot();
  expect(luma(dark)).toBeLessThan(luma(before));
  const darkened = image.diff(before, dark);
  expect(darkened).toBeGreaterThan(0.005);
  expect(darkened).toBeGreaterThan(churn);

  app.drag(slider(), { dx: 0, dy: -140 }, { steps: 12, restale: "match" });
  until("the house lights back up", () => level() === "House lights = 100%");
  settle();
  const restored = app.screenshot();
  const lost = luma(before) - luma(dark);
  // Restoring brings back at least 90% of the energy the dark frame lost.
  expect(Math.abs(luma(restored) - luma(before))).toBeLessThan(0.1 * lost);
  expect(image.diff(before, restored)).toBeLessThan(darkened);

  // Left-drag on the viewport is orbit. The camera reading is the direct
  // evidence; the frame diff says the picture followed it, above the churn.
  const camera = () => {
    const node = text("CAMERA ");
    assert(node, "the stage publishes no camera");
    return node.label;
  };
  const aimed = camera();
  const stage = app.snapshot().find({ role: "card", label: "Stage" });
  assert(stage, "the viewport is not on screen");
  app.drag(stage, { dx: 220, dy: 60 }, { steps: 20, restale: "match" });
  app.frames(4, { waitMs: 16 });
  assert(camera() !== aimed, "the orbit drag did not turn the camera");
  const moved = image.diff(before, app.screenshot());
  expect(moved).toBeGreaterThan(0.02);
  expect(moved).toBeGreaterThan(churn);
});

// The reading is the label, so this matches its shape: `^CPU` and not the full
// split, because "CPU/GPU timing unavailable" is the honest reading on a frame
// the renderer has not timed yet.
//
// The fullscreen button is painted over the middle of the folded stats card,
// so a click on the card's centre toggles fullscreen instead. The press aims
// at the card's top-left corner, and when it still lands on the button the
// script undoes that and aims again.
test("the frame-stats panel folds, unfolds and publishes CPU and GPU timing", () => {
  openViewport();
  const stats = () => {
    const node = app.snapshot().find({ role: "toggle", label: "Frame stats" });
    assert(node, "the stage has no frame-stats panel");
    return node;
  };
  const unfolded = () => app.snapshot().find((n) => n.role === "text" && /^CPU/.test(n.label)) !== undefined;
  const full = () => app.snapshot().find({ role: "button", label: "Exit fullscreen" }) !== undefined;
  const wasFull = full();
  const press = () => {
    const bounds = stats().bounds;
    app.drag({ x: bounds.x + 3, y: bounds.y + 3 }, { dx: 0, dy: 0 }, { steps: 2, restale: "match" });
    app.frames(4, { waitMs: 30 });
    if (full() === wasFull) return true;
    const label = wasFull ? "Fullscreen visualizer" : "Exit fullscreen";
    app.click(app.snapshot().find({ role: "button", label }), { restale: "match" });
    app.frames(4, { waitMs: 30 });
    return false;
  };
  const fold = (want) => {
    for (let i = 0; i < 10; i++) {
      if (press() && unfolded() === want) return;
    }
    throw new Error(`the frame-stats card would not ${want ? "unfold" : "fold"}`);
  };
  fold(true);
  fold(false);
});

// A still stage stops submitting frames once the temporal haze has settled —
// the FPS readout says `IDLE` — and a camera drag wakes it. Without the gate a
// still stage re-marches the haze at display rate for nobody. Drifting haze
// (wind or turbulence) is motion and keeps the stage awake on purpose, so the
// room's haze is switched off first.
test("a still stage rests and a camera drag wakes it", () => {
  openViewport();
  nav.step("the haze switch", "toggle", "Haze", { restale: "match" });
  until("the stage to rest", () => idle());
  const stage = app.snapshot().find({ role: "card", label: "Stage" });
  assert(stage, "the viewport is not on screen");
  app.drag(stage, { dx: 80, dy: 0 }, { steps: 5, restale: "match" });
  app.frames(3, { waitMs: 16 });
  assert(!idle(), "a camera drag did not wake the resting stage");
});
