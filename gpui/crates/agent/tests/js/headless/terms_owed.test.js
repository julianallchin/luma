// Quitting on the terms page is not a way past it: the account's stored
// session opens the next launch on the page again, and the app only after
// Accept.

fixture({ terms_owed: true });

const terms = (s) => s.find({ role: "card", label: "Terms" }) !== undefined;

test("a launch that still owes the terms opens on them", () => {
  const page = until("the terms page", (s) => terms(s) ? s : undefined);
  expect(page.find({ role: "card", label: "Test Venue" })).toBe(undefined);

  nav.step("Accept", "button", "Accept");
  until("the app behind the page", (s) => !terms(s) && s.find({ role: "text", label: "Loading" }) === undefined);
  nav.venue("Test Venue");
});
