// Sending, as motion: the prompt lifts from the composer, lands near the top
// of the conversation and stays there while the reply fills the room. The
// thinking trailer waits for the prompt to land and does not move after.
//
// Three turns, one per send: a short reply; one that keeps thinking after its
// text stops growing (empty deltas), so the trailer is observable standing
// still; and one long enough to overflow the room.

const LINE = "A softly lit stage. ";

fixture({
  window: [1280, 800],
  motion: true,
  model_cadence_ms: 35,
  model: [
    Array(16).fill({ text: LINE }),
    [...Array(16).fill({ text: LINE }), ...Array(100).fill({ text: "" })],
    [{ text: "A softly lit stage.\n\n".repeat(100) }],
  ],
});

const texts = (s) => s.findAll({ role: "text" }).map((n) => n.label);
const busy = (s) => texts(s).some((label) => label === "Working" || label === "Sending");
const flying = (s) => s.find({ role: "card", label: "Sending message" });
// A turn in flight turns Send into Steer, so Send is the idle composer.
const idle = (s) => !busy(s) && s.find({ role: "button", label: "Send" }) !== undefined;

const send = (text) => {
  app.type(app.snapshot().find({ role: "input", label: "Do anything…" }), text);
  app.click(app.snapshot().find({ role: "button", label: "Send" }));
};

// In the app shell the composer overlays the list's tail, so the prompt's
// landing has to leave room above the composer, not the whole list height.
test("in the app shell a sent prompt stays on screen once it lands", () => {
  nav.trackEditor("Test Venue", "Aurora");
  nav.step("new conversation", "button", "New chat");
  until("the composer", (s) => {
    const send = s.find({ role: "button", label: "Send" });
    return send && send.bounds.width > 0 && s.find({ role: "button", label: "New chat" })?.enabled;
  });

  send("Light the stage");
  until("the first reply", (s) => idle(s) && texts(s).some((l) => l.includes("softly lit")));

  send("Make it warmer");
  const start = flying(until("the prompt in flight", flying));
  // Thinking waits for the landing: no trailer while the prompt is flying.
  let thoughtEarly = false;
  const middle = flying(until("the prompt to lift", (s) => {
    thoughtEarly ||= flying(s) !== undefined && busy(s);
    return flying(s) && flying(s).bounds.y < start.bounds.y;
  }));
  expect(middle.bounds.y).toBeLessThan(start.bounds.y);
  const settled = until("the message to land", (s) => {
    thoughtEarly ||= flying(s) !== undefined && busy(s);
    return !flying(s);
  });
  assert(!thoughtEarly, "thinking appeared before the message landed");

  // It lands near the top of the conversation, not at the bottom.
  const viewport = settled.find({ role: "card", label: "Conversation" }).bounds;
  const prompt = settled.findAll({ role: "text" }).find((n) => n.label === "Make it warmer");
  assert(prompt, "the landed prompt is not on screen");
  expect(prompt.bounds.y - viewport.y).toBeLessThan(viewport.height / 4);

  // The trailer appears after the landing and holds still while the model
  // keeps thinking.
  const thinking = until("thinking", (s) => s.find({ role: "text", label: "Working" }))
    .find({ role: "text", label: "Working" });
  const later = until("a later frame", (s) => s.frame > thinking.frame + 5);
  const still = later.find({ role: "text", label: "Working" });
  assert(still, "thinking stopped before its text did");
  expect(Math.abs(still.bounds.y - thinking.bounds.y)).toBeLessThan(0.5);
  until("the second reply", idle);

  // A reply taller than the room still lands its prompt.
  send("Show every cue");
  until("the long reply", (s) => !flying(s) && idle(s) && texts(s).some((l) => l.includes("softly lit")));
});
