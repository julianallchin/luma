// Compact turns: a finished turn's work (narration, tool calls) folds behind
// one "Worked for" header and the final reply stays visible under it. Opening
// the header shows the work as ordinary rows; closing it hides them again.

const OPENING = "Sampling the ramp first.";
const CLOSING = "The ramp peaks at bar 3.";
const PROMPT = "where does the ramp peak?";

fixture({
  window: [1400, 900],
  motion: false,
  model_cadence_ms: 10,
  sql: ["INSERT OR REPLACE INTO settings (key, value) VALUES ('compact_turns', 'true')"],
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

const header = (s) => s.findAll({ role: "chip" }).find((n) => n.label.startsWith("Worked"));
const toolChip = (s) => s.findAll({ role: "chip" }).find((n) => n.label.startsWith("Checked"));
const has = (s, line) => s.findAll({ role: "text" }).some((n) => n.label.includes(line));

test("a settled turn folds its work behind a header and opens on a click", () => {
  nav.trackEditor("Test Venue", "Aurora");
  nav.step("new conversation", "button", "New chat");
  until("the composer", (s) => {
    const send = s.find({ role: "button", label: "Send" });
    return send && send.bounds.width > 0 && s.find({ role: "button", label: "New chat" })?.enabled;
  });
  app.type(app.snapshot().find({ role: "input", label: "Do anything…" }), PROMPT);
  app.click(app.snapshot().find({ role: "button", label: "Send" }));

  const folded = until("the settled, folded turn", (s) => header(s) && has(s, CLOSING) && !busy(s));
  assert(!toolChip(folded), "the tool call is still showing outside the header");
  assert(!has(folded, OPENING), "the narration is still showing outside the header");

  app.click(header(folded));
  const opened = until("the work", (s) => toolChip(s) && has(s, OPENING));
  assert(has(opened, CLOSING), "the final reply left with the fold");
  // The work sits above the reply, as in the ordinary transcript.
  expect(toolChip(opened).bounds.y).toBeLessThan(
    opened.findAll({ role: "text" }).find((n) => n.label.includes(CLOSING)).bounds.y,
  );

  app.click(header(opened));
  const closed = until("the work to fold away", (s) => !toolChip(s) && header(s));
  assert(has(closed, CLOSING), "the final reply left with the fold");
});
