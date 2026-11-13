// The "About you" page: an account that has not been asked its two questions
// meets them once, after the terms are settled and before the app opens.
//
// `about_you_pending` names the accounts the server has yet to ask.

const NEWCOMER = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
const QUIET = "0a0a0a0a-1b1b-2c2c-3d3d-4e4e4e4e4e4e";
const RETURNING = "99999999-8888-7777-6666-555555555555";

fixture({
  accounts: {
    "newcomer@luma.test": NEWCOMER,
    "quiet@luma.test": QUIET,
    "returning@luma.test": RETURNING,
  },
  terms_pending: ["newcomer@luma.test"],
  about_you_pending: ["newcomer@luma.test", "quiet@luma.test"],
});

const gate = (s) => s.find({ role: "text", label: "Sign in to Luma" }) !== undefined;
const terms = (s) => s.find({ role: "card", label: "Terms" }) !== undefined;
const questions = (s) => s.find({ role: "text", label: "About you" }) !== undefined;
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

const signOutOfWelcome = () => {
  nav.step("the welcome page's sign-out", "button", "Sign out");
  until("the sign-in screen", gate, { timeoutMs: 15000 });
};

const leave = (s) => {
  const skip = s.find({ role: "button", label: "Skip" }) !== undefined;
  const go = s.find({ role: "button", label: "Continue" }) !== undefined;
  return skip === go ? undefined : skip ? "Skip" : "Continue";
};

test("a new account is asked after the terms, answers, and is not asked again", () => {
  signOutOfTheApp();
  signIn("newcomer@luma.test");

  // The terms come first; the questions are not on the page with them.
  const page = until("the terms page", (s) => terms(s) ? s : undefined);
  expect(questions(page)).toBe(false);
  nav.step("Accept", "button", "Accept");

  // What the person does is asked before how often they have done it.
  const asked = until("the questions", (s) => questions(s) ? s : undefined);
  expect(welcome(asked)).toBe(false);
  const role = asked.find({ role: "text", label: "What describes you? Pick any that fit." }).bounds;
  const experience = asked.find({ role: "text", label: "How often have you programmed lights?" }).bounds;
  expect(role.y).toBeLessThan(experience.y);

  // The page has one way off, and it says what it will do: with nothing
  // taken it skips, and it turns to Continue while an answer is held.
  expect(leave(asked)).toBe("Skip");
  nav.step("an answer", "toggle", "DJ");
  until("Continue", (s) => leave(s) === "Continue");
  nav.step("the same answer, let go", "toggle", "DJ");
  until("Skip again", (s) => leave(s) === "Skip");

  // More than one role fits a person; "Other" asks which.
  nav.step("a role", "toggle", "DJ");
  nav.step("a second role", "toggle", "Other");
  const named = until("the field for the other role", (s) =>
    s.find({ role: "input", label: "What do you do?" }) ? s : undefined);
  app.type(named.find({ role: "input", label: "What do you do?" }), "VJ");
  nav.step("how often", "toggle", "Often");
  nav.step("Continue", "button", "Continue");
  until("the welcome page", welcome);

  // Asked is the account's, not the visit's.
  signOutOfWelcome();
  signIn("newcomer@luma.test");
  until("the welcome page", welcome);
  expect(questions(app.snapshot())).toBe(false);
});

test("skipping lets the account in and counts as asked", () => {
  signOutOfTheApp();
  signIn("quiet@luma.test");
  until("the questions", questions);
  nav.step("Skip", "button", "Skip");
  until("the welcome page", welcome);

  signOutOfWelcome();
  signIn("quiet@luma.test");
  until("the welcome page", welcome);
  expect(questions(app.snapshot())).toBe(false);
});

test("an account that was asked before goes straight in", () => {
  signOutOfTheApp();
  signIn("returning@luma.test");
  until("the welcome page", welcome);
  expect(questions(app.snapshot())).toBe(false);
});
