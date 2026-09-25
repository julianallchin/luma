// A chat belongs to its score: choosing another score shows that score's
// chat, and the history lists only the open score's chats.

fixture({
  seconds: 4,
  clips: [{ pattern: "pat-glow", name: "Glow", start: 1, end: 2 }],
  extra_scores: 1,
  seeded_threads: true,
});

const SEEDED = "Seeded question about score ";

test("switching score switches to that score's chat", () => {
  const seeded = (s) => s.findAll({ role: "text" }).find((n) => n.label.startsWith(SEEDED));
  nav.trackEditor("Test Venue", "Aurora");
  const first = seeded(until("the open score's chat", seeded)).label;

  nav.step("history", "button", "Chat history");
  const history = until("this score's chats", (s) =>
    s.findAll({ role: "card" }).some((n) => n.label.startsWith(SEEDED)));
  expect(history.findAll({ role: "card" }).filter((n) => n.label.startsWith(SEEDED)).length).toBe(1);
  nav.dismiss();
  until("history dismissed", (s) => !s.find({ role: "input", label: "Search chats…" }));

  // The ordinal the editor shows, as the scores list spells it.
  const opened = app.snapshot().findAll({ role: "text" }).find((n) => n.label.startsWith("Score #")).label;
  nav.scores("Aurora");
  const other = app.snapshot().findAll({ role: "row" })
    .find((n) => n.label.startsWith("#") && !n.label.startsWith(`${opened.slice("Score ".length)} `));
  app.click(other);
  until("the other score's chat", (s) => {
    const text = seeded(s);
    return text !== undefined && text.label !== first;
  });
});
