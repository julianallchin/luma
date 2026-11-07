// Resizing the shell: a seam dragged with motion on, and the window at its
// smallest.
//
// A seam is held by the pointer: one frame after a move, the seam is where
// the pointer is, whether or not the app animates its panels. A panel whose
// width eased toward the pointer trailed it, and a window at its minimum size
// must still seat the agent panel at its floor beside the content at its own.

const WINDOW = [1600, 950];
fixture({
  seconds: 20,
  rig: 4,
  clips: [{ pattern: "pattern-strobe", name: "Strobe", start: 2, end: 6 }],
  window: WINDOW,
});

const at = (s, role, label) => s.find({ role, label })?.bounds;
const centre = (b, axis) => (axis === "x" ? b.x + b.width / 2 : b.y + b.height / 2);

// Drag a grip by `delta` along `axis` and read where it ends up one frame
// after each step: it must be under the pointer, not on its way there.
function follows(label, axis, delta) {
  const grip = at(app.snapshot(), "slider", label);
  assert(grip, `no ${label} grip`);
  const from = centre(grip, axis);
  const steps = 6;
  for (let step = 1; step <= steps; step++) {
    const now = at(app.snapshot(), "slider", label);
    const offset = from + (delta * step) / steps - centre(now, axis);
    app.drag(now, axis === "x" ? { dx: offset, dy: 0 } : { dx: 0, dy: offset }, { steps: 2 });
    app.frames(1);
    const landed = centre(at(app.snapshot(), "slider", label), axis);
    const wanted = from + (delta * step) / steps;
    assert(Math.abs(landed - wanted) <= 1,
      `the ${label} seam trails the pointer at step ${step}: at ${landed}, pointer at ${wanted}`);
  }
}

test("each seam stays under the pointer while motion is on", { fixture: { motion: true } }, () => {
  nav.trackEditor("Test Venue", "Aurora");
  until("the timeline", (s) => at(s, "card", "Waveform") && at(s, "slider", "Agent width"));
  app.frames(30, { waitMs: 16 });
  follows("Agent width", "x", 180);
  follows("Agent width", "x", -120);
  follows("Stage height", "y", 90);

  app.action("luma::ShowLive");
  until("Live's deck seam", (s) => at(s, "slider", "Deck panel width"));
  app.frames(4);
  follows("Deck panel width", "x", 120);
});

test("a window at its smallest seats the agent panel at its floor", { fixture: { window: [800, 600] } }, () => {
  nav.trackEditor("Test Venue", "Aurora");
  const shot = until("the timeline", (s) => at(s, "card", "Waveform") && at(s, "card", "Agent") ? s : undefined);
  const agent = at(shot, "card", "Agent");
  const page = at(shot, "card", "Page");
  assert(agent.width >= 300 - 1, `the agent panel is under its 300px floor: ${agent.width}`);
  assert(page.x + page.width - (agent.x + agent.width) >= 480,
    `the content is under its 480px floor: ${JSON.stringify({ agent, page })}`);
});
