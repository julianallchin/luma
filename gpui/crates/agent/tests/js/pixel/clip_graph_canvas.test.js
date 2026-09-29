// The clip graph canvas under the renderer: each wire takes the colour of
// what it carries, and the canvas zooms whole, cards and wires together.

const clipOf = (preset) => ({ pattern: "graph-clip", name: preset, start: 1, end: 3, preset });
fixture({ seconds: 20, clips: [clipOf("Chase")], rig: 4, window: [1600, 1000] });

const node = (role, label) => until(label, (s) => s.find({ role, label })).find({ role, label });

function open() {
  nav.trackEditor("Test Venue", "Aurora");
  nav.expand();
  nav.stageOff();
  app.click(node("card", "Chase"));
  nav.widenGraph();
  app.frames(6, { waitMs: 30 });
}

// The mean colour of a small box on the wire's midpoint, with the wire in view.
function wireColor(label) {
  const mid = nav.inGraph((s) => s.find({ role: "text", label }), label).bounds;
  app.frames(3, { waitMs: 30 });
  const box = { x: mid.x + mid.width / 2 - 2, y: mid.y + mid.height / 2 - 2, width: 4, height: 4 };
  return image.stats(app.screenshot(), box).mean;
}

test("a clock wire and a number wire differ in colour", () => {
  open();
  const [cr, cg, cb] = wireColor("Clock 1 → Time 1 clock");
  const [nr, ng, nb] = wireColor("Curve 2 → Color 1 brightness");
  // A clock reads green, a number amber.
  assert(cg > cr && cg > cb, `clock wire: ${[cr, cg, cb]}`);
  assert(nr > nb + 20, `number wire: ${[nr, ng, nb]}`);
  image.keep(app.screenshot(), "clip_graph/wires");
});

test("zooming out shows more of the graph at once", () => {
  open();
  const inView = () => {
    const c = node("card", "Graph canvas").bounds;
    return app.snapshot().findAll({ role: "card" })
      .filter((n) => /^[A-Z][a-z]+ \d+$/.test(n.label) && n.bounds.width > 0 && n.bounds.x >= c.x && n.bounds.x + n.bounds.width <= c.x + c.width)
      .length;
  };
  const before = inView();
  app.click(node("button", "Fit graph"));
  app.frames(4, { waitMs: 30 });
  assert(inView() > before, `fit shows ${inView()} cards, rest ${before}`);
  image.keep(app.screenshot(), "clip_graph/fit");
});
