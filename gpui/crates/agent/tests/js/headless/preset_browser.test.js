// The preset browser from the outside: with no clip selected the inspector
// lists the shipped presets by form, one row each with a strip; the search
// filters them; a click and a drag place a form clip; hovering a row plays
// the preset on the stage and leaving it gives the stage back, with the
// score untouched.

fixture({ seconds: 20, graph_score: { clips: {} }, rig: 4, window: [1400, 1000] });

const node = (role, label) => until(label, (s) => s.find({ role, label })).find({ role, label });
const shipped = (form, name) => library.presets().find((p) => p.form === form && p.name === name);
const onlyClip = () => {
  const clips = Object.values(library.score().clips);
  expect(clips.length).toBe(1);
  return clips[0];
};

function open({ stageOff = true } = {}) {
  nav.venue("Test Venue");
  nav.track("Aurora");
  nav.expand();
  if (stageOff) nav.stageOff();
  until("the waveform", (s) => s.find({ role: "card", label: "Waveform" }));
}

test("the browser lists presets by form, filters, and places on click", () => {
  open();
  const inBrowser = (role) => {
    const p = node("card", "Presets").bounds;
    const inside = (n) => n.bounds.x >= p.x && n.bounds.x < p.x + p.width && n.bounds.y >= p.y && n.bounds.y < p.y + p.height;
    return app.snapshot().findAll({ role }).filter(inside).map((n) => n.label);
  };
  // With nothing selected the inspector is the browser, open and visible.
  const browser = node("card", "Presets").bounds;
  expect(browser.width).toBeGreaterThan(0);
  expect(app.snapshot().find({ role: "card", label: "Clip inputs" })).toBe(undefined);

  // The list scrolls; the forms on screen come first, one caption each, and
  // the tiles follow the shipped order.
  const captions = inBrowser("text");
  expect(captions.length).toBeGreaterThan(3);
  expect(new Set(captions).size).toBe(captions.length);
  const tiles = inBrowser("row");
  expect(tiles.slice(0, 4)).toEqual(library.presets().slice(0, 4).map((p) => p.name));
  // Each tile shows the preset's strip on this rig once it renders.
  until("the thumbnails", (s) => s.find({ role: "card", label: "Wash thumbnail" }) && s.find({ role: "card", label: "Chase thumbnail" }));

  // A search matches the form's name.
  app.type(node("input", "Search presets…"), "chase");
  app.frames(2);
  const filtered = inBrowser("row");
  for (const name of ["Chase", "Wave", "Bounce"]) expect(filtered).toContain(name);
  assert(!filtered.includes("Wash"), `the search kept Wash: ${filtered}`);

  // A click places the preset; the placed clip is selected.
  app.click(node("row", "Bounce"));
  until("the clip inputs", (s) => s.find({ role: "card", label: "Clip inputs" }));
  expect(app.snapshot().find({ role: "card", label: "Presets" })).toBe(undefined);
  app.click(node("card", "Waveform"));
  until("the browser back", (s) => s.find({ role: "card", label: "Presets" }));

  const clip = onlyClip();
  expect(clip.graph).toBe("color.chase@1");
  expect(clip.selection.expression).toBe("all");
  // A placed clip copies every value of its preset.
  expect(clip.inputs).toEqual(shipped("color.chase@1", "Bounce").inputs);
  // A click places four bars.
  assert(Math.abs(clip.duration - 16) < 0.01, `duration ${clip.duration}`);
});

test("a row dragged onto the timeline shows where it lands and lands there", () => {
  open();
  const lane = node("row", "Lane 0").bounds;
  const tile = node("row", "Ripple");
  const ghosts = () => app.painted().flatMap((s) => s.findAll({ role: "card", label: "Ripple drop preview" }));
  const centre = (b) => ({ x: b.x + b.width / 2, y: b.y + b.height / 2 });

  // Carried off the timeline and let go: nothing lands.
  const search = centre(node("input", "Search presets…").bounds);
  app.drag(tile, { dx: 0, dy: search.y - centre(tile.bounds).y }, { steps: 6, restale: "match" });
  app.frames(2);
  let shot = app.snapshot();
  expect(shot.find({ role: "card", label: "Ripple drop preview" })).toBe(undefined);
  expect(shot.find({ role: "card", label: "Presets" }) !== undefined).toBe(true);

  const from = node("row", "Ripple");
  const target = centre(lane);
  app.drag(from, { dx: target.x - centre(from.bounds).x, dy: target.y - centre(from.bounds).y }, { steps: 12, restale: "match" });
  // While over the lane, the timeline drew the clip it would make, in the
  // lane under the pointer.
  const seen = ghosts().map((n) => n.bounds);
  assert(seen.length > 0, "no drop preview while over the lane");
  assert(Math.abs(seen.at(-1).y - lane.y) < 4, `the drop preview is not in the lane: ${JSON.stringify(seen.at(-1))} vs ${JSON.stringify(lane)}`);
  until("the clip inputs", (s) => s.find({ role: "card", label: "Clip inputs" }));
  expect(app.snapshot().find({ role: "card", label: "Ripple drop preview" })).toBe(undefined);
  // The placed clip is written after a round trip.
  app.frames(8, { waitMs: 80 });

  const clip = onlyClip();
  expect(clip.graph).toBe("color.chase@1");
  expect(clip.inputs).toEqual(shipped("color.chase@1", "Ripple").inputs);
  // Where it was let go, not at the playhead.
  expect(clip.start).toBeGreaterThan(0);
});

test("hovering a row plays it on the stage until the pointer leaves", () => {
  open({ stageOff: false });
  const badge = (s) => s.find({ role: "text", label: "Preview · Gradient" });
  node("card", "Stage");
  expect(badge(app.snapshot())).toBe(undefined);
  // A wheel of nothing moves the pointer onto a control and leaves it there.
  app.scroll(node("row", "Gradient"), { dy: 0 });
  until("the stage to play the preset", badge);
  app.scroll(node("input", "Search presets…"), { dy: 0 });
  app.frames(1);
  const after = app.snapshot();
  // Leaving the tile gives the stage back, and hovering changed no score.
  expect(badge(after)).toBe(undefined);
  expect(after.findAll({ role: "card", label: "Gradient" }).length).toBe(0);
});
