// The rig is lit by the track, and the light moves with it.
//
// `visualizer.test.js` proves the viewport draws a rig and redraws it when the
// camera moves. That would pass with the evaluator disconnected. This is the
// other half: the frame's colour comes from the score sampled at the
// transport's time, and so changes when the transport or the score does.
//
// Every read is of the stage's own pixels: the stage is a fraction of the
// window, and the editor's playhead sweeping its timeline would satisfy "the
// frame changed" while the light stood still.

// The clip spans the whole track, so an unlit frame is never a legitimate
// outcome. Rainbow cycles through saturated hues every four beats (two
// seconds at the fixture's 120 bpm), and it is labelled by its name.
fixture({
  seconds: 20,
  rig: 4,
  clips: [{ pattern: "pattern-rainbow", name: "Rainbow", start: 0, end: 20, preset: "Rainbow" }],
});
const CLIP = "Rainbow";

const stageShot = () => app.screenshot({ node: app.snapshot().find({ role: "card", label: "Stage" }) });

// With the house lights down the room is dark, and the only saturated hue left
// is the pattern's — so a spread between the channel means is beam.
function chroma(shot) {
  const [r, g, b] = image.stats(shot).mean;
  return Math.max(r, g, b) - Math.min(r, g, b);
}

// The track editor is the only tab that names a `(track, venue)`, so it is
// what puts a lit stage up. The clip is the evidence the score has landed.
function openLitStage() {
  nav.trackEditor("Test Venue", "Aurora");
  until("the clip", (s) => s.find({ role: "card", label: CLIP }) !== undefined);
  nav.expand();
  app.frames(10, { waitMs: 60 });
  // The house lights tint the room on their own; take them to zero so what
  // colour is left came from the rig.
  const slider = app.snapshot().find({ role: "slider", label: "House lights" });
  app.drag(slider, { dx: 0, dy: 140 }, { steps: 12, restale: "match" });
  until("the house lights down", (s) => s.find({ role: "text", label: "House lights = 0%" }) !== undefined);
  app.frames(10, { waitMs: 60 });
}

test("the rig is lit by the playing track", () => {
  openLitStage();
  const first = stageShot();
  expect(chroma(first)).toBeGreaterThan(2);

  // The pattern has a two-second period, so a second of playback is half a
  // cycle: a sample pinned to one time, or a cached frame, cannot move.
  nav.step("the Play button", "button", "Play");
  app.frames(20, { waitMs: 55 });
  expect(image.diff(first, stageShot())).toBeGreaterThan(0.01);
});

// A screen that edited clips without re-installing the scene would pass the
// test above and still show the rig as the score was when the view opened.
// `selection` is the arg to move: pointing it at a group the venue does not
// have leaves the pattern no fixtures, so the beam must go out.
test("an arg edit relights the rig", () => {
  openLitStage();
  app.click(app.snapshot().find({ role: "card", label: CLIP }));
  const field = () => app.snapshot().findAll({ role: "input" }).find((n) => n.label.startsWith("expression = "));
  until("the clip's selection field", () => field() !== undefined);
  app.frames(6, { waitMs: 40 });

  const lit = chroma(stageShot());
  expect(lit).toBeGreaterThan(2);

  app.click(field());
  app.key("cmd-a backspace");
  app.type(field(), "nothing_is_in_this_group", { restale: "match" });
  // An unknown word suggests nothing, so enter commits. The composite trails
  // the edit by a round trip.
  app.key("enter");
  app.frames(20, { waitMs: 60 });

  // No fixture is selected, so no beam colours the room.
  expect(chroma(stageShot())).toBeLessThan(lit / 4);
});
