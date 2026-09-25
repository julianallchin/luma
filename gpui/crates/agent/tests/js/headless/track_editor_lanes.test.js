// The lane stack's vertical contract, on a score with more layers than the
// canvas is tall.
//
// The block is bottom-anchored, so z = 0 sits on the floor and the lanes that
// do not fit run off the top, under the waveform. The ways back to them are
// the bare wheel and the vertical zoom, and `H` fits the whole stack at once.

// Eight layers, one clip each: nine lanes with the empty insertion lane,
// more than the window has room for.
const LAYERS = 8;
fixture({
  seconds: 20,
  clips: Array.from({ length: LAYERS }, (_, z) => ({ pattern: `pattern-l${z}`, name: `L${z}`, start: 2, end: 6, lane: z })),
});

const node = (role, label) => app.snapshot().find({ role, label });

// Every lane, top to bottom, at the height it is visible at: a lane the
// waveform covers is clipped to nothing.
function reading() {
  const shot = app.snapshot();
  const lanes = shot.findAll({ role: "row" }).filter((n) => n.label.startsWith("Lane "))
    .map((n) => ({ label: n.label, y: n.bounds.y, height: n.bounds.height }))
    .sort((a, b) => a.y - b.y);
  // The playhead spans the whole canvas.
  const head = shot.find({ role: "slider", label: "Playhead" }).bounds;
  return {
    lanes,
    first: lanes[0],
    last: lanes.at(-1),
    canvas: { top: head.y, bottom: head.y + head.height },
    shortest: Math.min(...lanes.map((r) => r.height)),
    tallest: Math.max(...lanes.map((r) => r.height)),
  };
}

// z = 0 is on the canvas floor: what bottom-anchored means.
function onTheFloor(r, what) {
  const floor = r.last.y + r.last.height;
  assert(Math.abs(floor - r.canvas.bottom) <= 1, `${what}: the lowest lane ends at ${floor}, not on the canvas floor at ${r.canvas.bottom}`);
}

function wheel(target, dy, steps, modifiers) {
  app.scroll(target, { dy, steps, modifiers });
  app.frames(2);
  return reading();
}

test("the lane stack sits on the floor and the wheel reaches the rest of it", () => {
  nav.venue("Test Venue");
  app.frames(8);
  nav.track("Aurora");
  until("the timeline", (s) => s.find({ role: "card", label: "Waveform" }) !== undefined);
  nav.expand();
  // The lane stack's own geometry: give the editor the whole column.
  nav.stageOff();

  const opened = reading();
  expect(opened.lanes.length).toBe(LAYERS + 1);
  onTheFloor(opened, "the lanes as they opened");
  // The overflow runs off the top, under the waveform.
  expect(opened.first.height).toBe(0);

  // A bare vertical wheel walks the block up, and walking it back down puts
  // z = 0 on the floor again however far past the end it is pushed.
  const lifted = wheel(node("row", "Lane 5"), 400, 10);
  expect(lifted.first.height).toBeGreaterThan(0);
  const dropped = wheel(node("row", "Lane 5"), -4000, 20);
  expect([dropped.first.height, dropped.last.y]).toEqual([opened.first.height, opened.last.y]);
  onTheFloor(dropped, "the lanes after scrolling back down");

  // Alt+wheel is the vertical zoom, clamped at both ends: a second notch past
  // the clamp changes nothing.
  const shrunk = wheel(node("row", "Lane 5"), -600, 10, ["alt"]);
  expect(shrunk.tallest).toBeLessThan(opened.tallest);
  // At the smallest zoom every lane fits, so none is clipped.
  expect(shrunk.shortest).toBe(shrunk.tallest);
  onTheFloor(shrunk, "the shrunk lanes");
  expect(wheel(node("row", "Lane 5"), -600, 10, ["alt"]).tallest).toBe(shrunk.tallest);

  // Over the waveform it means nothing: that band is navigation, not the
  // workspace.
  expect(wheel(node("card", "Waveform"), 600, 10, ["alt"]).tallest).toBe(shrunk.tallest);

  const grown = wheel(node("row", "Lane 5"), 600, 10, ["alt"]);
  expect(grown.tallest).toBeGreaterThan(opened.tallest);
  expect(wheel(node("row", "Lane 5"), 600, 10, ["alt"]).tallest).toBe(grown.tallest);

  // H fits the stack: every lane visible at one height, the top one inside
  // the canvas, and still anchored.
  app.key("h");
  app.frames(2);
  const fitted = reading();
  assert(fitted.shortest > 0 && fitted.tallest - fitted.shortest <= 2, `H left a lane clipped out of view: ${JSON.stringify(fitted.lanes)}`);
  assert(fitted.first.y >= fitted.canvas.top - 1, "H left the top lane above the canvas");
  onTheFloor(fitted, "the fitted lanes");
});
