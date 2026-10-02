// The workspace's new-tab menu and the panel it lives in.

fixture({
  seconds: 20,
  clips: [{ pattern: "pattern-strobe", name: "Strobe", start: 2, end: 6 }],
  rig: 4,
  // The interesting frames are the ones during the panel's entrance: the menu
  // is opened while its region is still arriving, and has to be there both
  // then and after it settles.
  motion: true,
});

// ⌘T brings the editor back and opens the menu, including from a shut
// editor. The strip spans the tab's chat and its editor, so it stays while the
// editor is away.
test("new tab opens the panel and its menu together", () => {
  nav.trackEditor("Test Venue", "Aurora");
  until("the timeline", (s) => s.find({ role: "card", label: "Waveform" }) !== undefined);

  app.action("luma::ToggleWorkspace");
  until("the editor put away", (s) => s.find({ role: "card", label: "Waveform" }) === undefined);

  app.action("luma::NewTab");
  const menu = until("the new-tab menu", (s) => s.find({ role: "card", label: "New tab menu" }) !== undefined);
  expect(menu.find({ role: "button", label: "Track editor" }) !== undefined).toBe(true);
  // And it stays: a menu that survives one frame and then vanishes as the
  // panel settles is the same bug arriving late.
  app.frames(12, { waitMs: 40 });
  const settled = app.snapshot();
  assert(settled.find({ role: "card", label: "New tab menu" }) !== undefined,
    "the menu was dismissed while the panel it belongs to was still arriving");
  assert(settled.find({ role: "card", label: "Tab strip" }) !== undefined, "the panel came back without its strip");
});
