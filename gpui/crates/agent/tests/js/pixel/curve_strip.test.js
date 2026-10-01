// The curve strip under the renderer: a Bézier that overshoots its two
// values draws past the box's edge, in the air around it, and a handle above
// the box shows there too.

// A rise that overshoots 1, a jump down at 0.6, and a flat tail.
const SHAPE = [[0, 0, [0.3, 1.5, 0.6, 1.2]], [0.6, 1], [0.6, 0.2], [1, 0.2]];
const graph = {
  version: 3,
  nodes: {
    t: { kind: "time", inputs: { every: 2 } },
    curve1: { kind: "curve", settings: { kind: "number" }, inputs: { x: { node: "t" }, shape: { points: SHAPE } } },
    color1: { kind: "color", inputs: { brightness: { node: "curve1" } } },
  },
};
fixture({ seconds: 20, clips: [{ pattern: "graph-clip", name: "Pulse", start: 1, end: 3, preset: "Pulse", graph }], rig: 4, window: [1400, 1000] });

const node = (role, label) => until(label, (s) => s.find({ role, label })).find({ role, label });
const settle = () => app.frames(6, { waitMs: 30 });

function open() {
  nav.trackEditor("Test Venue", "Aurora");
  nav.expand();
  nav.stageOff();
  app.click(node("card", "Pulse"));
  nav.widenGraph();
  app.click(nav.inGraph((s) => s.find({ role: "button", label: "Expand Curve 1" }), "Expand Curve 1"));
  until("the strip", (s) => s.find({ role: "card", label: "Curve 1 strip" }));
  nav.inGraph((s) => s.find({ role: "card", label: "Curve 1 strip" }), "Curve 1 strip");
  settle();
}

// A band `height` tall just above the box, from x share `a` to `b`.
const above = (box, a, b, height) => ({
  x: box.x + box.width * a,
  y: box.y - height - 2,
  width: box.width * (b - a),
  height,
});

test("an overshooting curve draws above the box, and its handle shows there", () => {
  open();
  // Select the tail, a straight segment: no handles draw, only the curve.
  app.click(node("slider", "Curve 1 point 4"));
  settle();
  const box = node("card", "Curve 1 strip").bounds;
  const shot = app.screenshot();
  // Over the rise the curve passes the top edge; over the tail, at 0.2,
  // nothing draws above the box.
  const over = image.stats(shot, above(box, 0.3, 0.55, 8));
  const tail = image.stats(shot, above(box, 0.7, 0.95, 8));
  assert(over.max > tail.max + 60, `no curve above the box: ${over.max} vs ${tail.max}`);

  // The rise's own handles: the first one sits above the box.
  app.click(node("slider", "Curve 1 point 1"));
  settle();
  const handle = node("slider", "Curve 1 segment 1 handle 1").bounds;
  assert(handle.y + handle.height / 2 < box.y, `handle at ${handle.y}, box top ${box.y}`);
  const lit = image.stats(app.screenshot(), handle);
  const dark = image.stats(app.screenshot(), above(box, 0.7, 0.95, 8));
  assert(lit.meanLuma > dark.meanLuma + 20, `the handle does not show: ${lit.meanLuma} vs ${dark.meanLuma}`);
  image.keep(
    app.screenshot({ node: node("card", "Color 1") }),
    "curve_strip/overshoot",
  );
});
