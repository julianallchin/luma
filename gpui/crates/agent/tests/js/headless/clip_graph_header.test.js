// The graph page's header is one strip: the way back, the name, the
// selection and the blend share a row, and the canvas takes the rest. A pane
// too narrow for one row wraps the strip instead of overlapping it.

const clip = { pattern: "graph-clip", name: "Sparkle rain", start: 1, end: 3, preset: "Sparkle rain" };
fixture({ seconds: 20, clips: [clip], rig: 4, window: [1280, 680] });

const node = (role, label) => until(label, (s) => s.find({ role, label })).find({ role, label });

function open() {
  nav.trackEditor("Test Venue", "Aurora");
  nav.program();
  nav.stageOff();
  app.click(node("card", "Sparkle rain"));
  nav.graphPage();
  const s = app.snapshot();
  const find = (role, label) => s.find({ role, label }).bounds;
  return {
    page: find("card", "Clip graph"),
    canvas: find("card", "Graph canvas"),
    controls: [
      find("button", "Back to timeline"),
      find("input", "Name"),
      s.findAll({ role: "input" }).find((n) => n.label.startsWith("expression")).bounds,
      find("button", "Pick fixtures"),
      find("row", "Blend"),
    ],
    toolbar: ["Add node", "Zoom out", "Actual size", "Zoom in", "Fit graph"].map((l) => find("button", l)),
  };
}

const overlaps = (a, b) =>
  a.x < b.x + b.width - 0.5 && b.x < a.x + a.width - 0.5 && a.y < b.y + b.height - 0.5 && b.y < a.y + a.height - 0.5;
const middle = (b) => b.y + b.height / 2;

function noOverlaps(boxes) {
  for (let i = 0; i < boxes.length; i++)
    for (let j = i + 1; j < boxes.length; j++)
      assert(!overlaps(boxes[i], boxes[j]), `controls ${i} and ${j} overlap: ${JSON.stringify([boxes[i], boxes[j]])}`);
}

test("the header is one row, and the canvas takes the rest of the page", () => {
  const { page, canvas, controls, toolbar } = open();
  const row = middle(controls[0]);
  for (const box of controls) assert(Math.abs(middle(box) - row) < 2, `off the header row: ${JSON.stringify(box)}`);
  noOverlaps(controls);
  for (const box of controls) assert(box.y + box.height <= canvas.y, "a header control sits on the canvas");
  // The header is one control tall plus its gutters; the canvas has the rest.
  const header = canvas.y - page.y;
  assert(header < 3 * controls[0].height, `header ${header}px tall`);
  // The canvas tools float on the canvas, in its top-right corner.
  for (const box of toolbar) {
    assert(box.y >= canvas.y && box.y + box.height <= canvas.y + canvas.height / 4, "a canvas tool left the corner");
    assert(box.x >= canvas.x + canvas.width / 2, "a canvas tool left the right half");
  }
});

test("a narrow pane wraps the header without overlapping", { fixture: { window: [640, 680] } }, () => {
  const { page, canvas, controls } = open();
  noOverlaps(controls);
  for (const box of controls) {
    assert(box.x >= page.x && box.x + box.width <= page.x + page.width + 0.5, `clipped: ${JSON.stringify(box)}`);
    assert(box.y + box.height <= canvas.y, "a header control sits on the canvas");
  }
});
