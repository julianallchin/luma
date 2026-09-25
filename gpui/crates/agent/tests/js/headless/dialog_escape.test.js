// Escape closes every dialog, including one that moves the keyboard to a
// control of its own after opening.
//
// The pattern picker leaves focus on the host's trap handle; `AddTracks`
// routes it onward to its search field. Both sit under the overlay's
// `key_context`, so `escape` has to resolve the same way — "the dialog took
// the keyboard" and "the dialog can still be dismissed by it" are different
// claims.

fixture({ seconds: 1 });

test("escape dismisses a dialog that routed focus to its own text field", () => {
  nav.venue("Test Venue");
  nav.step("the add-track affordance", "button", "Add track");
  const opened = until("the add-tracks search field to take the keyboard", (s) =>
    s.find({ role: "input", label: "Search all tracks…" })?.focused === true);
  // The field inside the dialog holds focus, so the dialog's card reads as
  // focused: the keyboard stayed inside the host's trap.
  expect(opened.find({ role: "card", label: "Add tracks dialog" })?.focused).toBe(true);

  app.key("escape");
  until("the dialog to close", (s) => s.find({ role: "card", label: "Add tracks dialog" }) === undefined);
});
