// Opening or closing a tool call is the reader's own act: the transcript above
// the chip stays where it is on screen, frame by frame, while the call's detail
// folds open or shut below it.
//
// The call is in a short exchange after a long one, so the transcript scrolls
// and the sent prompt rests near the top over the room reserved under it —
// the place a fold used to pull the view: the room answered a fold one frame
// after the list had already clamped its scroll to the shorter content.

const LINE = "A softly lit stage.";

fixture({
  window: [1280, 720],
  motion: true,
  model_cadence_ms: 10,
  model: [
    [{ text: `${LINE}\n\n`.repeat(30) }, { end: "end_turn" }],
    [
      { text: "Checking the ramp." },
      {
        call: "python",
        id: "call-1",
        args: { verb: "Checking", verbPast: "Checked", purpose: "ramp peak", code: "ramp.peak()" },
      },
    ],
    [{ text: "It peaks at bar 3." }, { end: "end_turn" }],
  ],
  tools: [
    {
      name: "python",
      latency_ms: 50,
      schema: {
        type: "object",
        required: ["verb", "verbPast", "purpose", "code"],
        properties: {
          verb: { type: "string" },
          verbPast: { type: "string" },
          purpose: { type: "string" },
          code: { type: "string" },
        },
      },
      result: {
        status: "ok",
        stdout: "peak=3.0\n",
        stderr: "",
        repr: null,
        traceback: null,
        notices: [],
        figures: [],
        durationMs: 12,
      },
    },
  ],
});

const texts = (s) => s.findAll({ role: "text" }).map((n) => n.label);
const busy = (s) => texts(s).some((label) => label === "Working" || label === "Sending");
const chip = (s) => s.findAll({ role: "chip" }).find((n) => n.label.startsWith("Checked"));
// The paragraph right above the chip.
const above = (s) => s.find({ role: "text", label: "Checking the ramp." });

// The row that holds the chip: its node grows and shrinks with the fold.
const chipRow = (s) => {
  const c = chip(s);
  return c && s.findAll({ role: "text" }).find((n) => n.label === c.label);
};

// Wait until nothing moves: the pin has parked, or the fold has finished. A
// poll that finds no new work returns the same frame, so a few equal polls in
// a row mean the panel stopped asking for frames.
const settle = (what, onFrame = () => {}) => {
  let last;
  let same = 0;
  return until(what, (s) => {
    onFrame();
    const c = chip(s);
    const row = chipRow(s);
    const shape = c && row && `${c.bounds.y}:${row.bounds.height}`;
    same = shape && shape === last ? same + 1 : 0;
    last = shape;
    return same >= 3;
  });
};

// Every frame the click and the fold drew, until the fold has settled.
const foldFrames = (from) => {
  const seen = new Map();
  const collect = () => {
    for (const s of app.painted()) if (s.frame > from.frame) seen.set(s.frame, s);
  };
  collect();
  settle("the fold to settle", collect);
  collect();
  return [...seen.values()].sort((a, b) => a.frame - b.frame);
};

const expectStill = (frames, y, what) => {
  assert(frames.length > 2, `${what}: too few frames to see the fold`);
  for (const s of frames) {
    const c = chip(s);
    if (c) expect(Math.abs(c.bounds.y - y)).toBeLessThan(0.5);
  }
};

test("folding a tool call leaves the transcript above it still", () => {
  nav.trackEditor("Test Venue", "Aurora");
  nav.step("new conversation", "button", "New chat");
  until("the composer", (s) => {
    const send = s.find({ role: "button", label: "Send" });
    return send && send.bounds.width > 0 && s.find({ role: "button", label: "New chat" })?.enabled;
  });
  const send = (text) => {
    app.type(app.snapshot().find({ role: "input", label: "Do anything…" }), text);
    app.click(app.snapshot().find({ role: "button", label: "Send" }));
  };
  send("Light the stage");
  until("the first reply", (s) => !busy(s) && texts(s).includes(LINE));
  send("where does the ramp peak?");
  until("the answer", (s) => !busy(s) && texts(s).some((l) => l.includes("It peaks at bar 3.")));
  // Let the pin bring the end into view and park.
  const rest = settle("the chip at rest");
  assert(above(rest), "no paragraph above the chip");

  const before = chip(rest).bounds.y;
  const line = above(rest).bounds.y;
  const ready = app.snapshot();
  app.click(chip(ready));
  const opened = foldFrames(ready);
  expectStill(opened, before, "opening");
  const open = opened[opened.length - 1];
  expect(chipRow(open).bounds.height).toBeGreaterThan(chipRow(rest).bounds.height);
  expect(Math.abs(above(open).bounds.y - line)).toBeLessThan(0.5);

  const shown = app.snapshot();
  app.click(chip(shown));
  const closed = foldFrames(shown);
  expectStill(closed, before, "closing");
  const shut = closed[closed.length - 1];
  expect(Math.abs(chipRow(shut).bounds.height - chipRow(rest).bounds.height)).toBeLessThan(0.5);
});
