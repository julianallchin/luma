// One press, one owner.
//
// A seam grip is wider than the rule it pulls, so it overhangs the panes
// either side. gpui hit-tests in paint order and reports every hitbox under
// the pointer, so the surface underneath used to take the same press:
// dragging the workspace seam seeked the transport, because the timeline's
// ruler was under the overhang. The grip blocks the mouse for what is painted
// behind it, and is mounted after both panes so "behind it" means both.
//
// The converse stops a fix that over-reaches: a gesture that starts on the
// canvas keeps the pointer wherever it wanders, seam included.

fixture({ seconds: 20, clips: [{ pattern: "pattern-strobe", name: "Strobe", start: 2, end: 6 }] });

// Where the seam is, how wide the tab body is, and where the playhead sits
// in the editor. The playhead is the witness for a leaked press: the ruler
// seeks the transport, and it runs under the strip the grip overhangs.
function read() {
  const shot = app.snapshot();
  const waveform = shot.find({ role: "card", label: "Waveform" });
  return {
    seam: shot.find({ role: "slider", label: "Workspace width" }).bounds.x,
    tab: waveform.bounds.width,
    playhead: shot.find({ role: "slider", label: "Playhead" }).bounds.x - waveform.bounds.x,
  };
}

// The middle of the ruler's height at some x.
function atRuler(x) {
  const ruler = app.snapshot().find({ role: "card", label: "Ruler" });
  return { x, y: ruler.bounds.y + ruler.bounds.height / 2 };
}

function open() {
  nav.trackEditor("Test Venue", "Aurora");
  until("the timeline", (s) => s.find({ role: "card", label: "Waveform" }) !== undefined);
}

test("a seam drag moves the seam and nothing under it", () => {
  open();
  // Park the playhead away from zero first: a shared press would seek it to
  // the pointer on the panel's edge.
  const wave = app.snapshot().find({ role: "card", label: "Waveform" });
  app.drag(atRuler(wave.bounds.x + 200), { dx: 0, dy: 0 }, { steps: 1 });
  app.frames(2);
  const before = read();
  // The grip's trailing pixel: the strip that overhangs the panel.
  const grip = app.snapshot().find({ role: "slider", label: "Workspace width" }).bounds;
  // Rightward: at the default size the thread already sits at its minimum
  // width, so the panel has room to give and none to take.
  app.drag(atRuler(grip.x + grip.width - 1), { dx: 120, dy: 0 }, { steps: 10 });
  app.frames(2);
  const after = read();
  const travelled = after.seam - before.seam;
  assert(travelled >= 100 && travelled <= 125, `the seam did not follow the pointer 120px right: ${before.seam} → ${after.seam}`);
  expect(after.tab).toBeLessThan(before.tab - 100);
  // The point of the test: the press did not reach the ruler under the grip.
  assert(after.playhead === before.playhead, "dragging the seam seeked the transport — the grip shares its press");
});

test("a scrub that crosses the seam scrubs and resizes nothing", () => {
  open();
  const before = read();
  // Start inside the ruler and drag through the seam, ending past it.
  const wave = app.snapshot().find({ role: "card", label: "Waveform" });
  app.drag(atRuler(wave.bounds.x + 160), { dx: -150, dy: 0 }, { steps: 10 });
  app.frames(2);
  const after = read();
  assert(after.playhead >= 0 && after.playhead <= 30 && after.playhead !== before.playhead,
    `the scrub did not leave the playhead where it ended, ~10px in: ${after.playhead}`);
  expect(after.seam).toBe(before.seam);
});
