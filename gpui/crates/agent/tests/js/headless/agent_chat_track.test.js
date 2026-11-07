// An open score's chat survives navigation inside the track and a visit to
// Rig; only an explicit New chat or a history pick changes it.

fixture({
  seconds: 4,
  clips: [{ pattern: "pattern-strobe", name: "Strobe", start: 0.5, end: 2 }],
  seeded_threads: true,
  rig: 4,
  window: [1400, 900],
});

const SEEDED = "Seeded question about score ";

test("a track keeps its chat until an explicit new or history choice", () => {
  const header = () => until("the chat header", (s) => s.find({ role: "button", label: "Chat history" }));
  nav.trackEditor("Test Venue", "Aurora");
  header();
  // A clip is labelled by its name; the fixture's clip is a Wash.
  until("the score's clip", (s) => s.find({ role: "card", label: "Wash" }));
  // Picking the track opened its most recent chat, a seeded one.
  until("the track's chat", (s) => s.findAll({ role: "text" }).some((n) => n.label.startsWith(SEEDED)));
  app.type(composer(until("the composer", composer)), "keep this draft");
  nav.step("new chat", "button", "New chat");
  header();
  nav.step("history", "button", "Chat history");
  const history = until("the track's history", (s) =>
    s.findAll({ role: "card" }).some((n) => n.label.startsWith(SEEDED)));
  const saved = history.findAll({ role: "card" }).find((n) => n.label.startsWith(SEEDED));
  app.click(saved);
  header();
  until("history dismissed", (s) => !s.find({ role: "input", label: "Search chats…" }));

  // Rig shows the venue's own chat; coming back to Arrange
  // shows the same conversation, not a fresh resolve.
  nav.venuePage("Test Venue");
  until("the venue's chat", (s) => !s.find({ role: "text", label: saved.label }));
  nav.mode("Arrange");
  header();
  app.frames(8, { waitMs: 20 });
  assert(app.snapshot().find({ role: "text", label: saved.label }),
    "Rig replaced the score's conversation");

  header();
});
