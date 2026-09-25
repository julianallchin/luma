// The shell's panel slides, shot frame by frame. Motion is judged by looking
// at it, so the frames are kept (`luma-shots/shell-motion/`); what is asserted
// is that each slide is a slide — its frames are not all one picture, which a
// snapped toggle would give.
//
// Stretched 10x so a burst samples the curve rather than its two endpoints.

fixture({
  clips: [{ pattern: "pattern-strobe", name: "Strobe", start: 1, end: 4 }],
  window: [1280, 800],
  motion: true,
  motion_scale: 10,
});

const SAMPLES = 5;
const GAP_MS = 400;

test("the sidebar and workspace slide rather than jump", { timeoutMs: 180000 }, () => {
  nav.trackEditor("Test Venue", "Aurora");
  app.frames(12, { waitMs: 60 });
  for (const [name, action] of [
    ["sidebar-close", "luma::ToggleSidebar"],
    ["sidebar-open", "luma::ToggleSidebar"],
    ["workspace-close", "luma::ToggleWorkspace"],
    ["workspace-open", "luma::ToggleWorkspace"],
  ]) {
    app.action(action);
    const shots = [];
    for (let i = 0; i < SAMPLES; i += 1) {
      app.frames(1, { waitMs: GAP_MS });
      shots.push(app.screenshot());
      image.keep(shots.at(-1), `shell-motion/${name}-${i}`);
    }
    // Three distinct pictures at least: start, somewhere between, end.
    const distinct = shots.filter((shot, i) => i === 0 || image.diff(shots[i - 1], shot) > 0.001).length;
    assert(distinct >= 3, `${name} showed only ${distinct} distinct frames`);
  }
});
