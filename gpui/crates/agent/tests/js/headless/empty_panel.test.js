// The panel before its first tab.
//
// An empty workspace used to be a second, silent reason to hide the panel.
// What is asserted is the way out of that state, by both routes a user has:
// the toggle in the window's corner, and ⌘T, which opens the strip's `+` box
// (docs/specs/venue-tabs.md rule 9).

const WIDTH = 1280;

fixture({
  seconds: 20,
  clips: [{ pattern: "pattern-strobe", name: "Strobe", start: 2, end: 6 }],
  window: [WIDTH, 800],
});

function read() {
  const shot = app.snapshot();
  const panel = shot.find({ role: "button", label: "panel-toggle" });
  return {
    empty: shot.find({ role: "card", label: "Empty panel" }) !== undefined,
    venue: shot.find({ role: "card", label: "Test Venue Venue" }) !== undefined,
    strip: shot.find({ role: "card", label: "Tab strip" }) !== undefined,
    panelEnabled: panel === undefined ? null : panel.enabled,
    add: shot.find({ role: "button", label: "new-tab" }) !== undefined,
  };
}

// A venue with no tabs open: the sidebar's list, nothing opened from it.
function openVenue() {
  nav.venue("Test Venue");
  until("the track list", (s) => s.find({ role: "input", label: "Search tracks" }) !== undefined);
  app.frames(2);
}

const closeButtons = (s) => s.nodes.filter((n) => n.role === "button" && n.label.startsWith("Close "));

test("an empty panel points at the + and the + is there", () => {
  openVenue();
  const landed = read();
  assert(landed.panelEnabled === true, "the panel toggle was inert with no tabs, so the empty state had no door");
  // With no tabs the panel rests open onto its empty state.
  assert(landed.empty, "the panel did not open onto its empty state");
  // One offer: the strip's `+`, with or without tabs. The panel only names
  // its key, so it holds no buttons of its own.
  assert(landed.add, "the strip had no + with no tabs open");
  const panel = app.snapshot().find({ role: "card", label: "Empty panel" }).bounds;
  const inPanel = app.snapshot().findAll({ role: "button" }).filter((n) =>
    n.bounds.x >= panel.x && n.bounds.y >= panel.y
    && n.bounds.x + n.bounds.width <= panel.x + panel.width
    && n.bounds.y + n.bounds.height <= panel.y + panel.height);
  expect(inPanel.map((n) => n.label)).toEqual([]);

  // The toggle is a door, and a door swings both ways.
  app.click(app.snapshot().find({ role: "button", label: "panel-toggle" }));
  app.frames(4);
  assert(!read().empty, "the toggle did not put the empty panel away");
  app.click(app.snapshot().find({ role: "button", label: "panel-toggle" }));
  app.frames(4);
  assert(read().empty, "the toggle closed the empty panel and could not bring it back");
});

// The last tab's close lands on the panel's offer: a tab owns its chat, so
// with no tab there is no chat to share the room with.
test("closing the last tab lands on the empty panel", () => {
  nav.trackEditor("Test Venue", "Aurora");
  until("the track's tab", (s) => closeButtons(s).length > 0);
  app.action("luma::CloseTab");
  const shot = until("the empty panel", (s) => s.find({ role: "card", label: "Empty panel" }) ? s : undefined);
  expect(closeButtons(shot).length).toBe(0);
  expect(shot.find({ role: "slider", label: "Workspace width" })).toBe(undefined);
  expect(shot.findAll({ role: "text" }).some((n) => n.label === "Luma")).toBe(false);
});

test("new tab with no tabs brings the panel up with the + box open", () => {
  openVenue();
  // The regression: ⌘T with an empty workspace produced nothing at all.
  app.action("luma::ToggleWorkspace");
  app.frames(4);
  app.action("luma::NewTab");
  const shot = until("the + box", (s) => s.find({ role: "card", label: "New tab" }) ? s : undefined);
  assert(read().empty, "⌘T with no tabs open did not bring the panel back");
  expect(shot.find({ role: "row", label: "Venue" }) !== undefined).toBe(true);

  // The venue tab replaces the empty state, in the strip like any tab.
  app.key("v e n u e enter");
  until("the venue tab", (s) => s.find({ role: "card", label: "Test Venue Venue" }) !== undefined);
  const opened = read();
  assert(!opened.empty && opened.strip, `the venue tab did not replace the empty state: ${JSON.stringify(opened)}`);
  expect(closeButtons(app.snapshot()).map((n) => n.label)).toEqual(["Close Venue"]);
});
