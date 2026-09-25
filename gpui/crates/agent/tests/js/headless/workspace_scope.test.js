// The tab strip belongs to the picked track.
//
// `tabs.rs` and `workspace.rs` prove the swap as pure logic. This proves the
// wiring: picking a row in the sidebar is what moves the strip, and the set
// comes back intact.

fixture({
  seconds: 20,
  clips: [{ pattern: "pattern-strobe", name: "Strobe", start: 2, end: 6 }],
  equal_timestamp_track: true,
  rig: 4,
});

// The sidebar's Venue row is a place, not a tab. Picking it hides the thread
// and the strip and shows the venue page. Picking a track brings that track's
// strip and the thread back as they were.
test("the venue row takes the workspace and a track gives it back", () => {
  const state = () => {
    const s = app.snapshot();
    return {
      page: s.find({ role: "card", label: "Test Venue Venue" }) !== undefined,
      strip: s.find({ role: "card", label: "Tab strip" }) !== undefined,
      thread: s.findAll({ role: "text" }).some((n) => n.label === "Luma"),
      aurora: s.find({ role: "button", label: "Aurora" }) !== undefined,
    };
  };
  nav.trackEditor("Test Venue", "Aurora");
  until("Aurora's timeline", (s) => s.find({ role: "card", label: "Waveform" }) !== undefined);
  expect(state()).toEqual({ page: false, strip: true, thread: true, aurora: true });

  nav.venuePage("Test Venue");
  until("the thread collapsed", (s) => !s.findAll({ role: "text" }).some((n) => n.label === "Luma"));
  expect(state()).toEqual({ page: true, strip: false, thread: false, aurora: false });

  nav.step("Aurora again", "row", "Aurora");
  until("Aurora's strip", (s) => s.find({ role: "button", label: "Aurora" }) !== undefined);
  until("the thread back", (s) => s.findAll({ role: "text" }).some((n) => n.label === "Luma"));
  expect(state()).toEqual({ page: false, strip: true, thread: true, aurora: true });
});
