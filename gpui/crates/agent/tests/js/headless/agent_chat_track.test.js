// A track's chat survives navigation inside the track and a visit to the
// venue page; only an explicit New chat or a history pick changes it.

fixture({
  seconds: 4,
  clips: [{ pattern: "pattern-strobe", name: "Strobe", start: 0.5, end: 2 }],
  seeded_threads: true,
  rig: 4,
  window: [1400, 900],
});

const SEEDED = "Seeded question about score ";

test("a track keeps its chat until an explicit new or history choice", () => {
  const header = (label) => until(label, (s) => s.find({ role: "text", label }));
  const composer = (s) => s.find({ role: "input", label: "Do anything…" });
  nav.trackEditor("Test Venue", "Aurora");
  header("Luma");
  // A clip is labelled by its form; the fixture's clips play Constant color.
  until("the score's clip", (s) => s.find({ role: "card", label: "Constant color" }));
  // Picking the track opened its most recent chat, a seeded one.
  until("the track's chat", (s) => s.findAll({ role: "text" }).some((n) => n.label.startsWith(SEEDED)));
  app.type(composer(until("the composer", composer)), "keep this draft");
  nav.step("new chat", "button", "New chat");
  header("Luma");
  nav.step("history", "button", "Chat history");
  const history = until("the track's history", (s) =>
    s.findAll({ role: "card" }).some((n) => n.label.startsWith(SEEDED)));
  const saved = history.findAll({ role: "card" }).find((n) => n.label.startsWith(SEEDED));
  app.click(saved);
  header("Luma");
  until("history dismissed", (s) => !s.find({ role: "input", label: "Search chats…" }));

  // The venue page hides the thread; coming back to the same track shows the
  // same conversation, not a fresh resolve.
  nav.venuePage("Test Venue");
  until("the thread hidden", (s) => !s.findAll({ role: "text" }).some((n) => n.label === "Luma"));
  nav.step("the track again", "row", "Aurora");
  header("Luma");
  app.frames(8, { waitMs: 20 });
  assert(app.snapshot().find({ role: "text", label: saved.label }),
    "the venue page replaced the track's conversation");

  nav.step("close track", "button", "Close Aurora");
  until("the score workspace closed", (s) => !s.find({ role: "button", label: "Aurora" }));
  header("Luma");
});
