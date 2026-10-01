// The Slash clip graph under the renderer, kept for design review: the
// whole canvas, then one capture per node card, and the fade card open.
// Its line direction and its place on the line are value nodes, each wired
// into the mirror and the bloom's space. Every node is a card; asserts only
// that each card shows.

const graph = {
  version: 3,
  nodes: {
    t: { kind: "time", inputs: { every: 2 } },
    curve1: { kind: "curve", inputs: { x: { node: "t" }, shape: { points: [[0, 1], [0.2, 0], [1, 0]] } } },
    diag: { kind: "space", inputs: { direction: [0.82, 0, -0.57], shift: { node: "curve1" } } },
    cut: { kind: "curve", inputs: { x: { node: "diag" }, shape: { points: [[0, 0], [0, 1], [1, 1]] } } },
    d: { kind: "value", inputs: { value: [0.57, 0, 0.82] } },
    at: { kind: "value", inputs: { value: 0.68 } },
    line: { kind: "mirror", inputs: { direction: { node: "d" }, at: { node: "at" } } },
    curve2: { kind: "curve", inputs: { x: { node: "t" }, low: 0.04, high: 0.74 } },
    dist: { kind: "space", inputs: { heads: { node: "line" }, direction: { node: "d" }, shift: { node: "at" }, scale: { node: "curve2" } } },
    bloom: { kind: "curve", inputs: { x: { node: "dist" }, shape: { points: [[0, 1], [0.76, 1], [1, 0]] } } },
    fade: { kind: "curve", inputs: { x: { node: "t" }, shape: { points: [[0, 1, "hold"], [0.2, 1, "sine-out"], [1, 0]] } } },
    heat: {
      kind: "curve", settings: { kind: "color" },
      inputs: { x: { node: "t" }, gradient: { stops: [
        { t: 0, color: [1, 1, 1] }, { t: 0.2, color: [1, 1, 1] }, { t: 0.5, color: [1, 0, 0.01] }, { t: 1, color: [1, 0, 0.01] },
      ] } },
    },
    mix: { kind: "math", inputs: { values: [{ node: "cut" }, { node: "bloom" }, { node: "fade" }] } },
    color1: { kind: "color", inputs: { color: { node: "heat" }, brightness: { node: "mix" } } },
  },
};

fixture({
  seconds: 20,
  clips: [{ pattern: "slash", name: "Slash", start: 1, end: 3, preset: "Pulse", graph }],
  rig: 4,
  window: [2400, 1800],
});

const CARDS = ["t", "Curve 1", "diag", "d", "at", "line", "Curve 2", "dist", "cut", "bloom", "fade", "heat", "mix", "Color 1"];
const file = (card) => `card_${card.toLowerCase().replace(" ", "_")}`;

test("the Slash canvas and each node card, captured", () => {
  nav.trackEditor("Test Venue", "Aurora");
  nav.expand();
  nav.stageOff();
  app.click(until("the clip", (s) => s.find({ role: "card", label: "Pulse" }))
    .find({ role: "card", label: "Pulse" }));
  nav.widenGraph();
  app.frames(6, { waitMs: 30 });
  app.click(until("fit", (s) => s.find({ role: "button", label: "Fit graph" })).find({ role: "button", label: "Fit graph" }));
  app.frames(6, { waitMs: 30 });
  const canvas = app.snapshot().find({ role: "card", label: "Graph canvas" });
  console.log(image.keep(app.screenshot({ node: canvas }), "canvas"));
  app.click(until("actual size", (s) => s.find({ role: "button", label: "Actual size" })).find({ role: "button", label: "Actual size" }));
  app.frames(4, { waitMs: 30 });
  for (const card of CARDS) {
    nav.inGraph((s) => s.find({ role: "card", label: card }), card);
    app.frames(3, { waitMs: 30 });
    const node = app.snapshot().find({ role: "card", label: card });
    console.log(image.keep(app.screenshot({ node }), file(card)));
  }
  app.click(nav.inGraph((s) => s.find({ role: "button", label: "Expand fade" }), "Expand fade"));
  until("the strip", (s) => s.find({ role: "card", label: "fade strip" }));
  nav.inGraph((s) => s.find({ role: "card", label: "fade" }), "fade");
  app.frames(3, { waitMs: 30 });
  console.log(image.keep(app.screenshot({ node: app.snapshot().find({ role: "card", label: "fade" }) }), "card_fade_open"));
});
