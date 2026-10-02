// A tab's status dot (`docs/specs/venue-tabs.md` rule 6): working while its
// chat's turn runs, finished when the turn ended behind another tab, and
// cleared by viewing the tab. The turn keeps running while another tab is in
// front.

const chunks = (text) => text.match(/[\s\S]{1,24}/g).map((chunk) => ({ text: chunk }));

fixture({
  window: [1400, 900],
  model_cadence_ms: 25,
  model: [
    [
      ...chunks("Sampling the ramp first."),
      { call: "python", id: "call-1", args: { verb: "Checking", verbPast: "Checked", purpose: "ramp", code: "ramp.peak()" } },
      { end: "tool_use" },
    ],
    [...chunks("It peaks at bar 3."), { end: "end_turn" }],
  ],
  tools: [
    {
      name: "python",
      // Holds the turn open while the test moves to another tab.
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
      result: { status: "ok", stdout: "peak=3\n", stderr: "", repr: "3", traceback: null, notices: [], figures: [], durationMs: 10 },
    },
  ],
});

const has = (s, label) => s.find({ role: "text", label }) !== undefined;

test("a turn's dot works, finishes behind another tab, and clears when seen", () => {
  nav.trackEditor("Test Venue", "Aurora");
  const tab = app.snapshot().findAll({ role: "button" }).find((n) => n.label.startsWith("Aurora · #")).label;
  nav.step("new conversation", "button", "New chat");
  const ready = until("the composer", (s) => s.find({ role: "button", label: "New chat" })?.enabled ? s : undefined);
  app.type(ready.find({ role: "input", label: "Do anything…" }), "where does the ramp peak?");
  app.click(app.snapshot().find({ role: "button", label: "Send" }));
  until("the working dot", (s) => has(s, `${tab} working`));

  // Another tab in front: the turn keeps running and ends behind it.
  nav.venuePage("Test Venue");
  until("the finished dot", (s) => has(s, `${tab} finished`));

  // Viewing the tab clears it.
  nav.step("the score tab", "button", tab);
  until("the dot cleared", (s) => !has(s, `${tab} finished`) && !has(s, `${tab} working`));
});
