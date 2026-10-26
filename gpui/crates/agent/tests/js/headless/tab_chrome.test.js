// The workspace's `+` box and the panel it lives in.

fixture({
  seconds: 20,
  clips: [{ pattern: "pattern-strobe", name: "Strobe", start: 2, end: 6 }],
  rig: 4,
  // The interesting frames are the ones during the box's entrance: it has to
  // be there both while it arrives and after it settles.
  motion: true,
});

// ⌘T opens the box, and the box outlives the frames around it.
test("new tab opens its box and the box stays", () => {
  nav.trackEditor("Test Venue", "Aurora");
  until("the timeline", (s) => s.find({ role: "card", label: "Waveform" }) !== undefined);

  app.action("luma::NewTab");
  const menu = until("the + box", (s) => s.find({ role: "card", label: "New tab" }) !== undefined);
  expect(menu.find({ role: "row", label: "Venue" }) !== undefined).toBe(true);
  // And it stays: a box that survives one frame and then vanishes as the
  // shell settles is the same bug arriving late.
  app.frames(12, { waitMs: 40 });
  const settled = app.snapshot();
  assert(settled.find({ role: "card", label: "New tab" }) !== undefined,
    "the box was dismissed while it was still arriving");
  assert(settled.find({ role: "card", label: "Tab strip" }) !== undefined, "the strip went away under its box");
});
