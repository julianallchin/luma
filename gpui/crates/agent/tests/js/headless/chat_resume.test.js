// A turn that stopped part way, seen from the chat: the reader's own stop is
// final, and a turn cut short some other way offers Resume.
//
// The second test's first turn ends on a step that asked for tools and named
// none — the one shape a scripted model can leave that a quit also leaves:
// the last step on disk says `tool_use` and nothing answered it.

const PROMPT = "where does the ramp peak?";

fixture({
  window: [1400, 900],
  model_cadence_ms: 25,
  model: [
    [
      { text: "Checking the ramp." },
      {
        call: "python",
        id: "call-1",
        args: { verb: "Checking", verbPast: "Checked", purpose: "ramp peak", code: "ramp.peak()" },
      },
    ],
    [{ text: "The step before this one" }, { end: "tool_use" }],
    [{ text: "It peaks at bar 3." }, { end: "end_turn" }],
  ],
  tools: [
    {
      name: "python",
      latency_ms: 1500,
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
const chips = (s) => s.findAll({ role: "chip" }).map((n) => n.label);
const busy = (s) => texts(s).some((label) => label === "Working" || label === "Sending");
const composer = (s) => s.find({ role: "input", label: "Do anything…" });
const button = (label) => app.snapshot().find({ role: "button", label });
const resume = (s) => s.find({ role: "button", label: "Resume" });

const openChat = () => {
  nav.trackEditor("Test Venue", "Aurora");
  nav.step("new conversation", "button", "New chat");
  until("the conversation", (s) => s.find({ role: "button", label: "New chat" })?.enabled);
  until("the chat centre", (s) => {
    const send = s.find({ role: "button", label: "Send" });
    return send !== undefined && send.bounds.width > 0;
  });
};

const send = () => {
  app.type(composer(app.snapshot()), PROMPT);
  app.click(button("Send"));
  until("the turn to begin", busy);
};

test("a stopped turn is not offered for resume", () => {
  openChat();
  send();
  until("the tool call start", (s) => chips(s).some((c) => c.startsWith("Checking")));
  app.click(button("Stop"));
  until("the turn to stop", (s) => !busy(s));
  // The stop is written, and the thread read back, before the panel settles
  // on it: the call that was running is then answered.
  until("the recorded stop", (s) => !chips(s).some((c) => c.startsWith("Checking")));
  expect(resume(app.snapshot())).toBe(undefined);
});

test("a turn cut short offers resume, and resume finishes it", () => {
  openChat();
  send();
  const cut = until("the turn to end part way", (s) => !busy(s) && resume(s));
  expect(texts(cut).some((l) => l.includes("It peaks at bar 3."))).toBe(false);

  app.click(resume(cut));
  const done = until("the resumed answer", (s) =>
    !busy(s) && texts(s).some((l) => l.includes("It peaks at bar 3.")));
  expect(resume(done)).toBe(undefined);
});
