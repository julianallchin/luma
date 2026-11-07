// Rig chats about the room, over a library with no tracks in it at all.
//
// Chats belong to a mode's body: with no score open Arrange has no chat, and
// Rig shows the venue's own chat beside the venue page.

fixture({ track: false, seconds: 1, rig: 4, window: [1400, 900] });

const chat = (s) => s.find({ role: "button", label: "Chat history" }) !== undefined;

test("Rig carries the venue's chat", () => {
  nav.venue("Test Venue");
  const empty = until("the song list", (s) => s.find({ role: "card", label: "Song list" }));
  expect(chat(empty)).toBe(false);
  nav.venuePage("Test Venue");
  const page = until("the venue chat beside the page", (s) => chat(s) ? s : undefined);
  expect(page.find({ role: "input", label: "Do anything…" }) !== undefined).toBe(true);
});
