// Rebindable shortcuts, end to end: Settings → Shortcuts records a key, the
// keymap answers it at once, the old key stops, a key another command holds
// is refused by name, and every hint that names the command follows. The
// relaunch half (a rebind is still there next launch) is Rust:
// `tests/headless/venues.rs`, which can open the app twice on one library.

fixture({ seconds: 4 });

const SHORTCUTS = "Shortcuts";
const AGENT = "View: Show or hide the agent panel";
const SONGS = "View: Song list";

// The caps inside a node: the text nodes drawn within its bounds.
const capsIn = (s, node) => {
  const b = node.bounds;
  return s.findAll({ role: "text" })
    .filter((n) => n !== node && n.bounds.width > 0
      && n.bounds.x >= b.x - 0.5 && n.bounds.x + n.bounds.width <= b.x + b.width + 0.5
      && n.bounds.y >= b.y - 0.5 && n.bounds.y + n.bounds.height <= b.y + b.height + 0.5)
    .map((n) => n.label);
};
const keysOf = (s, name) => {
  const recorder = s.find({ role: "button", label: name });
  return recorder === undefined ? undefined : capsIn(s, recorder);
};

function openShortcuts() {
  app.action("luma::OpenSettings");
  nav.step("the Shortcuts section", "toggle", SHORTCUTS);
  return until("the shortcuts", (s) => keysOf(s, AGENT)?.length > 0 ? s : undefined);
}

function record(name, key) {
  nav.step(`the ${name} recorder`, "button", name);
  until("the recorder listening", (s) => s.find({ role: "text", label: "Press a key…" }));
  app.key(key);
}

// The agent panel's state, read off its toggle: `true` up, `false` away.
const agentUp = (s) => s.find({ role: "button", label: "Hide agent" }) !== undefined
  ? true : s.find({ role: "button", label: "Show agent" }) !== undefined ? false : undefined;
// Rig: a page that has the panel (the song list a venue opens on has none).
const shell = () => {
  app.action("luma::ShowRig");
  return until("the shell", (s) => agentUp(s) !== undefined ? s : undefined);
};
const agentIs = (up) => until(`the agent panel ${up ? "up" : "away"}`, (s) => agentUp(s) === up);

test("a rebound command answers its new key and not its old one", () => {
  nav.venue("Test Venue");
  const before = openShortcuts();
  const defaults = keysOf(before, AGENT);

  record(AGENT, "secondary-shift-k");
  const after = until("the new keys", (s) => {
    const keys = keysOf(s, AGENT);
    return keys !== undefined && keys.length === 1 && keys[0] !== defaults[0] ? s : undefined;
  });
  assert(keysOf(after, AGENT)[0].includes("K"), `recorded ${keysOf(after, AGENT)}`);
  assert(after.find({ role: "button", label: `Reset ${AGENT}` }), "a rebound row offers no Reset");

  nav.dismiss();
  const start = agentUp(shell());

  // The old key does nothing now…
  app.key("secondary-b");
  app.frames(4);
  expect(agentUp(app.snapshot())).toBe(start);
  // …and the new one does what it did.
  app.key("secondary-shift-k");
  agentIs(!start);
  app.key("secondary-shift-k");
  agentIs(start);
});

test("a key another command holds is refused, by its name", () => {
  nav.venue("Test Venue");
  const before = openShortcuts();
  const songs = keysOf(before, SONGS);

  record(SONGS, "secondary-b");
  const refused = until("the refusal", (s) =>
    s.findAll({ role: "text" }).find((n) => n.label.includes("Show or hide the agent panel")) ? s : undefined);
  expect(keysOf(refused, SONGS)).toEqual(songs);
  assert(refused.find({ role: "button", label: `Reset ${SONGS}` }) === undefined,
    "a refused key still counts as a rebind");

  // The key was heard, not run: the agent panel never moved, and the
  // command that holds it still has it.
  nav.dismiss();
  const start = agentUp(shell());
  app.key("secondary-b");
  agentIs(!start);
});

