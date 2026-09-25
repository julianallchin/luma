// A venue has no chat, over a library with no tracks in it at all.
//
// With no score open the thread says how to start one, and the venue page
// takes the thread's room instead of pointing it at the room.

fixture({ track: false, seconds: 1, rig: 4, window: [1400, 900] });

const HEADLINE = "Pick a track to chat";

test("the thread waits for a score and the venue page hides it", () => {
  nav.venue("Test Venue");
  const idle = until("the unattached thread", (s) => s.find({ role: "text", label: HEADLINE }));
  expect(idle.find({ role: "button", label: "Send" })).toBe(undefined);
  nav.venuePage("Test Venue");
  const page = until("the thread collapsed", (s) => !s.findAll({ role: "text" }).some((n) => n.label === "Luma"));
  expect(page.find({ role: "text", label: HEADLINE })).toBe(undefined);
});
