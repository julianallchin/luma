// The panel before its first tab.
//
// An empty workspace used to be a second, silent reason to hide the panel,
// so the surface that offers the first tab was withheld until a tab existed.
// What is asserted is the way out of that state, by both routes a user has:
// the toggle in the window's corner, and ⌘T.

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

test("an empty panel offers the ways to open a tab", () => {
  openVenue();
  const landed = read();
  assert(landed.panelEnabled === true, "the panel toggle was inert with no tabs, so the empty state had no door");
  // With no tabs the panel rests open onto its empty state.
  assert(landed.empty, "the panel did not open onto its empty state");
  // Every choice, by its canonical label: the list the `+` menu offers.
  const buttons = app.snapshot().findAll({ role: "button" }).map((n) => n.label);
  expect(buttons).toContain("Venue");
  expect(buttons).toContain("Track editor");
  // Exactly one offer: no `+` while the empty state is up.
  assert(!landed.add, "the add control appeared beside the empty state");

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

test("new tab with no tabs reaches the empty panel and the venue row opens the venue tab", () => {
  openVenue();
  // The regression: ⌘T with an empty workspace produced nothing at all. It
  // must land on the panel's offer.
  app.action("luma::ToggleWorkspace");
  app.frames(4);
  app.action("luma::NewTab");
  app.frames(4);
  assert(read().empty, "⌘T with no tabs open reached nothing that can open one");

  // The venue tab replaces the empty state, in the strip like any tab.
  nav.venuePage("Test Venue");
  app.frames(4);
  const opened = read();
  assert(!opened.empty && opened.venue, `the venue row did not open the venue tab: ${JSON.stringify(opened)}`);
  assert(opened.strip, "the venue tab drew no strip");
  expect(closeButtons(app.snapshot()).map((n) => n.label)).toEqual(["Close Venue"]);
});
