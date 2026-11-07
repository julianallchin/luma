// The agent panel's header floats over the transcript's head: what scrolls up
// passes under it and dissolves, the header's icons stay on top of it and
// take the pointer, and the conversation's first message can still be
// scrolled clear of it.

const PARAGRAPHS = Array.from(
  { length: 40 },
  (_, i) => `Paragraph ${i + 1}: the wash holds on the downbeat and the movers sweep back across the rig.`,
);
const PROMPT = "talk me through the rig";

fixture({
  window: [1400, 900],
  rig: 2,
  model: [
    [{ text: PARAGRAPHS.join("\n\n") }, { end: "end_turn" }],
    [{ text: "The movers are parked." }, { end: "end_turn" }],
  ],
});

const header = (s) => s.find({ role: "card", label: "Chat header" })?.bounds;
const conversation = (s) => s.find({ role: "card", label: "Conversation" })?.bounds;
const paragraphs = (s) => s.findAll({ role: "text" })
  .filter((n) => n.label.startsWith("Paragraph ") && n.bounds.height > 0);
const prompt = (s) => s.findAll({ role: "text" }).find((n) => n.label === PROMPT && n.bounds.height > 0);
const overlaps = (a, b) => a.y < b.y + b.height && b.y < a.y + a.height;

function sendLongReply() {
  nav.venue("Test Venue");
  nav.venuePage("Test Venue");
  const field = until("the composer", (s) => {
    const node = composer(s);
    return node && node.bounds.height > 0 ? s : undefined;
  });
  app.type(composer(field), PROMPT);
  app.click(app.snapshot().find({ role: "button", label: "Send" }));
  until("the reply", (s) => !busy(s) && paragraphs(s).some((n) => n.label.startsWith("Paragraph 40")));
  app.frames(4);
}

// Scroll the transcript by `dy` (positive is toward its head) from its middle.
function scroll(dy) {
  const box = conversation(app.snapshot());
  app.scroll({ x: box.x + box.width / 2, y: box.y + box.height / 2 }, { dy, steps: 6 });
  app.frames(6);
  return app.snapshot();
}

test("the header's icons sit over the transcript and take the pointer", () => {
  sendLongReply();
  const shot = scroll(600);
  const band = header(shot);
  const box = conversation(shot);
  // The header is over the transcript, not above it: the conversation runs
  // up under it, and a paragraph is passing under it right now.
  assert(box.y <= band.y, `the conversation starts below the header: ${JSON.stringify({ box, band })}`);
  assert(paragraphs(shot).some((n) => overlaps(n.bounds, band)),
    "no paragraph scrolls under the header");
  const fresh = shot.find({ role: "button", label: "New chat" });
  assert(overlaps(fresh.bounds, band), "New chat is not in the header");
  // A press on the icon is the icon's, not the text's under it.
  app.click(fresh);
  until("a new conversation", (s) => paragraphs(s).length === 0 && !prompt(s));
});

// The tail is the head's mirror: text scrolled down under the composer
// dissolves above it, and nothing of the transcript shows beside or below
// the pill. The tree lists covered text too, so what is asserted is that the
// cover spans the conversation's width from above the composer to its bottom
// while text is actually passing under it.
test("no transcript text shows beside or below the composer", () => {
  sendLongReply();
  const shot = scroll(600);
  const box = conversation(shot);
  const field = composer(shot).bounds;
  const tail = shot.find({ role: "card", label: "Transcript tail" })?.bounds;
  assert(tail !== undefined, "nothing covers the transcript's tail");
  assert(paragraphs(shot).some((n) => n.bounds.y + n.bounds.height > field.y + field.height),
    "no text passes below the composer, so nothing is tested");
  assert(tail.y < field.y, `the cover starts below the composer's top: ${JSON.stringify({ tail, field })}`);
  assert(tail.y + tail.height >= box.y + box.height - 0.5, "the cover stops short of the panel's bottom");
  assert(tail.x <= box.x + 0.5 && tail.x + tail.width >= box.x + box.width - 0.5,
    `the cover leaves the gutters beside the composer open: ${JSON.stringify({ tail, box })}`);
});

test("the first message scrolls fully clear of the header", () => {
  sendLongReply();
  const top = scroll(100000);
  const band = header(top);
  const first = prompt(top);
  assert(first !== undefined, "the first message is not on screen at the top of the transcript");
  assert(first.bounds.y >= band.y + band.height,
    `the first message rests under the header: ${JSON.stringify({ first: first.bounds, band })}`);
});

// A sent prompt lands at the head of what the reader can see, which starts
// below the header, not under it.
test("a sent prompt lands below the header", () => {
  sendLongReply();
  const AGAIN = "park the movers";
  app.type(composer(app.snapshot()), AGAIN);
  app.click(app.snapshot().find({ role: "button", label: "Send" }));
  const landed = until("the second reply", (s) =>
    !busy(s) && !s.find({ role: "card", label: "Sending message" })
      && s.findAll({ role: "text" }).some((n) => n.label === "The movers are parked.") ? s : undefined);
  app.frames(10);
  const shot = app.snapshot();
  const band = header(shot);
  const sent = shot.findAll({ role: "text" }).find((n) => n.label === AGAIN && n.bounds.height > 0);
  assert(sent !== undefined, `the sent prompt is not on screen: ${landed.frame}`);
  assert(sent.bounds.y >= band.y + band.height,
    `the sent prompt landed under the header: ${JSON.stringify({ sent: sent.bounds, band })}`);
});
