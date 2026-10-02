// A venue's tab set: a tab per score, and the venue tab.
//
// `tabs.rs` and `workspace.rs` prove the sets as pure logic. This proves the
// wiring: the sidebar opens tabs, a tab owns its chat, and the strip spans the
// chat and the editor (`docs/specs/venue-tabs.md`).

fixture({
  seconds: 20,
  clips: [{ pattern: "pattern-strobe", name: "Strobe", start: 2, end: 6 }],
  extra_scores: 1,
  rig: 4,
  window: [1600, 900],
});

const chips = (s) => s.findAll({ role: "button" })
  .map((n) => n.label)
  .filter((label) => label.startsWith("Aurora · #") || label === "Venue");
const scoreRows = (s) => s.findAll({ role: "row" }).filter((n) => n.label.startsWith("#"));
const chat = (s) => s.findAll({ role: "text" }).some((n) => n.label === "Luma");

// Opening a second score of the same track opens a second tab beside the
// first, rather than swapping the score inside one track's tab.
test("two scores of one track are two tabs", () => {
  nav.venue("Test Venue");
  nav.scores("Aurora");
  const listed = until("both scores", (s) => scoreRows(s).length === 2 ? s : undefined);
  app.click(scoreRows(listed)[0]);
  until("the first score's tab", (s) => chips(s).length === 1);
  // Opening a score hides the sidebar, a double-click interval after the
  // click; bring the launcher back.
  const sidebarAway = (s) => s.find({ role: "card", label: "Sidebar" }) === undefined;
  until("the sidebar put away", sidebarAway);
  app.action("luma::ToggleSidebar");
  app.frames(4);
  app.click(scoreRows(app.snapshot())[1]);
  const both = until("the second score's tab", (s) => chips(s).length === 2 ? s : undefined);
  expect(chips(both).sort()).toEqual(["Aurora · #1", "Aurora · #2"]);
  // The strip spans the chat: the front tab's chat is on screen beside it.
  assert(chat(both), "the score tab has no chat");

  // A click on a score that already has a tab brings it forward.
  until("the sidebar put away", sidebarAway);
  app.action("luma::ToggleSidebar");
  app.frames(4);
  app.click(scoreRows(app.snapshot())[0]);
  app.frames(4);
  expect(chips(app.snapshot()).length).toBe(2);
});

// The venue tab sits in the same strip as the score tabs and shows the
// venue's chat beside the patch page.
test("the venue tab is a tab with its own chat", () => {
  nav.trackEditor("Test Venue", "Aurora");
  until("Aurora's tab", (s) => chips(s).length === 1);

  nav.venuePage("Test Venue");
  const page = until("the venue tab", (s) => chips(s).includes("Venue") ? s : undefined);
  expect(chips(page).length).toBe(2);
  assert(chat(page), "the venue tab hid its chat");
  assert(page.find({ role: "card", label: "Tab strip" }) !== undefined, "the venue tab drew no strip");

  // Back to the score: its tab was parked behind the venue tab, not closed.
  nav.step("Aurora's chip", "button", chips(page).find((label) => label !== "Venue"));
  until("Aurora's timeline", (s) => s.find({ role: "card", label: "Waveform" }) !== undefined);
});
