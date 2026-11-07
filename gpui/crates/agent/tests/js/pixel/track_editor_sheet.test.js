// Edit focus under the native renderer: a taller stage leaves the clip's
// graph page usable, and both picker previews render.

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
  nav.agentOff();
  until("the clip", () => peek("card", CLIP));
  nav.clipGraph(CLIP);
  until("the clip controls", () => peek("button", "Pick fixtures"));
  app.frames(10, { waitMs: 50 });

  // A taller stage takes height from the graph page, down to a floor that
  // holds however far the seam is pulled. At the floor the page is still
  // under the stage, inside the window, and its controls are on it.
  const graph = () => peek("card", "Pattern graph").bounds;
  const before = graph();
  app.drag(peek("slider", "Stage height"), { dx: 0, dy: 160 });
  app.frames(5, { waitMs: 50 });
  const squeezed = graph();
  expect(squeezed.height).toBeLessThan(before.height);
  app.drag(peek("slider", "Stage height"), { dx: 0, dy: 160 });
  app.frames(5, { waitMs: 50 });
  const floor = graph();
  expect(Math.abs(floor.height - squeezed.height)).toBeLessThan(1);
  const seam = peek("slider", "Stage height").bounds;
  const tall = app.screenshot();
  expect(floor.y).toBeGreaterThan(seam.y - 1);
  expect(floor.y + floor.height).toBeLessThan(tall.height / tall.scale + 1);
  const pick = peek("button", "Pick fixtures").bounds;
  assert(pick.height > 0 && pick.y >= floor.y - 0.5 && pick.y + pick.height <= floor.y + floor.height + 0.5,
    "the clip controls fell off the squeezed page");

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

  // Back on the timeline, the pattern picker's preview draws.
  app.click(peek("button", "Back to timeline"));
  until("the timeline", () => peek("card", "Waveform"));
  app.frames(10, { waitMs: 50 });
  const normal = app.screenshot();
  app.click(peek("button", "Add pattern"));
  until("the pattern search", () => peek("input", "Search patterns…"));
  app.type(peek("input", "Search patterns…"), "Wash");
  until("the pattern preview", () => peek("card", "Pattern preview"));
  app.frames(8, { waitMs: 50 });
  const pattern = app.screenshot();
  expect(image.diff(normal, pattern, { rect: peek("card", "Pattern preview").bounds })).toBeGreaterThan(0);
});
