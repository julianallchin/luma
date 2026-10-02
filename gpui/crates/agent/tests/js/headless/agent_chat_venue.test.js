// The venue tab chats about the room, over a library with no tracks in it at
// all.
//
// Chats belong to a tab: with no tab open there is no chat, and the venue tab
// shows the venue's own chat beside the patch page.

fixture({ track: false, seconds: 1, rig: 4, window: [1400, 900] });

const chat = (s) => s.findAll({ role: "text" }).some((n) => n.label === "Luma");

test("the venue tab carries the venue's chat", () => {
  nav.venue("Test Venue");
  const empty = until("the empty panel", (s) => s.find({ role: "card", label: "Empty panel" }));
  expect(chat(empty)).toBe(false);
  nav.venuePage("Test Venue");
  const page = until("the venue chat beside the page", (s) => chat(s) ? s : undefined);
  expect(page.find({ role: "input", label: "Do anything…" }) !== undefined).toBe(true);
});
