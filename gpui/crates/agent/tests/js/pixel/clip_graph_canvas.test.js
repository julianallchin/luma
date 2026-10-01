// The clip graph canvas under the renderer: each wire takes the colour of
// what it carries, and the canvas zooms whole, cards and wires together.

const clipOf = (preset) => ({ pattern: "graph-clip", name: preset, start: 1, end: 3, preset });
// A named curve is a card, so its number wire shows.
const graph = {
  version: 3,
  nodes: {
    t: { kind: "time", inputs: { every: 2 } },
    cut: { kind: "curve", settings: { kind: "number" }, inputs: { x: { node: "t" }, shape: { points: [[0, 1], [1, 0]] } } },
    color1: { kind: "color", inputs: { brightness: { node: "cut" } } },
  },
};
fixture({ seconds: 20, clips: [{ ...clipOf("Pulse"), graph }], rig: 4, window: [1600, 1000] });

const node = (role, label) => until(label, (s) => s.find({ role, label })).find({ role, label });

function open(preset = "Pulse") {
  nav.trackEditor("Test Venue", "Aurora");
  nav.expand();
  nav.stageOff();
  app.click(node("card", preset));
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

test("a time wire and a number wire differ in colour", () => {
  open();
  const [cr, cg, cb] = wireColor("t → cut x");
  const [nr, ng, nb] = wireColor("cut → Color 1 brightness");
  // A coordinate reads blue, a number amber.
  assert(cb > cr + 20, `time wire: ${[cr, cg, cb]}`);
  assert(nr > nb + 20, `number wire: ${[nr, ng, nb]}`);
  image.keep(app.screenshot(), "clip_graph/wires");
});

// Sparkle rain's node cards.
const RAIN = ["k", "bars", "order", "columns", "drop", "rank", "Color 1"];

test("zooming out shows more of the graph at once", { fixture: { clips: [clipOf("Sparkle rain")] } }, () => {
  open("Sparkle rain");
  const inView = () => {
    const c = node("card", "Graph canvas").bounds;
    return app.snapshot().findAll({ role: "card" })
      .filter((n) => RAIN.includes(n.label) && n.bounds.width > 0 && n.bounds.x >= c.x && n.bounds.x + n.bounds.width <= c.x + c.width)
      .length;
  };
  const before = inView();
  app.click(node("button", "Fit graph"));
  app.frames(4, { waitMs: 30 });
  assert(inView() > before, `fit shows ${inView()} cards, rest ${before}`);
  image.keep(app.screenshot(), "clip_graph/fit");
});
