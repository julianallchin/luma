// Thumbs on a turn: a finished turn carries a good and a bad button under its
// reply. One at most is lit, pressing the lit one takes the verdict back, and
// the verdict is the thread's — it is still there when the thread is opened
// again.
//
// The `python` stand-in answers slowly so there is a moment at which the turn
// is half-finished and must show no thumbs. Compact turns are off so that
// moment can be told by its running tool chip.

const OPENING = "Sampling the ramp first.";
const CLOSING = "The ramp peaks at bar 3.";
const PROMPT = "where does the ramp peak?";

fixture({
  window: [1400, 900],
  motion: false,
  model_cadence_ms: 10,
  sql: ["INSERT OR REPLACE INTO settings (key, value) VALUES ('compact_turns', 'false')"],
  model: [
    [
      { text: OPENING },
      {
        call: "python",
        id: "call-1",
        args: { verb: "Checking", verbPast: "Checked", purpose: "ramp peak", code: "ramp.peak()" },
      },
      { end: "tool_use" },
    ],
    [{ text: CLOSING }, { end: "end_turn" }],
  ],
  tools: [
    {
      name: "python",
      latency_ms: 300,
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

const GOOD = "Good response";
const BAD = "Bad response";
const thumbs = (s, label) => s.findAll({ role: "button", label });
const thumb = (s, label) => thumbs(s, label)[0];
// A lit thumb reports itself the way every toggle does.
const lit = (s) => [GOOD, BAD].filter((label) => thumb(s, label)?.focused);
const has = (s, line) => s.findAll({ role: "text" }).some((n) => n.label.includes(line));
const button = (s, label) => s.find({ role: "button", label });

const press = (label, want) => {
  app.click(thumb(app.snapshot(), label));
  return until(`${label} pressed`, (s) => JSON.stringify(lit(s)) === JSON.stringify(want));
};

test("a finished turn takes one verdict, and the thread keeps it", () => {
  nav.trackEditor("Test Venue", "Aurora");
  nav.step("new conversation", "button", "New chat");
  until("the composer", (s) => {
    const send = button(s, "Send");
    return send && send.bounds.width > 0 && button(s, "New chat")?.enabled;
  });
  expect(thumbs(app.snapshot(), GOOD)).toEqual([]);

  app.type(app.snapshot().find({ role: "input", label: "Do anything…" }), PROMPT);
  app.click(button(app.snapshot(), "Send"));

  // Half-finished: the reply's first words are up and the tool is running.
  const running = until("the tool call", (s) =>
    busy(s) && s.findAll({ role: "chip" }).some((n) => n.label.startsWith("Checking")));
  expect(thumbs(running, GOOD)).toEqual([]);
  expect(thumbs(running, BAD)).toEqual([]);

  // One pair for the turn, under its reply and not under the prompt.
  const settled = until("the settled turn", (s) => has(s, CLOSING) && !busy(s) && thumb(s, GOOD));
  expect(thumbs(settled, GOOD).length).toBe(1);
  expect(thumbs(settled, BAD).length).toBe(1);
  expect(lit(settled)).toEqual([]);
  const reply = settled.findAll({ role: "text" }).find((n) => n.label.includes(CLOSING));
  expect(thumb(settled, GOOD).bounds.y).toBeGreaterThan(reply.bounds.y);
  expect(thumb(settled, GOOD).bounds.x).toBeLessThan(thumb(settled, BAD).bounds.x);

  press(GOOD, [GOOD]);
  // The lit one again takes it back.
  press(GOOD, []);
  // The other one switches rather than adds.
  press(BAD, [BAD]);
  press(GOOD, [GOOD]);

  // Leave the thread and come back to it through the history.
  app.click(button(app.snapshot(), "New chat"));
  until("the fresh conversation", (s) => !has(s, CLOSING) && button(s, "New chat")?.enabled);
  expect(thumbs(app.snapshot(), GOOD)).toEqual([]);
  app.click(button(app.snapshot(), "Chat history"));
  const listed = until("the chat list", (s) => s.find({ role: "card", label: PROMPT }));
  app.click(listed.find({ role: "card", label: PROMPT }));
  const back = until("the thread, reopened", (s) => has(s, CLOSING) && lit(s).length > 0);
  expect(lit(back)).toEqual([GOOD]);
});
