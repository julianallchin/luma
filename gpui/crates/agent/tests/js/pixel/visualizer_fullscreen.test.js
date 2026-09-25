// A live viewport keeps its camera across fullscreen and stays interactive.
// The headless `visualizer_fullscreen.test.js` covers the layout; this covers
// the live stage renderer, which headless does not have.

fixture({
  seconds: 20,
  rig: 4,
  window: [1400, 900],
  clips: [{ pattern: "pat-glow", name: "Glow", start: 2, end: 15 }],
});

// A clip is labelled by its form; the fixture's clip plays Constant color.
const CLIP = "Constant color";
const node = (role, label) => app.snapshot().find({ role, label });
const camera = () => app.snapshot().findAll({ role: "text" }).find((n) => n.label.startsWith("CAMERA "))?.label;

test("fullscreen keeps the live camera and restores its pose", () => {
  nav.trackEditor("Test Venue", "Aurora");
  until("the clip", () => node("card", CLIP));
  app.click(node("card", CLIP));
  until("the live stage", () => camera());
  app.frames(8, { waitMs: 60 });
  const before = camera();

  app.click(node("button", "Fullscreen visualizer"));
  until("expanded", () => !node("card", "Waveform"));
  app.frames(8, { waitMs: 60 });
  expect(camera()).toBe(before);
  // The fullscreen stage fills the window it is shot from.
  const full = app.screenshot();
  expect(full.width).toBe(1400 * full.scale);
  expect(full.height).toBe(900 * full.scale);

  app.click(node("button", "Exit fullscreen"));
  until("returned", () => node("card", "Waveform"));
  app.frames(8, { waitMs: 60 });
  expect(camera()).toBe(before);

  // Pointer focus makes the visualizer's keyboard shortcuts available.
  app.click(node("card", "Stage"));
  app.key("space");
  until("play after a stage click", () => node("button", "Pause"));
  app.key("space");
  until("pause after a stage click", () => node("button", "Play"));
  app.key("shift-f");
  until("shortcut entry", () => node("button", "Exit fullscreen"));
  app.drag({ x: 700, y: 450 }, { dx: 90, dy: 40 }, { steps: 8, restale: "match" });
  app.frames(6, { waitMs: 60 });
  const orbited = camera();
  assert(orbited !== before, "fullscreen could not orbit");
  app.key("escape");
  until("shortcut exit", () => node("card", "Waveform"));
  app.frames(4, { waitMs: 60 });
  expect(camera()).toBe(orbited);

  // In a venue tab, fullscreen hides the authoring shortcuts and exit restores them.
  app.action("luma::NewTab");
  nav.step("venue tab", "button", "Venue");
  until("venue controls", () => node("toggle", "Stage objects"));
  app.click(node("card", "Stage"));
  app.key("shift-f");
  until("venue fullscreen", () => node("button", "Exit fullscreen"));
  app.key("a");
  expect(node("input", "Search elements")).toBe(undefined);
  app.key("escape");
  until("venue restored", () => node("toggle", "Stage objects"));
  app.key("a");
  until("authoring shortcut restored", () => node("input", "Search elements"));
});
