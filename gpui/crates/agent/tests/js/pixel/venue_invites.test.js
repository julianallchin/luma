// Venue invites under a real renderer, for the eye: the list with its Share
// chip, the owner's Share route with a code and people, the Join route and its
// inline refusal, and onboarding's way to Join. The flows themselves are
// asserted in `headless/venue_invites.test.js`.

const OTHER = "99999999-8888-7777-6666-555555555555";
const FRIEND = "f00df00d-0000-4000-8000-000000000001";
const LATER = "c0ffee00-0000-4000-8000-000000000002";
fixture({
  window: [1100, 720],
  track: false,
  seconds: 1,
  sql: [
    `INSERT INTO venues (id, uid, name, description, share_code) VALUES ('warehouse', '${OTHER}', 'Warehouse', 'Shared with this account', 'K7M4Q9TX')`,
    `INSERT INTO venue_nodes (id, uid, venue_id, kind) VALUES ('warehouse:venue', '${OTHER}', 'warehouse', 'venue')`,
    `INSERT INTO venue_members (id, uid, venue_id, role) VALUES ('warehouse:$PRINCIPAL', '$PRINCIPAL', 'warehouse', 'member')`,
    `INSERT INTO venue_members (id, uid, venue_id, role, created_at) VALUES ('venue-main:${FRIEND}', '${FRIEND}', 'venue-main', 'member', strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-3 days'))`,
    `INSERT INTO venue_members (id, uid, venue_id, role, created_at) VALUES ('venue-main:${LATER}', '${LATER}', 'venue-main', 'member', strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-2 hours'))`,
  ],
});

const settle = () => app.frames(6, { waitMs: 40 });
const card = (s) => s.find({ role: "card", label: "Venue dialog" });
const keepCard = (name) => {
  settle();
  const s = app.snapshot();
  image.keep(app.screenshot({ node: card(s) }), name);
};

test("the list, the share route and the join route", () => {
  until("the picker", (s) => s.find({ role: "card", label: "Warehouse" }) !== undefined
    && s.find({ role: "card", label: "Test Venue" }) !== undefined);
  // The cursor on the owned venue, so its Share chip shows.
  const list = app.snapshot();
  const rows = list.findAll({ role: "card" }).filter((n) => n.label === "Test Venue" || n.label === "Warehouse");
  if (rows[0].label !== "Test Venue") app.key("down");
  keepCard("venue-invites/list");

  nav.step("the Share chip", "button", "Share Test Venue");
  until("the people", (s) => s.find({ role: "row", label: "Account f00df00d" }) !== undefined);
  keepCard("venue-invites/share-empty");

  nav.step("Create invite code", "button", "Create invite code");
  until("a code", (s) => s.find({ role: "button", label: "New code" }) !== undefined);
  keepCard("venue-invites/share-code");

  nav.step("Remove", "button", "Remove Account c0ffee00");
  until("the confirmation", (s) => s.find({ role: "button", label: "Confirm removing Account c0ffee00" }) !== undefined);
  keepCard("venue-invites/share-confirm-remove");

  nav.step("Back", "button", "Back to venues");
  nav.step("Join", "button", "Join a venue");
  until("the join route", (s) => s.find({ role: "text", label: "Join a venue" }) !== undefined);
  keepCard("venue-invites/join");

  app.type(app.snapshot().find((n) => n.role === "input" && n.focused === true), "abcd-efgh");
  app.key("enter");
  until("the refusal", (s) => s.find((n) => n.role === "text" && n.label.startsWith("No venue uses")) !== undefined);
  keepCard("venue-invites/join-error");
});

test("onboarding offers the way to join", {
  fixture: { sql: ["DELETE FROM venue_nodes WHERE venue_id = 'venue-main'", "DELETE FROM venues WHERE id = 'venue-main'"] },
}, () => {
  until("onboarding", (s) => s.find({ role: "text", label: "Light it up" }) !== undefined
    && s.find({ role: "button", label: "Join a venue" }) !== undefined);
  settle();
  image.keep(app.screenshot(), "venue-invites/onboarding");

  // Its join dialog opens over the page, the code filling in as it is typed.
  nav.step("Join", "button", "Join a venue");
  const focused = (s) => s.find((n) => n.role === "input" && n.focused === true);
  app.type(focused(until("the code field", (s) => focused(s) !== undefined)), "k7m4q");
  settle();
  image.keep(app.screenshot(), "venue-invites/onboarding-join");
});
