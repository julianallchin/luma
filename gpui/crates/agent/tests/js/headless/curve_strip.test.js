// The curve strip from the outside: one editor for a number curve and a
// gradient. A number's point moves in x and y and takes a typed value; over
// time the strip draws the beats one event spans and the playhead. A new
// curve is a chip on the row it feeds; it opens in place to its strip.

const WASH = { pattern: "graph-clip", name: "Wash", start: 1, end: 3, preset: "Wash" };
fixture({ seconds: 20, clips: [WASH], rig: 4, window: [1400, 1400] });

const node = (role, label) => until(label, (s) => s.find({ role, label })).find({ role, label });
const settle = () => app.frames(16, { waitMs: 60 });
const graphClip = () => library.score().clips["graph-clip"];
const nodes = () => graphClip().graph.nodes;

function open() {
  nav.venue("Test Venue");
  nav.track("Aurora");
  nav.expand();
  nav.stageOff();
  app.click(node("card", "Wash"));
  until("the graph", (s) => s.find({ role: "row", label: "Brightness" }));
  nav.widenGraph();
}

// The control of `role` and `label` in the row `row` of the graph card
// `card`, in view.
function inRow(card, row, role, label) {
  const r = nav.inCard(card, "row", row).bounds;
  const found = app.snapshot().findAll({ role, label }).find((n) => n.bounds.y >= r.y && n.bounds.y < r.y + r.height);
  if (!found) throw new Error(`no ${role} ${label} in ${row}`);
  return found;
}

// The strip of Curve 1, open on the Color 1 card, in view.
const strip = () => nav.inCard("Color 1", "card", "Curve 1 strip");

// Open the chip of Curve 1 on its card.
function expand() {
  app.click(nav.inGraph((s) => s.find({ role: "button", label: "Expand Curve 1" }), "Expand Curve 1"));
  until("the strip", (s) => s.find({ role: "card", label: "Curve 1 strip" }));
  settle();
}

// Brightness over time: a time and a curve, the curve's shape in a strip.
function promote() {
  app.click(inRow("Color 1", "Brightness", "select", "Value"));
  app.click(node("button", "Over time"));
  until("the chip", (s) => s.find({ role: "chip", label: "Curve 1" }));
  expand();
}

// Move the playhead by pressing the ruler at window x `x`.
function scrubTo(x) {
  const ruler = node("card", "Ruler").bounds;
  app.drag({ x, y: ruler.y + ruler.height / 2 }, { dx: 0, dy: 0 }, { steps: 1 });
  settle();
}

const field = (label) => app.snapshot().findAll({ role: "input" }).find((n) => n.label.startsWith(`${label} = `));

test("a number strip edits a point", () => {
  open();
  promote();
  strip();
  // Select the end point and type its value; brightness reads in percent.
  app.click(node("slider", "Curve 1 point 2"));
  settle();
  expect(field("Curve 1 position").label).toBe("Curve 1 position = 100");
  const was = node("slider", "Curve 1 point 2").bounds;
  app.click(field("Curve 1 value"));
  app.key("secondary-a backspace");
  app.type(field("Curve 1 value"), "25");
  app.key("enter");
  settle();
  const points = () => nodes().curve1.inputs.shape.points;
  expect(points().at(-1)[1]).toBe(0.25);
  // The point went down in the box, and kept its x.
  const end = node("slider", "Curve 1 point 2").bounds;
  assert(end.y > was.y, `the end point did not go down: ${end.y} vs ${was.y}`);
  expect(end.x).toBe(was.x);

  // A double-click adds a point on the curve, which changes no value.
  app.click(node("card", "Curve 1 strip"), { count: 2 });
  until("a point added", (s) => s.find({ role: "slider", label: "Curve 1 point 3" }));
  settle();
  expect(points().length).toBe(3);
  const [a, mid, b] = points();
  assert(Math.abs(mid[1] - (a[1] + (b[1] - a[1]) * mid[0])) < 1e-6, `the new point is off the line: ${JSON.stringify(points())}`);
});

