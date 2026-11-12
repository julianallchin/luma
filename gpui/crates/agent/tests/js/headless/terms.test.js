// The terms page: an account that has never accepted the terms meets them
// once, after its code is verified and before anything of the app opens.
//
// Signing in is against the fixture's `accounts`; `terms_pending` names the
// ones the server holds no acceptance for.

const NEWCOMER = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
const RETURNING = "99999999-8888-7777-6666-555555555555";

fixture({
  accounts: {
    "newcomer@luma.test": NEWCOMER,
    "returning@luma.test": RETURNING,
  },
  terms_pending: ["newcomer@luma.test"],
});

const gate = (s) => s.find({ role: "text", label: "Sign in to Luma" }) !== undefined;
const terms = (s) => s.find({ role: "card", label: "Terms" }) !== undefined;
const welcome = (s) => s.find({ role: "button", label: "Get started" }) !== undefined;

const signOutOfTheApp = () => {
  nav.venue("Test Venue");
  nav.venueMenu();
  nav.step("the sign-out row", "row", "Sign out");
  until("the sign-in screen", gate, { timeoutMs: 15000 });
};

const signIn = (email) => {
  app.type(app.snapshot().find({ role: "input", label: "Email" }), email);
  nav.step("the email's capsule", "button", "Continue");
  const sent = until("the code route", (s) => s.find({ role: "input", label: "Code" }) ? s : undefined);
  app.type(sent.find({ role: "input", label: "Code" }), "123456");
  until("the sign-in screen to leave", (s) =>
    !gate(s) && s.find({ role: "text", label: "Check your email" }) === undefined);
};

test("a new account accepts the terms once, before the app opens", () => {
  signOutOfTheApp();
  signIn("newcomer@luma.test");

  // The page is the whole window: the text, Accept, and the way out.
  const page = until("the terms page", (s) => terms(s) ? s : undefined);
  expect(welcome(page)).toBe(false);
  const box = page.find({ role: "card", label: "Terms" }).bounds;
  const accept = page.find({ role: "button", label: "Accept" }).bounds;
  expect(accept.y).toBeGreaterThan(box.y + box.height);

  nav.step("Accept", "button", "Accept");
  until("the welcome page", welcome);
  expect(terms(app.snapshot())).toBe(false);

  // Accepted is the account's, not the visit's: it is not asked again.
  nav.step("the welcome page's sign-out", "button", "Sign out");
  until("the sign-in screen", gate, { timeoutMs: 15000 });
  signIn("newcomer@luma.test");
  until("the welcome page", welcome);
  expect(terms(app.snapshot())).toBe(false);
});

test("leaving the terms page accepts nothing", () => {
  signOutOfTheApp();
  signIn("newcomer@luma.test");
  until("the terms page", terms);

  nav.step("the page's sign-out", "button", "Sign out");
  until("the sign-in screen", gate, { timeoutMs: 15000 });

  signIn("newcomer@luma.test");
  until("the terms page again", terms);
});

test("an account that has accepted never sees the page", () => {
  signOutOfTheApp();
  signIn("returning@luma.test");
  until("the welcome page", welcome);
  expect(terms(app.snapshot())).toBe(false);
});
