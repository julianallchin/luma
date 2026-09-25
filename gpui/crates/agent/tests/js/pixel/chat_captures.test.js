// Reference captures of the chat surfaces, for the gauntlet in
// `harness/gauntlet-chat/` (the style spec and its comet reference plates are
// the bar these are measured against). A person or a critic compares the
// pictures; what is asserted is only that each capture shows what it names.
//
// Regenerate the plates in the repository:
//
//   LUMA_SHOTS="$PWD/harness" gpui/test --pixel gpui/crates/agent/tests/js/pixel/chat_captures.test.js
//
// The turns are the ones `headless/chat.test.js` and `chat_subagents.test.js`
// assert on, so the pictures and the assertions describe the same replies.
// Only the model is scripted; the turn under it is real. Motion is on: under
// reduced motion a sent turn is never painted (see chat.test.js).

const OPENING = "Chasing the **downbeat**. I'll sample the ramp and check where it peaks.";
const CLOSING =
  "Here is the curve it settled on:\n\n```python\nfor beat in range(4):\n    ramp(beat, 0.5)\n```\n\n" +
  "- peak at bar 3\n- release over two bars\n";
const chunks = (text) => text.match(/[\s\S]{1,24}/g).map((chunk) => ({ text: chunk }));
// A warm shape: most of the prompt from the cache.
const USAGE = [
  { inputTokens: 12400, outputTokens: 96, cacheReadInputTokens: 300000, cacheCreationInputTokens: 12400 },
  { inputTokens: 4800, outputTokens: 512, cacheReadInputTokens: 610000, cacheCreationInputTokens: 12000 },
];
const PYTHON = {
  name: "python",
  latency_ms: 1200,
  schema: {
    type: "object",
    required: ["verb", "verbPast", "purpose", "code"],
    properties: { verb: { type: "string" }, verbPast: { type: "string" }, purpose: { type: "string" }, code: { type: "string" } },
  },
  result: {
    status: "ok",
    stdout: "peak=3.0\nrelease=2 bars\n",
    stderr: "",
    repr: "<Ramp peak=3.0 release=2.0>",
    traceback: null,
    notices: [],
    figures: [],
    durationMs: 247,
  },
};

fixture({
  window: [1440, 960],
  motion: true,
  model_cadence_ms: 25,
  model: [
    [
      ...chunks(OPENING),
      { call: "python", id: "call-1", args: { verb: "Checking", verbPast: "Checked", purpose: "ramp peak", code: "ramp.peak()" } },
      { end: "tool_use", usage: USAGE[0] },
    ],
    [...chunks(CLOSING), { end: "end_turn", usage: USAGE[1] }],
  ],
  tools: [PYTHON],
});

const texts = (s) => s.findAll({ role: "text" }).map((n) => n.label);
const chips = (s) => s.findAll({ role: "chip" }).map((n) => n.label);
const busy = (s) => texts(s).some((label) => label === "Working" || label === "Sending");
const keep = (shot, name) => image.keep(shot, `gauntlet-chat/${name}`);

// The track's new conversation, with Send pressable: an unattached centre
// has none, so that is proof the scope resolved.
function openChat() {
  nav.trackEditor("Test Venue", "Aurora");
  nav.step("new conversation", "button", "New chat");
  until("the chat centre", (s) => {
    const send = s.find({ role: "button", label: "Send" });
    return send !== undefined && send.bounds.width > 0 && s.find({ role: "button", label: "New chat" })?.enabled;
  });
  app.frames(8, { waitMs: 40 });
}

// Send, and wait for the turn to begin: "not busy" is equally true before it.
function send(prompt) {
  app.type(app.snapshot().find({ role: "input", label: "Do anything…" }), prompt);
  app.click(app.snapshot().find({ role: "button", label: "Send" }));
  until("the turn to begin", busy);
}