test("a time strip draws the clip's beats and follows the playhead", () => {
  open();
  promote();
  const beats = () => app.snapshot().find((n) => n.label.startsWith("Curve 1 beat grid = "));
  until("the beat grid", () => beats());
  // Over time, the strip spans the clip: as many beats as the clip lasts.
  expect(Number(beats().label.split(" = ")[1])).toBe(graphClip().duration);

  const playhead = () => app.snapshot().find({ role: "text", label: "Curve 1 playhead" });
  // The track starts before the clip: no playhead on the strip.
  expect(playhead()).toBe(undefined);
  const clip = node("card", "Wash").bounds;
  const box = strip().bounds;
  const shareOf = () => {
    const p = playhead().bounds;
    return (p.x + p.width / 2 - box.x) / box.width;
  };
  scrubTo(clip.x + clip.width * 0.25);
  until("the playhead", () => playhead());
  const early = shareOf();
  scrubTo(clip.x + clip.width * 0.75);
  const late = shareOf();
  // The playhead moved with the track time, and sits where the clip is.
  assert(late > early, `the playhead did not move right: ${early} → ${late}`);
  assert(Math.abs(early - 0.25) < 0.05 && Math.abs(late - 0.75) < 0.05, `playhead at ${early} and ${late}`);
  // Past the clip it goes.
  scrubTo(clip.x + clip.width + 40);
  until("no playhead", () => !playhead());
});

test("a strip on a time with events spans one event", () => {
  open();
  promote();
  const beats = () => app.snapshot().find((n) => n.label.startsWith("Curve 1 beat grid = "));
  until("the beat grid", () => beats());
  const spanned = () => Number(beats().label.split(" = ")[1]);
  // With no events, the time runs once over the whole clip.
  expect(spanned()).toBe(graphClip().duration);
  // Events every beat: one event spans one beat.
  app.click(inRow("Time 1", "Every", "select", "Once"));
  app.click(node("button", "Value"));
  until("every beat", () => nodes().time1?.inputs?.every === 1);
  until("one beat", () => spanned() === 1);
  // Every two beats: the strip follows.
  const every = () => field("Time 1 every");
  until("the every field", () => every());
  nav.inCard("Time 1", "row", "Every");
  app.click(every());
  app.key("secondary-a backspace");
  app.type(every(), "2");
  app.key("enter");
  until("two beats", () => spanned() === 2);
  settle();
  expect(nodes().time1.inputs.every).toBe(2);
  // A duration of one beat: each event lasts one beat.
  app.click(inRow("Time 1", "Duration", "select", "Same as every"));
  app.click(node("button", "Value"));
  until("a duration", () => typeof nodes().time1.inputs.duration === "number");
  const duration = () => field("Time 1 duration");
  app.click(duration());
  app.key("secondary-a backspace");
  app.type(duration(), "1");
  app.key("enter");
  until("one beat again", () => spanned() === 1);
});

const GRADIENT = { pattern: "graph-clip", name: "Gradient", start: 1, end: 3, preset: "Gradient" };

test("a strip across space marks where each head falls", { fixture: { clips: [GRADIENT] } }, () => {
  nav.venue("Test Venue");
  nav.track("Aurora");
  nav.expand();
  nav.stageOff();
  app.click(node("card", "Gradient"));
  nav.widenGraph();
  expand();
  strip();
  const marks = () => app.snapshot().findAll({ role: "text" }).filter((n) => /^Curve 1 head \d+$/.test(n.label));
  // Each mark's share of the strip's width, left to right.
  const shares = () => {
    const box = node("card", "Curve 1 strip").bounds;
    return marks().map((m) => (m.bounds.x + m.bounds.width / 2 - box.x) / box.width).sort((a, b) => a - b);
  };
  until("head marks", () => marks().length >= 2);
  // A line over the whole axis runs from the lowest head at 0 to the highest at 1.
  const line = shares();
  assert(line.every((x) => x >= -0.01 && x <= 1.01), `marks off the strip: ${line}`);
  assert(line[0] < 0.02 && line.at(-1) > 0.98, `a line spans the strip: ${line}`);
  // In order, the heads sit evenly, each at the centre of its cell.
  app.click(nav.inCard("place", "button", "Order"));
  strip();
  until("order stored", () => nodes().place.settings.kind === "order");
  const n = line.length;
  until("even marks", () => {
    const order = shares();
    return order.length === n && order.every((x, i) => Math.abs(x - (i + 0.5) / n) < 0.02);
  });
});
