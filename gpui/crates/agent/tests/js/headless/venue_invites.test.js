// Venue invites through the production picker: the owner's Share route and
// anyone's Join route.
//
// The harness has no server, so the app redeems codes against its own rows
// (`LocalShareCodes`): the fixture seeds a venue another account owns with a
// code on it, which this account cannot open until it joins, and a member in
// the fixture's own venue for the owner to see and remove.

const OTHER = "99999999-8888-7777-6666-555555555555";
const FRIEND = "f00df00d-0000-4000-8000-000000000001";
fixture({
  track: false,
  seconds: 1,
  sql: [
    `INSERT INTO venues (id, uid, name, share_code) VALUES ('warehouse', '${OTHER}', 'Warehouse', 'K7M4Q9TX')`,
    `INSERT INTO venue_nodes (id, uid, venue_id, kind) VALUES ('warehouse:venue', '${OTHER}', 'warehouse', 'venue')`,
    `INSERT INTO venue_members (id, uid, venue_id, role) VALUES ('venue-main:${FRIEND}', '${FRIEND}', 'venue-main', 'member')`,
  ],
});

const CODE = /^[2-9A-HJKMNP-TV-Z]{4}-[2-9A-HJKMNP-TV-Z]{4}$/;
const codeOn = (s) => s.find((n) => n.role === "text" && CODE.test(n.label));

test("the owner mints, replaces and revokes a code and removes a member", () => {
  // The picker opens at launch, and the venue another account owns is not
  // on it: a code has not let this account in yet.
  const picker = until("the picker", (s) => s.find({ role: "card", label: "Test Venue" }) ? s : undefined);
  expect(picker.find({ role: "card", label: "Warehouse" })).toBe(undefined);

  nav.step("the Share chip", "button", "Share Test Venue");
  until("the share route", (s) => s.find({ role: "text", label: "Share Test Venue" }) !== undefined
    && s.find({ role: "text", label: "No invite code" }) !== undefined);
  const people = until("the people with access", (s) =>
    s.find({ role: "row", label: "Account f00df00d" }) ? s : undefined);
  assert(people.find({ role: "row", label: "You" }), "the owner is not on their own list");

  nav.step("Create invite code", "button", "Create invite code");
  const first = codeOn(until("a code", (s) => codeOn(s) !== undefined)).label;

  nav.step("New code", "button", "New code");
  until("a new code", (s) => codeOn(s) !== undefined && codeOn(s).label !== first);

  // Remove asks once, then removes.
  nav.step("Remove", "button", "Remove Account f00df00d");
  nav.step("Confirm", "button", "Confirm removing Account f00df00d");
  until("the member to go", (s) => s.find({ role: "row", label: "Account f00df00d" }) === undefined
    && s.find({ role: "text", label: "Nobody else has access yet." }) !== undefined);

  nav.step("Revoke", "button", "Revoke code");
  until("the code to go", (s) => s.find({ role: "text", label: "No invite code" }) !== undefined);

  // Back is the list, not a closed dialog.
  nav.step("Back", "button", "Back to venues");
  until("the list", (s) => s.find({ role: "card", label: "Test Venue" }) !== undefined
    && s.find({ role: "input", label: "Search venues…" }) !== undefined);
});

test("a code joins a venue, and a bad one says why inline", () => {
  until("the picker", (s) => s.find({ role: "card", label: "Test Venue" }) !== undefined);
  nav.step("Join", "button", "Join a venue");
  const focusedInput = (s) => s.find((n) => n.role === "input" && n.focused === true);
  const field = () => focusedInput(until("the code field", (s) => focusedInput(s) !== undefined));
  until("the join route", (s) => s.find({ role: "text", label: "Join a venue" }) !== undefined);

  app.type(field(), "hello");
  app.key("enter");
  until("not a code", (s) => s.find((n) => n.role === "text" && n.label.startsWith("That isn't an invite code")) !== undefined);

  app.key("cmd-a backspace");
  app.type(field(), "abcd-efgh");
  app.key("enter");
  const refused = until("no such venue", (s) => s.find((n) => n.role === "text" && n.label.startsWith("No venue uses ABCD-EFGH")) !== undefined);
  // Enter is the picker's, not the field's: the code stays on its one line.
  expect(refused.find((n) => n.role === "input").label).toBe("abcd-efgh");

  // Typed the way a person types it off a message.
  app.key("cmd-a backspace");
  app.type(field(), "k7m4 q9tx");
  app.key("enter");
  until("the joined venue to open", (s) =>
    s.find({ role: "card", label: "Venue dialog" }) === undefined
      && s.find({ role: "button", label: "Warehouse" }) !== undefined, { timeoutMs: 10000 });

  // On the venue menu now, with no way to share it.
  nav.venueMenu();
  const listed = until("the list", (s) => s.find({ role: "row", label: "Warehouse" }) ? s : undefined);
  expect(listed.findAll({ role: "row" }).find((n) => n.label.startsWith("Share "))).toBe(undefined);

  // The same code again is a venue already open.
  nav.step("Join", "row", "Join with code…");
  until("the join route", (s) => s.find({ role: "text", label: "Join a venue" }) !== undefined);
  app.type(field(), "K7M4-Q9TX");
  app.key("enter");
  until("already in", (s) => s.find({ role: "text", label: "You already have access to Warehouse." }) !== undefined);
});

// The venue menu marks a joined venue "shared with you", as the picker does.
test("the venue menu marks a joined venue as shared with you", () => {
  until("the picker", (s) => s.find({ role: "card", label: "Test Venue" }) !== undefined);
  nav.step("Join", "button", "Join a venue");
  const focusedInput = (s) => s.find((n) => n.role === "input" && n.focused === true);
  app.type(focusedInput(until("the code field", (s) => focusedInput(s) !== undefined)), "k7m4 q9tx");
  app.key("enter");
  until("the joined venue to open", (s) => s.find({ role: "button", label: "Warehouse" }) !== undefined, { timeoutMs: 10000 });
  nav.venueMenu();
  const listed = until("the list", (s) => s.find({ role: "row", label: "Warehouse" }) ? s : undefined);
  assert(listed.find({ role: "text", label: "Warehouse is shared with you" }), "the joined venue is not marked as shared");
});
