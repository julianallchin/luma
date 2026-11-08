// A Python call made while the app is still setting Python up says so on its
// chip, rather than claim the model's verb for minutes, and reads as an
// ordinary running call once setup is done.
//
// Only the model is scripted. The `python` stand-in reports its wait for setup
// the way the shipped tool does, through the turn's live progress.

const PURPOSE = "ramp bounds";

fixture({
  window: [1280, 900],
  model_cadence_ms: 25,
  model: [
    [{
      call: "python",
      id: "call-py",
      args: { verb: "Measuring", verbPast: "Measured", purpose: PURPOSE, code: "ramp.bounds()" },
    }],
    [{ text: "The ramp spans bars 3 to 5." }],
  ],
  tools: [
    {
      name: "python",
      setup_wait_ms: 2000,
      latency_ms: 1500,
      result: { status: "ok", stdout: "", stderr: "", repr: null, traceback: null, notices: [], figures: [], durationMs: 12 },
    },
  ],
});

const waiting = (s) => chips(s).some((c) => c.includes("Python setup") && c.includes(PURPOSE));

test("a python call waiting for setup says so on its chip", () => {
  nav.trackEditor("Test Venue", "Aurora");
  nav.step("new conversation", "button", "New chat");
  until("the chat centre", (s) => {
    const send = s.find({ role: "button", label: "Send" });
    return send !== undefined && send.bounds.width > 0 && s.find({ role: "button", label: "New chat" })?.enabled;
  });
  app.type(app.snapshot().find({ role: "input", label: "Do anything…" }), "where does the ramp start?");
  app.click(app.snapshot().find({ role: "button", label: "Send" }));
  until("the turn to begin", busy);

  const held = until("the chip waiting for setup", waiting);
  assert(!chips(held).some((c) => c.startsWith("Measuring")), "the waiting call claims the model's verb");

  // Setup done, the same call runs and then settles.
  until("the call running", (s) => chips(s).includes(`Measuring ${PURPOSE}`) && !waiting(s));
  const settled = until("the turn end", (s) => !busy(s));
  expect(chips(settled)).toEqual([`Measured ${PURPOSE}`]);
});
