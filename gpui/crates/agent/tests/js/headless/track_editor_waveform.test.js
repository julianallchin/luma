// The overview minimap navigates the main timeline: its viewport pans,
// resizes, and clamps to the map's ends, without moving playback.

fixture({
  seconds: 180,
  clips: [{ pattern: "pattern-strobe", name: "Strobe", start: 11.5, end: 12.5 }],
  window: [1480, 828],
});

test("the minimap pans, resizes and clamps the viewport", () => {
  nav.trackEditor("Test Venue", "Aurora");
  nav.expand();
  nav.stageOff();
  const node = (label) => app.snapshot().find({ role: "slider", label });
  const settle = () => app.frames(12, { waitMs: 60 });
  const near = (a, b) => Math.abs(a - b) < 1;
  settle();
  const map = node("Timeline minimap").bounds;
  const before = node("Minimap viewport").bounds;

  app.drag(node("Minimap viewport"), { dx: map.width / 4, dy: 0 });
  settle();
  const panned = node("Minimap viewport").bounds;
  expect(panned.x).toBeGreaterThan(before.x + 100);
  assert(near(panned.width, before.width), `a pan changed the viewport's width: ${before.width} → ${panned.width}`);

  app.drag(node("Minimap end"), { dx: 50, dy: 0 });
  settle();
  const resized = node("Minimap viewport").bounds;
  expect(resized.width).toBeGreaterThan(panned.width + 30);
  assert(near(resized.x, panned.x), "dragging the end handle moved the start");

  // Past either end of the map: the viewport stops at the edge.
  let viewport = node("Minimap viewport").bounds;
  app.drag(node("Minimap viewport"), { dx: map.x - 50 - viewport.x - viewport.width / 2, dy: 0 });
  settle();
  assert(near(node("Minimap viewport").bounds.x, map.x), "the viewport ran past the map's start");
  viewport = node("Minimap viewport").bounds;
  const windowRight = 1479;
  app.drag(node("Minimap viewport"), { dx: windowRight - viewport.x - viewport.width / 2, dy: 0 });
  settle();
  const right = node("Minimap viewport").bounds;
  assert(near(right.x + right.width, map.x + map.width), `the viewport ran past the map's end: ${JSON.stringify({ right, map })}`);

  // Navigating the overview is not a fine-scrub gesture on the timeline.
  expect(app.snapshot().findAll({ role: "text" }).filter((n) => n.label.startsWith("FINE "))).toEqual([]);
});
