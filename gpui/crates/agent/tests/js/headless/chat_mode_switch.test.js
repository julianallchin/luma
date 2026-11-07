// A mode's chat on screen holds still: switching modes swaps which mode's
// chat the agent panel shows, and neither the one that arrives nor the one
// that comes back moves its composer or its transcript once it is drawn.
//
// Motion is on: what broke was a layout animated from a guess.

const PARAGRAPHS = Array.from(
  { length: 40 },
  (_, i) => `Paragraph ${i + 1}: the wash holds on the downbeat and the movers sweep back across the rig.`,
);
const REPLY = PARAGRAPHS.join("\n\n");

fixture({
  window: [1400, 900],
  rig: 2,
  motion: true,
  model: [[{ text: REPLY }, { end: "end_turn" }]],
});

// Where the transcript is scrolled, read off the text it shows: the first
// paragraph whose top is in the conversation, and that top.
const anchor = (s) => {
  const conversation = s.find({ role: "card", label: "Conversation" })?.bounds;
  if (!conversation) return undefined;
  const shown = s.findAll({ role: "text" })
    .filter((n) => n.label.startsWith("Paragraph ") && n.bounds.height > 0
      && n.bounds.y >= conversation.y && n.bounds.y < conversation.y + conversation.height)
    .sort((a, b) => a.bounds.y - b.bounds.y);
  return shown[0] && { label: shown[0].label.split(":")[0], y: shown[0].bounds.y };
};

// The chat's geometry in one frame, or undefined while the panel shows no
// composer.
const layout = (s) => {
  const field = composer(s)?.bounds;
  // An unplaced composer is laid out in a box of no height: not on screen.
  if (!field || field.width === 0 || field.height === 0) return undefined;
  return {
    anchor: anchor(s),
    composer: field,
    send: s.find({ role: "button", label: "Send" })?.bounds,
  };
};

// Every frame drawn since `frame` that shows a composer, after `settle` more.
const shownSince = (frame, settle = 20) => {
  app.frames(settle, { waitMs: 16 });
  return app.painted().filter((s) => s.frame >= frame).map(layout).filter((l) => l !== undefined);
};

const sendLongReply = () => {
  until("the composer", (s) => layout(s) !== undefined);
  app.frames(20, { waitMs: 16 });
  app.type(composer(app.snapshot()), "talk me through the rig");
  app.click(app.snapshot().find({ role: "button", label: "Send" }));
  until("the reply", (s) => !busy(s) && texts(s).some((l) => l.startsWith("Paragraph 40")));
  app.frames(10, { waitMs: 16 });
};

test("a mode's chat shows its composer at rest from its first frame", () => {
  nav.trackEditor("Test Venue", "Aurora");
  until("the composer", (s) => layout(s) !== undefined);
  // Live's chat has never been on screen: this is its first frame.
  const { frame } = app.action("luma::ShowLive");
  const shown = shownSince(frame);
  assert(shown.length > 1, "too few frames with the composer to see it move");
  const rest = shown[shown.length - 1];
  for (const l of shown) {
    expect(l.composer).toEqual(rest.composer);
    expect(l.send).toEqual(rest.send);
  }
});

test("Rig's thread keeps its scroll and its composer across Rig, Arrange, Live, Rig", () => {
  nav.venue("Test Venue");
  nav.venuePage("Test Venue");
  sendLongReply();

  // Up from the end, to somewhere a reset would not land by accident.
  const conversation = app.snapshot().find({ role: "card", label: "Conversation" }).bounds;
  app.scroll({ x: conversation.x + conversation.width / 2, y: conversation.y + conversation.height / 2 },
    { dy: 600, steps: 6 });
  app.frames(6);
  const before = layout(app.snapshot());
  assert(before.anchor !== undefined, "no paragraph on screen to read the scroll from");
  assert(before.anchor.label !== "Paragraph 1", "the scroll went all the way to the top");

  // Arrange has no score open, so it is the song list and the agent panel
  // stands aside; Live brings it back with Live's own chat.
  for (const mode of ["Arrange", "Live"]) {
    app.action(`luma::Show${mode}`);
    app.frames(20, { waitMs: 16 });
  }
  const { frame } = app.action("luma::ShowRig");
  const shown = shownSince(frame);
  assert(shown.length > 1, "too few frames with the composer to see it move");
  for (const l of shown) expect(l).toEqual(before);
});
