// What ⌘B costs while the stage and an editor are live.
//
// A sidebar toggle is a layout change, so it has two costs: the UI thread's
// (the panel re-laid-out at a new width every frame of the slide — `drawMs`)
// and the renderer's (the frame-stats panel's `Draw` span; `drawMs` never
// sees the GPU). A toggle does strictly more work than an idle frame, so the
// claim is not that they are equal but that the slide does not fall off a
// cliff. Every bound is a ratio against this run's own idle stretch: the two
// stretches are seconds apart on one machine, so load moves both together.
//
// Motion is on: snapped, ⌘B would be one jump and measure a settled resize,
// which is not the complaint.

fixture({
  seconds: 60,
  clips: [{ pattern: "pattern-pulse", name: "Pulse", start: 0, end: 60 }],
  rig: 120,
  motion: true,
  window: [2560, 1440],
});

const median = (values) => {
  const sorted = values.filter((v) => v !== null).sort((a, b) => a - b);
  return { n: sorted.length, median: sorted[Math.floor(sorted.length / 2)], max: sorted.at(-1) };
};
const drawMs = (from) => median(app.timings().frames.filter((f) => f.frame >= from).map((f) => f.drawMs));
// The renderer's own submit-to-completion span, off the frame-stats panel.
const rendererDraw = () => {
  const node = app.snapshot().find((n) => n.role === "text" && n.label.startsWith("Draw "));
  const ms = node && Number(node.label.split(" ")[1]);
  return Number.isFinite(ms) ? ms : null;
};
const stage = () => app.snapshot().find({ role: "card", label: "Stage" });

// The stage's Fullscreen button (x 2512–2544, y 54–86 at this size) sits over
// the Frame stats toggle (x 2499–2550, y 50–78): a click on the toggle's
// centre opens fullscreen, and ⌘B then moves nothing.
test.skip("bug: the Fullscreen button covers the Frame stats toggle", { timeoutMs: 300000 }, () => {
  nav.trackEditor("Test Venue", "Aurora");
  // A clip is labelled by its form; the fixture's clip plays Constant color.
  until("the clip", (s) => s.find({ role: "card", label: "Constant color" }) !== undefined, { timeoutMs: 15000 });
  nav.expand();
  app.frames(10, { waitMs: 60 });
  nav.step("the frame-stats panel", "toggle", "Frame stats");
  nav.step("the Play button", "button", "Play");
  app.frames(20, { waitMs: 55 });
  assert(stage() !== undefined, "the viewport is not on screen");

  // Playing, nobody touching the shell.
  const idleFrom = app.snapshot().frame;
  const idleDraw = [];
  for (let step = 0; step < 60; step += 1) {
    app.frames(2, { waitMs: 8 });
    idleDraw.push(rendererDraw());
  }
  const idle = drawMs(idleFrom);

  // ⌘B there and back, three times, sampled through each slide. The stage's
  // width has to move mid-slide: a ⌘B that reached no handler would leave
  // every number equal to the idle stretch and say nothing.
  const toggleFrom = app.snapshot().frame;
  const widths = [];
  const slideDraw = [];
  const slide = () => {
    app.key("cmd-b");
    for (let step = 0; step < 20; step += 1) {
      app.frames(2, { waitMs: 8 });
      widths.push(Math.round(stage().bounds.width));
      // Only while the slide still moves: settled frames would dilute it.
      if (widths.length < 2 || widths.at(-1) !== widths.at(-2)) slideDraw.push(rendererDraw());
    }
  };
  for (let round = 0; round < 3; round += 1) {
    slide();
    slide();
  }
  const toggle = drawMs(toggleFrom);
  const renderer = { idle: median(idleDraw), slide: median(slideDraw) };
  console.log(JSON.stringify({ idle, toggle, renderer, widths: [...new Set(widths)].length }));

  expect(toggle.n).toBeGreaterThan(119);
  assert(new Set(widths).size >= 4, "the stage never slid, so ⌘B was snapped rather than animated");
  expect(toggle.median).toBeLessThan(idle.median * 3);
  expect(toggle.max).toBeLessThan(idle.median * 12);

  // The renderer gate: every frame of a slide used to hand the renderer a
  // width it had never seen, reallocating its targets and dropping the haze
  // history (16.2ms median against 6.5ms still, at this size and rig).
  expect(renderer.slide.n).toBeGreaterThan(7);
  expect(renderer.idle.median).toBeGreaterThan(0);
  expect(renderer.slide.median).toBeLessThan(renderer.idle.median * 1.5);
});

// Holding the render size means the stage draws at a size the layout has
// left behind, and asks for frames while a hold is outstanding. That request
// could keep a stage awake forever; `FPS IDLE` is the idle gate saying it did
// not. (The countdown's arithmetic is `RenderSize`'s unit tests.) Drifting
// haze is motion and keeps a stage awake on purpose, so the room's haze is
// switched off first — which is why this runs on the venue page, where the
// switch is.
test("a paused stage rests again after the sidebar slides", { fixture: { rig: 20, clips: [], window: null }, timeoutMs: 120000 }, () => {
  nav.patch("Test Venue");
  nav.expand();
  app.frames(10, { waitMs: 60 });
  nav.step("the haze switch", "toggle", "Haze", { restale: "match" });
  const idle = () => app.snapshot().find((n) => n.role === "text" && n.label === "FPS IDLE") !== undefined;
  const settle = () => {
    for (let i = 0; i < 180; i += 1) {
      if (idle()) return true;
      app.frames(1, { waitMs: 16 });
    }
    return false;
  };
  assert(settle(), "the stage never rested to begin with");
  app.key("cmd-b");
  // Past the slide, so only the stage's own request can carry the hold on.
  app.frames(30, { waitMs: 16 });
  assert(settle(), "the stage never rested after the slide, so the held size never ended");
});