test("the chat panel across one turn", { timeoutMs: 120000 }, () => {
  openChat();
  const idle = app.screenshot();
  send("where does the ramp peak?");
  // Inside the tool's latency: prose painted, chip running. A beat into the
  // veil, so the plate shows a reply mid-arrival and not a grey block.
  until("the tool call start", (s) => chips(s).some((c) => c.startsWith("Checking")));
  app.frames(6, { waitMs: 30 });
  const streaming = app.screenshot();
  until("the turn end", (s) => chips(s).includes("Checked ramp peak") && !busy(s), { timeoutMs: 15000 });
  app.frames(4, { waitMs: 40 });
  const finished = app.screenshot();
  keep(idle, "gpui-chat-idle");
  keep(streaming, "gpui-chat-streaming");
  keep(finished, "gpui-chat-finished");
  expect(image.diff(idle, streaming)).toBeGreaterThan(0.01);
  expect(image.diff(streaming, finished)).toBeGreaterThan(0.01);
});

// The ring is painted at 14px; what is judged is that it reads as a ring,
// and that the card's column lines up. The whole panel, so the ring is seen
// beside the text it sits with. `chat.test.js` covers the values.
test("the context gauge at rest and open", { timeoutMs: 120000 }, () => {
  openChat();
  send("where does the ramp peak?");
  until("the turn end", (s) => !busy(s), { timeoutMs: 15000 });
  app.frames(4, { waitMs: 40 });
  const settled = app.screenshot();
  nav.step("the context gauge", "button", app.snapshot().find((n) => n.role === "button" && n.label.startsWith("Context ")).label);
  app.frames(4, { waitMs: 40 });
  const open = app.screenshot();
  keep(settled, "gpui-context-settled");
  keep(open, "gpui-context-open");
  expect(image.diff(settled, open)).toBeGreaterThan(0.005);
});

test("the model picker with its effort slider", { timeoutMs: 120000 }, () => {
  openChat();
  const picker = until("the model picker", (s) => s.find({ role: "select" }) !== undefined).find({ role: "select" });
  app.click(picker, { restale: "match" });
  until("the effort slider", (s) => s.find({ role: "slider", label: "Reasoning effort" }) !== undefined);
  app.frames(4, { waitMs: 40 });
  keep(app.screenshot(), "gpui-context-model-picker");
});

// One delegation: the chip while the child works (with the pill counting it),
// the dialog's list, the child's own transcript morphed into the same card,
// and the chip once the report has landed.
const DESCRIPTION = "Fitting the chorus ramp";
const ANSWER = "Raised the ramp two bars early.";
test(
  "the delegation surfaces",
  {
    fixture: {
      model: [
        [{ call: "subagent", id: "call-sub", args: { description: DESCRIPTION, task: "Move the ramp two bars earlier and report what you changed." } }],
        [{ call: "python", id: "call-child", args: { verb: "Measuring", verbPast: "Measured", purpose: "ramp bounds", code: "ramp.bounds()" } }],
        [{ text: ANSWER }],
        [{ text: "The subagent moved the ramp and its work is merged." }],
      ],
      tools: [PYTHON, "subagent"],
    },
    timeoutMs: 120000,
  },
  () => {
    const row = (s) => s.findAll({ role: "card" }).find((n) => n.label.includes(DESCRIPTION));
    openChat();
    send("move the ramp");
    until("the subagent pill", (s) => s.find({ role: "button", label: "1 subagent working" }) !== undefined);
    app.frames(4, { waitMs: 30 });
    const working = app.screenshot();

    nav.step("the subagent pill", "button", "1 subagent working");
    until("the subagents dialog", row);
    // Past the morph's entrance, so the card is at rest.
    app.frames(10, { waitMs: 30 });
    const listed = app.screenshot();

    until("the turn end", (s) => !busy(s), { timeoutMs: 15000 });
    app.click(row(app.snapshot()));
    until("the child's transcript", (s) => texts(s).some((l) => l.includes(ANSWER)));
    app.frames(10, { waitMs: 30 });
    const child = app.screenshot();

    // The dialog's Back is the last of two: the shell keeps its own.
    app.click(app.snapshot().findAll({ role: "button", label: "Back" }).at(-1));
    until("the list again", (s) => s.findAll({ role: "button", label: "Back" }).length === 1 && row(s));
    app.frames(8, { waitMs: 30 });
    nav.step("close", "button", "Close");
    app.frames(10, { waitMs: 30 });
    const finished = app.screenshot();
    keep(working, "gpui-subagent-working");
    keep(listed, "gpui-subagent-listed");
    keep(child, "gpui-subagent-child");
    keep(finished, "gpui-subagent-finished");
    expect(image.diff(listed, child)).toBeGreaterThan(0.005);
  },
);
