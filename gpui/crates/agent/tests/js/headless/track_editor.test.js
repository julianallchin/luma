// The track editor, driven end to end against a seeded library.
//
// A clip's bounds are a document and the playhead is not. Dragging a clip's
// edge is a write that lands on the score's rows, so the test moves an edge,
// reads it, then leaves the screen and comes back: only the second reading
// can tell a repaint from a write. Playback is the opposite: nothing is
// written, and the evidence is that the transport moved on its own.

// Two clips on two lanes. A clip is labelled by its form, so both read
// Constant color; they are told apart by where they start.
fixture({
  seconds: 20,
  clips: [
    { pattern: "pattern-strobe", name: "Strobe", start: 2, end: 6, lane: 0 },
    { pattern: "pattern-wash", name: "Wash", start: 8, end: 14, lane: 1 },
  ],
  // Keep complete clip spans visible when the inspector opens.
  window: [1600, 900],
});

const FORM = "Constant color";
const DRAG_X = 100;

function read() {
  const shot = app.snapshot();
  const origin = shot.find({ role: "card", label: "Waveform" }).bounds.x;
  const clips = shot.findAll({ role: "card", label: FORM })
    .map((card) => ({ x: card.bounds.x - origin, width: card.bounds.width }))
    .sort((a, b) => a.x - b.x);
  const playhead = shot.find({ role: "slider", label: "Playhead" });
  return {
    clips,
    playhead: playhead === undefined ? null : playhead.bounds.x - origin,
    buttons: shot.findAll({ role: "button" }).map((n) => n.label),
    status: shot.findAll({ role: "text" }).map((n) => n.label),
  };
}

function open() {
  nav.track("Aurora");
  until("the timeline", (s) => s.find({ role: "card", label: "Waveform" }) !== undefined);
  nav.expand();
  // The editor's own geometry: give it the whole column.
  nav.stageOff();
  return read();
}

test("a clip edge dragged on the timeline moves, stays moved, and the playhead runs", () => {
  nav.venue("Test Venue");
  app.frames(8);
  const opened = open();
  expect(opened.status).toContain("2 clips");
  expect(opened.clips.length).toBe(2);
  expect(opened.buttons).toContain("Play");

  // Drag the first clip's end handle, not the clip: a drag starts at a
  // node's centre.
  const handle = app.snapshot().findAll({ role: "slider", label: `${FORM} end` }).sort((a, b) => a.bounds.x - b.bounds.x)[0];
  app.drag(handle, { dx: DRAG_X, dy: 0 });
  app.frames(20);
  const moved = read();
  // Exactly the clip it took hold of, by exactly the drag distance.
  expect(moved.clips[0].width - opened.clips[0].width).toBe(DRAG_X);
  expect(moved.clips[0].x).toBe(opened.clips[0].x);
  expect(moved.clips[1]).toEqual(opened.clips[1]);

  // Leave the screen and come back: what returns is what was written.
  nav.closeTab();
  app.frames(6);
  const reopened = open();
  expect(reopened.status).toContain("2 clips");
  expect(reopened.clips).toEqual(moved.clips);

  // Playing moves the playhead on its own, and the button says so.
  app.click(app.snapshot().find({ role: "button", label: "Play" }));
  until("the transport to report playing", (s) => s.find({ role: "button", label: "Pause" }));
  // The first poll that reads `isPlaying` can still read ~0; a few more.
  app.frames(4, { waitMs: 60 });
  const playing = read();
  expect(playing.playhead).toBeGreaterThan(reopened.playhead);
  app.click(app.snapshot().find({ role: "button", label: "Pause" }));
  app.frames(4, { waitMs: 60 });
  expect(read().buttons).toContain("Play");
});

test("beat grid approval survives reopening and can be undone", () => {
  const button = (label) => app.snapshot().find({ role: "button", label });
  const select = (label) => app.snapshot().find({ role: "select", label });
  const reopen = () => {
    nav.closeTab();
    nav.track("Aurora");
  };
  nav.venue("Test Venue");
  nav.track("Aurora");
  until("grid approval", () => button("Beat grid correct") !== undefined);
  app.click(button("Beat grid correct"));
  until("the saved approval", () => button("Beat grid approved") !== undefined);
  reopen();
  until("the reloaded approval", () => button("Beat grid approved") !== undefined);
  app.click(button("Beat grid approved"));
  until("the undone approval", () => button("Beat grid correct") !== undefined);
  reopen();
  until("the reloaded undo", () => button("Beat grid correct") !== undefined);
  app.click(button("Needs correction"));
  until("the saved rejection", () => button("Beat grid flagged") !== undefined);
  app.click(select("Reason (optional)"));
  app.click(button("Drift"));
  until("the saved reason", () => select("Drift") !== undefined);
  reopen();
  until("the reloaded rejection", () => select("Drift") !== undefined);
  app.click(button("Beat grid correct"));
  until("the replaced rejection", () => button("Beat grid approved") !== undefined);
  assert(select("Drift") === undefined, "approval kept a rejection reason");
  app.click(button("Needs correction"));
  until("rejected again", () => button("Beat grid flagged") !== undefined);
  app.click(button("Beat grid flagged"));
  until("the cleared rejection", () => button("Needs correction") !== undefined);
  reopen();
  until("the reloaded cleared rejection", () => button("Needs correction") !== undefined);
  assert(button("Beat grid approved") === undefined, "a cleared rejection became approval");
});
