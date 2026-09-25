// Delegation, as the surfaces a reader touches: the transcript chip, the
// floating pill, the dialog it opens, the child's read-only transcript, and
// the dialog's keyboard.
//
// Only the model is scripted. The parent's `subagent` call is the shipped
// tool: the child is a real thread running a real turn on a private
// workspace, and its work is really merged. The model is one queue shared by
// both threads: the parent delegates, the child runs a slow tool (which is
// what makes "1 subagent working" observable at all), the child answers, and
// the parent closes.
//
// Motion is on for the reason in chat.test.js.

const DESCRIPTION = "Fitting the chorus ramp";
const ANSWER = "Raised the ramp two bars early.";

fixture({
  window: [1280, 900],
  motion: true,
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
      latency_ms: 1200,
      result: { status: "ok", stdout: "", stderr: "", repr: null, traceback: null, notices: [], figures: [], durationMs: 12 },
    },
    "subagent",
  ],
});

const texts = (s) => s.findAll({ role: "text" }).map((n) => n.label);
const chips = (s) => s.findAll({ role: "chip" }).map((n) => n.label);
const busy = (s) => texts(s).some((label) => label === "Working" || label === "Sending");
const button = (label) => app.snapshot().find({ role: "button", label });
const PILL = "1 subagent working";
const row = (s) => s.findAll({ role: "card" }).find((n) => n.label.includes(DESCRIPTION));

// Open the track's new conversation, send, and wait for the turn to begin.
const delegate = () => {
  nav.trackEditor("Test Venue", "Aurora");
  nav.step("new conversation", "button", "New chat");
  until("the chat centre", (s) => {
    const send = s.find({ role: "button", label: "Send" });
    return send !== undefined && send.bounds.width > 0 && s.find({ role: "button", label: "New chat" })?.enabled;
  });
  app.type(app.snapshot().find({ role: "input", label: "Do anything…" }), "move the ramp");
  app.click(button("Send"));
  until("the turn to begin", busy);
};

test("a delegation reads as a pill, a count and a read-only thread", () => {
  delegate();

  // While the call has no output the chip narrates in the present tense, on
  // a trailing line of its own.
  const working = until("the delegation chip", (s) => chips(s).some((c) => c.includes(DESCRIPTION)));
  assert(chips(working).some((c) => c.startsWith("Started subagent")), "the running delegation is not a started subagent");
  expect(texts(working)).toContain("started working");

  // The pill is live state: it exists only while a child is in flight.
  nav.step("the subagent pill", "button", PILL);
  const listed = until("the subagents dialog", row).findAll({ role: "card" })
    .filter((n) => n.label.includes(DESCRIPTION));
  expect(listed.length).toBe(1);

  // The reader seats the transcript the child has when it opens, so let the
  // delegation finish first.
  until("the turn end", (s) => !busy(s));

  // The shell's composer is behind the scrim and still in the tree, so "no
  // composer" is a claim about what the reader adds.
  const sendsOutside = app.snapshot().findAll({ role: "button", label: "Send" }).length;
  app.click(row(app.snapshot()));
  const inside = until("the child's transcript", (s) => texts(s).some((l) => l.includes(ANSWER)));
  expect(inside.findAll({ role: "button", label: "Send" }).length).toBe(sendsOutside);
  // The child's chips render through the same rail as the parent's.
  expect(chips(inside)).toContain("Measured ramp bounds");

  // Back lands on the list: the shell's own Back survives, the dialog's goes.
  app.click(button("Back"));
  until("the list again", (s) => s.findAll({ role: "button", label: "Back" }).length === 1 && row(s));
  app.click(button("Close"));
  const closed = until("the dialog to close", (s) => !s.find({ role: "button", label: "Close" }));
  assert(chips(closed).some((c) => c.startsWith("Finished subagent")), "the settled delegation is not a finished subagent");
  expect(texts(closed)).toContain("finished working");
});

// Escape steps a child's transcript back to the list before it closes the
// list, and the arrows walk the rows. Both only run while the focus path is
// inside the card, so this pins the routing as much as the behaviour.
test("escape steps the subagents dialog back before it closes it", () => {
  delegate();
  nav.step("the subagent pill", "button", PILL);
  const list = until("the subagents dialog", row);
  expect(row(list).focused).toBe(true);
  until("the turn end", (s) => !busy(s));

  // Right opens the focused row, the same gesture Enter is bound to.
  app.key("right");
  until("the child's transcript", (s) => texts(s).some((l) => l.includes(ANSWER)));
  // The route change unmounts the row the keyboard was on; the shell has to
  // re-seat it, or nothing below reaches a handler.
  expect(app.snapshot().find({ role: "card", label: "Subagents dialog" })?.focused).toBe(true);

  app.key("escape");
  const back = until("the list again", row);
  assert(back.find({ role: "button", label: "Close" }), "escape closed the whole dialog instead of stepping back");

  app.key("escape");
  until("the dialog to close", (s) => !s.find({ role: "button", label: "Close" }));
});
