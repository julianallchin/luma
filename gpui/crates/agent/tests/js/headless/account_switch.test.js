// One machine, two accounts. What the shell holds belongs to the account that
// is signed in: after a sign-out, the next account to sign in finds nothing of
// the one before it — not behind the venue picker, not after Escape, not in
// the venue it goes on to open.
//
// Signing in is the app's own screen, against the fixture's `accounts` in
// Supabase's place: no code is mailed and none is checked.

const OTHER = "99999999-8888-7777-6666-555555555555";
const NEWCOMER = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
const QUESTION = "Seeded question about score 0";

fixture({
  clips: [{ pattern: "pat-wash", name: "Wash", start: 1, end: 2 }],
  seeded_threads: true,
  accounts: {
    "other@luma.test": OTHER,
    "newcomer@luma.test": NEWCOMER,
  },
  // The other account has a venue of its own; the newcomer has none.
  sql: [
    `INSERT INTO venues (id, uid, name) VALUES ('venue-other', '${OTHER}', 'Other Room')`,
    `INSERT INTO venue_nodes (id, uid, venue_id, kind)
       VALUES ('venue-other:venue', '${OTHER}', 'venue-other', 'venue')`,
  ],
});

// Everything in `s` that is the first account's: its venue, its song and the
// open score's crumb, the clip on its timeline, and its conversation.
const ofFirst = (s) => s.nodes
  .filter((n) => n.label.includes("Test Venue")
    || n.label.includes("Aurora")
    || n.label.includes(QUESTION)
    || (n.role === "card" && n.label === "Wash"))
  .map((n) => `${n.role}: ${n.label}`);

// A room on screen: the stage, or a score's timeline.
const room = (s) => s.findAll({ role: "card" })
  .filter((n) => n.label === "Stage" || n.label === "Waveform")
  .map((n) => n.label)
  .sort();

const gate = (s) => s.find({ role: "text", label: "Sign in to Luma" }) !== undefined;
const picker = (s) => s.find({ role: "card", label: "Venue dialog" }) !== undefined;

// The first account at work: its venue, a score open on the timeline, the
// stage above it and the score's conversation beside it.
const firstAtWork = () => {
  nav.trackEditor("Test Venue", "Aurora");
  const work = until("the first account's score and chat", (s) =>
    texts(s).includes(QUESTION) && s.find({ role: "card", label: "Wash" }) ? s : undefined);
  expect(nav.openScore(work, "Aurora")).toBe("Aurora · #1");
  expect(room(work)).toEqual(["Stage", "Waveform"]);
};

const signOut = () => {
  nav.venueMenu();
  nav.step("the sign-out row", "row", "Sign out");
  const out = until("the sign-in screen", (s) => gate(s) ? s : undefined, { timeoutMs: 15000 });
  expect(ofFirst(out)).toEqual([]);
};

// From the sign-in screen to whatever the app opens for `email`'s account.
const signIn = (email) => {
  app.type(app.snapshot().find({ role: "input", label: "Email" }), email);
  nav.step("the email's capsule", "button", "Continue");
  const sent = until("the code route", (s) => s.find({ role: "input", label: "Code" }) ? s : undefined);
  app.type(sent.find({ role: "input", label: "Code" }), "123456");
  nav.step("the code's capsule", "button", "Sign in");
  until("the sign-in screen to leave", (s) =>
    !gate(s) && s.find({ role: "text", label: "Check your email" }) === undefined);
};

test("an account with no venue finds nothing of the one before it", () => {
  firstAtWork();
  signOut();
  signIn("newcomer@luma.test");

  // Nothing to choose from, so the picker asks for a first venue — over an
  // empty shell, not over the last account's.
  const create = until("the first-venue form", (s) =>
    s.find({ role: "input", label: "Venue name" }) ? s : undefined);
  expect(ofFirst(create)).toEqual([]);
  expect(room(create)).toEqual([]);

  // Escape is not a way past it: there is no venue behind the form.
  nav.dismiss();
  const held = app.snapshot();
  assert(picker(held), "Escape closed the picker with no venue open");
  expect(ofFirst(held)).toEqual([]);
  expect(room(held)).toEqual([]);

  app.type(held.find({ role: "input", label: "Venue name" }), "Fresh Room");
  nav.step("the create capsule", "button", "Create venue");
  const opened = until("the newcomer's venue", (s) =>
    s.find({ role: "card", label: "Fresh Room Venue" }) && !picker(s) ? s : undefined);
  expect(ofFirst(opened)).toEqual([]);

  nav.mode("Arrange");
  const songs = until("its empty song list", (s) =>
    s.find({ role: "text", label: "No tracks imported" }) ? s : undefined);
  expect(ofFirst(songs)).toEqual([]);

  // The venue menu lists the newcomer's venues and no one else's.
  nav.venueMenu();
  const menu = until("the venue menu's venues", (s) =>
    s.find({ role: "row", label: "Fresh Room" }) ? s : undefined);
  expect(ofFirst(menu)).toEqual([]);
});

test("an account with its own venue sees only that venue", () => {
  firstAtWork();
  signOut();
  signIn("other@luma.test");

  const choose = until("the other account's venues", (s) =>
    s.find({ role: "card", label: "Other Room" }) ? s : undefined);
  expect(ofFirst(choose)).toEqual([]);
  expect(room(choose)).toEqual([]);

  nav.dismiss();
  const held = app.snapshot();
  assert(picker(held), "Escape closed the picker with no venue open");
  expect(ofFirst(held)).toEqual([]);
  expect(room(held)).toEqual([]);

  nav.venue("Other Room");
  nav.mode("Arrange");
  const songs = until("the other account's song list", (s) =>
    s.find({ role: "text", label: "No tracks imported" }) ? s : undefined);
  expect(ofFirst(songs)).toEqual([]);

  // Its Rig has a conversation panel of its own, with none of the first
  // account's in it or in its history.
  nav.venuePage("Other Room");
  nav.step("the chat history", "button", "Chat history");
  const history = until("the venue's chats", (s) =>
    s.find({ role: "input", label: "Search chats…" }) ? s : undefined);
  expect(ofFirst(history)).toEqual([]);
  nav.dismiss();

  nav.venueMenu();
  const menu = until("the venue menu's venues", (s) =>
    s.find({ role: "row", label: "Other Room" }) ? s : undefined);
  expect(ofFirst(menu)).toEqual([]);
});
