// Edit focus under the native renderer: a compact stage leaves the clip
// inspector usable, and both picker previews render.

fixture({
  seconds: 20,
  rig: 4,
  window: [1400, 900],
  clips: [{ pattern: "pat-glow", name: "Glow", start: 2, end: 5 }],
});

// A clip is labelled by its form; the fixture's clip plays Constant color.
const CLIP = "Constant color";
const node = (role, label) => app.snapshot().find({ role, label });

test("edit focus keeps the controls usable and renders both picker previews", () => {
  nav.trackEditor("Test Venue", "Aurora");
  nav.expand();
  until("the clip", () => node("card", CLIP));
  app.click(node("card", CLIP));
  until("the clip controls", () => node("button", "Pick fixtures"));
  app.frames(10, { waitMs: 50 });
  const normal = app.screenshot();

  // Taller stage, same inspector: it keeps usable height and stays above the timeline.
  app.drag(node("slider", "Stage height"), { dx: 0, dy: 160 });
  app.frames(5, { waitMs: 50 });
  const inspector = node("card", "Clip inputs").bounds;
  const timeline = node("card", "Waveform").bounds;
  expect(inspector.height).toBeGreaterThan(299);
  expect(inspector.y + inspector.height).toBeLessThan(timeline.y + 1);

  // The fixture picker's preview draws, and a checked group changes it.
  app.click(node("button", "Pick fixtures"));
  until("the selection preview", () => node("card", "Selection preview"));
  app.frames(8, { waitMs: 50 });
  const preview = () => node("card", "Selection preview");
  const selection = app.screenshot();
  expect(image.stats(selection, preview().bounds).max).toBeGreaterThan(0);
  app.click(node("checkbox", "left_movers"));
  app.frames(8, { waitMs: 50 });
  const highlighted = app.screenshot();
  expect(image.diff(selection, highlighted, { rect: preview().bounds })).toBeGreaterThan(0);
  app.key("escape");
  until("the picker closed", () => !node("card", "Fixture picker dialog"));

  app.click(node("button", "Add pattern"));
  until("the pattern search", () => node("input", "Search patterns…"));
  app.type(node("input", "Search patterns…"), "Wash");
  until("the pattern preview", () => node("card", "Pattern preview"));
  app.frames(8, { waitMs: 50 });
  const pattern = app.screenshot();
  expect(image.diff(normal, pattern)).toBeGreaterThan(0);
});
