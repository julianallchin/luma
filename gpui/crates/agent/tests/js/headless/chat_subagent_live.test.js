// A child's thread opened while it runs follows its turn live, as the main
// chat follows its own: the running tool chip appears, completes, and the
// answer streams in without reopening the reader, then settles on the rows
// with nothing doubled.
//
// Only the model is scripted; the parent's `subagent` call is the shipped
// tool. The child's slow tool is what leaves time to open its thread mid-run.

const DESCRIPTION = "Fitting the chorus ramp";
const ANSWER = "Raised the ramp two bars early.";

fixture({
  window: [1280, 900],
  model_cadence_ms: 25,
  model: [
    [{
      call: "subagent",
      id: "call-sub",
      args: { description: DESCRIPTION, task: "Move the ramp two bars earlier and report what you changed." },
    }],
    [{
      call: "python",
      id: "call-child",
      args: { verb: "Measuring", verbPast: "Measured", purpose: "ramp bounds", code: "ramp.bounds()" },
    }],
    [{ text: ANSWER }],
    [{ text: "The subagent moved the ramp and its work is merged." }],
  ],
  tools: [
    {
      name: "python",
      latency_ms: 4000,
      result: { status: "ok", stdout: "", stderr: "", repr: null, traceback: null, notices: [], figures: [], durationMs: 12 },
    },
    "subagent",
  ],
});

const texts = (s) => s.findAll({ role: "text" }).map((n) => n.label);
const chips = (s) => s.findAll({ role: "chip" }).map((n) => n.label);
const busy = (s) => texts(s).some((label) => label === "Working" || label === "Sending");
const row = (s) => s.findAll({ role: "card" }).find((n) => n.label.includes(DESCRIPTION));
const answers = (s) => texts(s).filter((l) => l.includes(ANSWER)).length;

test("a running child's thread streams in its reader", () => {
  nav.trackEditor("Test Venue", "Aurora");
  nav.step("new conversation", "button", "New chat");
  until("the chat centre", (s) => {
    const send = s.find({ role: "button", label: "Send" });
    return send !== undefined && send.bounds.width > 0 && s.find({ role: "button", label: "New chat" })?.enabled;
  });
  app.type(app.snapshot().find({ role: "input", label: "Do anything…" }), "move the ramp");
  app.click(app.snapshot().find({ role: "button", label: "Send" }));
  until("the turn to begin", busy);

  nav.step("the subagent pill", "button", "1 subagent working");
  app.click(until("the subagents dialog", row).findAll({ role: "card" }).find((n) => n.label.includes(DESCRIPTION)));

  // The child's tool call is live state: no row holds it while it runs.
  const running = until("the child's running chip", (s) => chips(s).includes("Measuring ramp bounds"));
  expect(answers(running)).toBe(0);

  // The same reader, never reopened, follows the call to its end and the
  // answer after it.
  const done = until("the child's answer", (s) => answers(s) === 1 && chips(s).includes("Measured ramp bounds"));
  assert(!chips(done).includes("Measuring ramp bounds"), "the finished call still reads as running");

  // Settled on the persisted rows: one answer, one chip.
  const settled = until("the parent turn end", (s) => !busy(s));
  expect(answers(settled)).toBe(1);
  expect(chips(settled).filter((c) => c === "Measured ramp bounds").length).toBe(1);
});
