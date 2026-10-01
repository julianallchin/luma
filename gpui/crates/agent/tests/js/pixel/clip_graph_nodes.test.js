// One capture per node card of the Slash clip graph, kept under
// clip_graph_nodes/ for design review. Asserts only that each card shows.

const graph = {
  version: 2,
  nodes: {
    clock1: { kind: "clock", inputs: { every: 2, duration: 2 } },
    time1: { kind: "time", inputs: { clock: { node: "clock1" } } },
    space1: {
      kind: "space", settings: { kind: "line", wrap: "no" },
      inputs: { direction: [-0.8192, 0, 0.5736] },
    },
    space2: {
      kind: "space", settings: { kind: "line", wrap: "no" },
      inputs: { direction: [0.5736, 0, 0.8192] },
    },
    curve4: {
      kind: "curve", settings: { kind: "number" },
      inputs: { x: { node: "time1" }, shape: { points: [[0, 1, "hold"], [0.2, 1, "sine-out"], [1, 0]] } },
    },
    curve5: {
      kind: "curve", settings: { kind: "number" },
      inputs: { x: { node: "space1" }, shape: { points: [[0, 1], [1, 1]] }, low: 0, high: { node: "curve4" } },
    },
    curve6: {
      kind: "curve", settings: { kind: "number" },
      inputs: { x: { node: "space2" }, shape: { points: [[0, 0], [0.12, 1], [0.88, 1], [1, 0]] }, low: 0, high: { node: "curve5" } },
    },
    curve7: {
      kind: "curve", settings: { kind: "color" },
      inputs: {
        x: { node: "time1" },
        gradient: { stops: [
          { t: 0, color: [1, 1, 1] }, { t: 0.2, color: [1, 1, 1] },
          { t: 0.5, color: [1, 0, 0.01] }, { t: 1, color: [1, 0, 0.01] },
        ] },
      },
    },
    color1: { kind: "color", inputs: { color: { node: "curve7" }, brightness: { node: "curve6" } } },
  },
};

fixture({
  seconds: 20,
  clips: [{ pattern: "slash", name: "Diagonal slash", start: 1, end: 3, preset: "Diagonal slash", graph }],
  rig: 4,
  window: [2400, 1800],
});

const CARDS = [
  "Clock 1", "Time 1", "Space 1", "Space 2",
  "Curve 4", "Curve 5", "Curve 6", "Curve 7", "Color 1",
];

test("each node card, captured", () => {
  nav.trackEditor("Test Venue", "Aurora");
  nav.expand();
  nav.stageOff();
  app.click(until("the clip", (s) => s.find({ role: "card", label: "Diagonal slash" }))
    .find({ role: "card", label: "Diagonal slash" }));
  nav.widenGraph();
  app.frames(6, { waitMs: 30 });
  for (const card of CARDS) {
    nav.inGraph((s) => s.find({ role: "card", label: card }), card);
    app.frames(3, { waitMs: 30 });
    const node = app.snapshot().find({ role: "card", label: card });
    const kept = image.keep(app.screenshot({ node }), `clip_graph_nodes/${card.toLowerCase().replace(" ", "_")}`);
    console.log(kept);
  }
});