test("Escape stops recording and leaves the keys and the page as they were", () => {
  nav.venue("Test Venue");
  const before = openShortcuts();
  const keys = keysOf(before, SONGS);
  record(SONGS, "escape");
  const after = until("the recorder stopped", (s) =>
    s.find({ role: "text", label: "Press a key…" }) === undefined ? s : undefined);
  expect(keysOf(after, SONGS)).toEqual(keys);
  assert(after.find({ role: "toggle", label: SHORTCUTS }), "Escape while recording left Settings");
});

test("Reset puts a row back, and Reset all every row", () => {
  nav.venue("Test Venue");
  const before = openShortcuts();
  const agent = keysOf(before, AGENT);
  const songs = keysOf(before, SONGS);

  record(AGENT, "secondary-shift-k");
  until("the agent rebound", (s) => s.find({ role: "button", label: `Reset ${AGENT}` }));
  nav.step("Reset", "button", `Reset ${AGENT}`);
  const reset = until("the agent's own keys", (s) =>
    s.find({ role: "button", label: `Reset ${AGENT}` }) === undefined ? s : undefined);
  expect(keysOf(reset, AGENT)).toEqual(agent);

  record(AGENT, "secondary-shift-k");
  until("the agent rebound", (s) => s.find({ role: "button", label: `Reset ${AGENT}` }));
  record(SONGS, "secondary-shift-j");
  until("the song list rebound", (s) => s.find({ role: "button", label: `Reset ${SONGS}` }));
  nav.step("Reset all", "button", "Reset all shortcuts");
  const all = until("every default back", (s) =>
    s.find({ role: "button", label: `Reset ${AGENT}` }) === undefined
      && s.find({ role: "button", label: `Reset ${SONGS}` }) === undefined ? s : undefined);
  expect(keysOf(all, AGENT)).toEqual(agent);
  expect(keysOf(all, SONGS)).toEqual(songs);

  // And the default key works again.
  nav.dismiss();
  const start = agentUp(shell());
  app.key("secondary-b");
  agentIs(!start);
});

test("Settings search finds a shortcut by its title and by its keys", () => {
  nav.venue("Test Venue");
  const page = openShortcuts();
  const keys = keysOf(page, SONGS)[0];
  const search = () => app.snapshot().find((n) => n.role === "input" && n.label === "Search settings");
  app.type(search(), "song list");
  until("the song list row by title", (s) => s.find({ role: "button", label: SONGS })
    && s.find({ role: "button", label: AGENT }) === undefined);
  app.key("escape");
  until("the search cleared", (s) => s.find({ role: "input", label: "Search settings" }));
  app.type(search(), keys);
  until("the song list row by its keys", (s) => s.find({ role: "button", label: SONGS }));
});

test("the venue dialog's chords are actions in the keymap", () => {
  const picker = until("the venue picker", (s) => s.find({ role: "card", label: "Test Venue" }) ? s : undefined);
  // Its legend names the chords the keymap holds.
  assert(texts(picker).some((label) => label.includes("J")), `no Join hint in ${texts(picker)}`);

  app.key("secondary-j");
  until("Join, by its key", (s) => s.find({ role: "text", label: "Join a venue" }));
  app.key("left");
  until("back on the list", (s) => s.find({ role: "card", label: "Test Venue" }));

  app.action("luma::JoinVenue");
  until("Join, by its action", (s) => s.find({ role: "text", label: "Join a venue" }));
  app.key("left");
  until("back on the list", (s) => s.find({ role: "card", label: "Test Venue" }));

  app.key("secondary-enter");
  until("Create, by its key", (s) => s.find({ role: "text", label: "A new room to light" }));
  app.key("left");
  until("back on the list", (s) => s.find({ role: "card", label: "Test Venue" }));
  app.action("luma::CreateVenue");
  until("Create, by its action", (s) => s.find({ role: "text", label: "A new room to light" }));
});
