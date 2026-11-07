// Edit focus under the native renderer: a compact stage leaves the clip
// inspector usable, and both picker previews render.

fixture({
  seconds: 20,
  rig: 4,
  window: [1400, 900],
  clips: [{ pattern: "pat-glow", name: "Glow", start: 2, end: 5 }],
});

// A clip is labelled by its name; the fixture's clip is a Wash.
const CLIP = "Wash";

test("edit focus keeps the controls usable and renders both picker previews", () => {
  nav.trackEditor("Test Venue", "Aurora");
  nav.program();
  until("the clip", () => peek("card", CLIP));
  app.click(peek("card", CLIP));
  until("the clip controls", () => peek("button", "Pick fixtures"));
  app.frames(10, { waitMs: 50 });
  const normal = app.screenshot();

  // Taller stage, same inspector: it keeps usable height and stays above the timeline.
  app.drag(peek("slider", "Stage height"), { dx: 0, dy: 160 });
  app.frames(5, { waitMs: 50 });
  const inspector = peek("card", "Pattern graph").bounds;
  const timeline = peek("card", "Waveform").bounds;
  expect(inspector.height).toBeGreaterThan(299);
  expect(inspector.y + inspector.height).toBeLessThan(timeline.y + 1);

  // The fixture picker's preview draws, and a checked group changes it.
  app.click(peek("button", "Pick fixtures"));
  until("the selection preview", () => peek("card", "Selection preview"));
  app.frames(8, { waitMs: 50 });
  const preview = () => peek("card", "Selection preview");
  const selection = app.screenshot();
  expect(image.stats(selection, preview().bounds).max).toBeGreaterThan(0);
  app.click(peek("checkbox", "left_movers"));
  app.frames(8, { waitMs: 50 });
  const highlighted = app.screenshot();
  expect(image.diff(selection, highlighted, { rect: preview().bounds })).toBeGreaterThan(0);
  app.key("escape");
  until("the picker closed", () => !peek("card", "Fixture picker dialog"));

  app.click(peek("button", "Add pattern"));
  until("the pattern search", () => peek("input", "Search patterns…"));
  app.type(peek("input", "Search patterns…"), "Wash");
  until("the pattern preview", () => peek("card", "Pattern preview"));
  app.frames(8, { waitMs: 50 });
  const pattern = app.screenshot();
  expect(image.diff(normal, pattern)).toBeGreaterThan(0);
});
