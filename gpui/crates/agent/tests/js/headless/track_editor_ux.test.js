// The track editor's pointer and view contract, driven through real input:
// which vertical band answers a press, that only a clip's header bar is
// grabbable, that a sweep of empty lane selects what it contains, that a
// resize snaps to the beat grid and moves every selected clip, that every
// destructive command steps back under undo, that two clips may share a
// layer, and that the wheel scrolls where it is not zooming. The contract is
// `harness/gauntlet-te/behavior-spec.md`.
//
// A clip's node bounds say where the canvas drew it; the toolbar says how
// many clips are selected and what the cursor spans. A change that moved only
// the picture, or only the state, disagrees with one of the two.

// The editor labels a clip by its form, so each clip plays its own form.
// Wash and Strobe share a span in different lanes, so a rectangle over both
// selects two clips and over one selects one. Haze sits early and alone.
const HAZE = "Noise";
const STROBE = "Strobe";
const WASH = "Chase";
fixture({
  // Twenty seconds at 120 bpm: a beat every half-second.
  seconds: 20,
  clips: [
    { pattern: "pattern-haze", name: "Haze", start: 2, end: 6, lane: 0, preset: ["color.noise@1", "Drift"] },
    { pattern: "pattern-strobe", name: "Strobe", start: 14, end: 18, lane: 0, preset: ["strobe.constant@1", "Strobe"] },
    { pattern: "pattern-wash", name: "Wash", start: 14, end: 18, lane: 1, preset: ["color.chase@1", "Chase"] },
  ],
  // The pixel premises below were authored against a 1200-wide canvas; in
  // takeover the shell and the inspector spend the rest.
  window: [1800, 818],
});
const HAZE_SECONDS = 4;

const shot = () => app.snapshot();
const status = () => shot().findAll({ role: "text" }).map((n) => n.label);
const readout = (prefix) => status().find((label) => label.startsWith(prefix)) ?? null;
const node = (role, label) => shot().find({ role, label });
const selected = (labels) => labels.some((label) => label.endsWith(" selected"));
// Where time zero is on screen: the canvas origin, off the waveform strip.
const origin = () => node("card", "Waveform").bounds.x;
const playhead = () => node("slider", "Playhead")?.bounds.x ?? null;
const waveform = () => node("card", "Waveform");
// The document's clip count.
const total = () => parseInt(status().find((label) => label.endsWith(" clips")), 10);
const count = (label) => shot().findAll({ role: "card", label }).length;
const laneHeight = () => node("row", "Lane 1").bounds.height;
// The editor shows no save state; outwait the debounce and the round trip.
const settled = () => app.frames(20, { waitMs: 40 });

// Pixels per second at the opening zoom, read off a clip of known length.
let ZOOM = null;

function open() {
  nav.track("Aurora");
  until("the timeline", (s) => s.find({ role: "card", label: "Waveform" }) !== undefined);
  nav.expand();
  // The editor's own geometry: give it the whole column.
  nav.stageOff();
  until("the clips", (s) => s.find({ role: "card", label: WASH }) !== undefined);
  if (ZOOM === null) ZOOM = node("card", HAZE).bounds.width / HAZE_SECONDS;
}

function reopen() {
  settled();
  nav.closeTab();
  app.frames(6);
  open();
}

// Every card with this label, left to right, in seconds.
function spans(label) {
  const zero = origin();
  return shot().findAll({ role: "card", label })
    .map((c) => ({ start: (c.bounds.x - zero) / ZOOM, length: c.bounds.width / ZOOM, y: c.bounds.y, height: c.bounds.height }))
    .sort((a, b) => a.start - b.start);
}
const span = (label) => spans(label)[0];
const near = (a, b, tolerance = 0.01) => Math.abs(a - b) < tolerance;

function begin() {
  nav.venue("Test Venue");
  app.frames(8);
  open();
  expect(status()).toContain("3 clips");
}

test("press regions, the header grab, the marquee, resize, snap and move", () => {
  begin();
  const opened = playhead();

  // A clip's header selects it and sets a cursor.
  app.click(node("card", STROBE));
  app.frames(2);
  expect(status()).toContain("1 selected");
  expect(node("slider", "Cursor") !== undefined).toBe(true);

  // The waveform clears both, and does not move the playhead.
  app.click(waveform());
  app.frames(2);
  assert(!selected(status()), "pressing the waveform left the selection");
  expect(node("slider", "Cursor")).toBe(undefined);
  expect(playhead()).toBe(opened);

  // The ruler seeks to the point under the press.
  const ruler = node("card", "Ruler");
  app.click(ruler);
  app.frames(4);
  assert(Math.abs(playhead() - (ruler.bounds.x + ruler.bounds.width / 2)) <= 1, "the ruler did not seek to the press");

  // The empty insertion lane above the top layer clears the selection too.
  app.click(node("card", STROBE));
  app.frames(2);
  app.click(node("row", "Lane 0"));
  app.frames(2);
  assert(!selected(status()), "pressing the empty row-0 lane did not clear the selection");

  // Only a clip's header bar answers the pointer: its node is far shorter
  // than its lane.
  for (const label of [HAZE, STROBE, WASH]) {
    expect(span(label).height).toBeLessThan(laneHeight() / 2);
  }

  // A sweep selects what its rectangle contains: one lane catches Strobe
  // alone; a lane taller catches Strobe and Wash; neither catches Haze, which
  // starts before the sweep.
  const sweep = (dy) => {
    const lane = node("row", "Lane 2");
    app.drag({ x: origin() + 13 * ZOOM, y: lane.bounds.y + lane.bounds.height / 2 }, { dx: 7 * ZOOM, dy });
    app.frames(2);
  };
  sweep(0);
  expect(status()).toContain("1 selected");
  sweep(-laneHeight());
  expect(status()).toContain("2 selected");
  // A range cursor, not a point.
  expect(readout("Cursor ")).toContain("-");

  // A resize moves the same edge of every selected clip by the same delta.
  const before = { strobe: span(STROBE), wash: span(WASH) };
  app.drag(node("slider", `${STROBE} end`), { dx: 2 * ZOOM, dy: 0 });
  app.frames(20);
  for (const [label, was] of [[STROBE, before.strobe], [WASH, before.wash]]) {
    const now = span(label);
    assert(near(now.length - was.length, 2) && near(now.start, was.start), `${label} did not follow the group resize`);
  }
  settled();

  // A resize snaps to the beat grid: 1.8 s of drag lands on a grid line (an
  // eighth of a second at this zoom), which the drag alone could not.
  app.click(node("card", HAZE));
  app.frames(2);
  const unsnapped = span(HAZE);
  app.drag(node("slider", `${HAZE} end`), { dx: 1.8 * ZOOM, dy: 0 });
  app.frames(20);
  const moved = span(HAZE).length - unsnapped.length;
  const steps = moved / 0.125;
  assert(Math.abs(steps - Math.round(steps)) * 0.125 <= 0.02 && !near(moved, 1.8, 0.03),
    `the resize moved the edge by ${moved}s, not onto the grid`);
  settled();

  // A header drag slides the whole clip, and it survives a reopen.
  const was = span(HAZE);
  app.drag(node("card", HAZE), { dx: 2 * ZOOM, dy: 0 });
  app.frames(20);
  const now = span(HAZE);
  assert(near(now.start - was.start, 2) && near(now.length, was.length), `a header drag did not slide the clip 2 s: ${JSON.stringify({ was, now })}`);
  reopen();
  const back = span(HAZE);
  assert(near(back.start, now.start) && near(back.length, now.length), "the move did not survive a reopen");
});

test("the wheel scrolls and zooms, and both clamp", () => {
  begin();
  const toStart = () => {
    app.scroll(waveform(), { dx: 80000, steps: 20 });
    app.frames(2);
  };
  // Twenty seconds at the opening zoom fit the window, so zoom in first.
  // Control-wheel is the pinch on every platform (the handler reads control
  // before the platform key), so one amount serves both.
  app.scroll(waveform(), { dy: 60, steps: 10, modifiers: ["control"] });
  toStart();
  const zoomedIn = node("card", HAZE).bounds.width / HAZE_SECONDS;
  expect(zoomedIn).toBeGreaterThan(1.5 * ZOOM);
  // The clip's right edge: its left one may scroll out of view and clip.
  const right = () => { const b = node("card", HAZE).bounds; return b.x + b.width; };
  const rest = right();

  // A 200 px wheel scrolls 200 px.
  app.scroll(waveform(), { dx: -200, steps: 10 });
  app.frames(2);
  assert(Math.abs(rest - right() - 200) < 1, `a 200 px wheel scrolled ${rest - right()} px`);

  // Far past the end, twice: a scroll that clamps lands in the same place.
  app.scroll(waveform(), { dx: -40000, steps: 20 });
  app.frames(2);
  const end = span(WASH).start;
  app.scroll(waveform(), { dx: -40000, steps: 20 });
  app.frames(2);
  assert(end > 1 && near(end, span(WASH).start, 0.001), "scrolling past the end kept going");

  // Back at the start, Haze sits at its own start time.
  toStart();
  assert(near(span(HAZE).start * ZOOM / zoomedIn, 2, 0.05), "scrolling back did not reach the start of the track");

  // All the way out, where the zoom clamps: a second pull changes nothing.
  app.scroll(waveform(), { dy: -4000, steps: 20, modifiers: ["control"] });
  toStart();
  const out = node("card", HAZE).bounds.width;
  expect(out / HAZE_SECONDS).toBeLessThan(ZOOM);
  app.scroll(waveform(), { dy: -4000, steps: 20, modifiers: ["control"] });
  toStart();
  expect(node("card", HAZE).bounds.width).toBe(out);

  // `F` toggles following the playhead, and says so.
  app.key("f");
  app.frames(2);
  expect(status()).toContain("Follow");
  app.key("f");
  app.frames(2);
  assert(!status().includes("Follow"), "F did not turn follow-playhead back off");
});

// ⌘L loops the cursor's range, and a second press over the same range takes
// the loop off. The chord reaches the editor, not the agent chat.
test("the loop key loops the cursor's range and takes it off again", () => {
  begin();
  app.drag(node("row", "Lane 2"), { dx: 200, dy: 0 });
  app.frames(2);
  const cursor = readout("Cursor ");
  app.key("secondary-l");
  app.frames(2);
  const region = readout("Loop ");
  assert(region !== null && region.slice("Loop ".length) === cursor.slice("Cursor ".length),
    `the loop is not the cursor's range: ${cursor} → ${region}`);
  app.key("secondary-l");
  app.frames(2);
  expect(readout("Loop ")).toBe(null);
});

// Everything here is a write: each section reads back after a reopen, the
// only proof the score and not just the canvas moved.
test("duplicate, delete, undo, split, lift, alt-drag and overlap are writes", () => {
  begin();
  // The lane block is bottom-anchored: the last lane's floor is the canvas's.
  const last = node("row", "Lane 2").bounds;
  const head = node("slider", "Playhead").bounds;
  assert(Math.abs(last.y + last.height - (head.y + head.height)) <= 1, "the lanes are not pinned to the canvas floor");
  const start = total();

  // Duplicate lays the cursor's region down again after itself.
  app.click(node("card", HAZE));
  app.frames(2);
  app.key("secondary-d");
  app.frames(20);
  settled();
  expect([count(HAZE), total()]).toEqual([2, start + 1]);

  // The copy is still selected and the cursor spans it, so Delete clears
  // exactly that region.
  app.key("delete");
  app.frames(20);
  reopen();
  expect([count(HAZE), total()]).toEqual([1, start]);

  // Undo puts a destructive command back, redo takes it away, and both are
  // writes. No reopen inside: the history belongs to the screen.
  app.click(node("card", STROBE));
  app.frames(2);
  app.key("delete");
  app.frames(20);
  expect([count(STROBE), total()]).toEqual([0, start - 1]);
  app.key("secondary-z");
  app.frames(20);
  expect([count(STROBE), total()]).toEqual([1, start]);
  app.key("secondary-shift-z");
  app.frames(20);
  expect(count(STROBE)).toBe(0);
  app.key("secondary-z");
  app.frames(20);
  reopen();
  expect([count(STROBE), total()]).toEqual([1, start]);

  // Split: Haze a second right of the middle of the lane, the cursor set
  // with a press on its body below the header, then cut in two. The second
  // is what makes the later half overlap Wash, so the lift below cannot
  // share Wash's lane.
  const middle = node("row", "Lane 2").bounds.width / 2 / ZOOM + 1;
  const haze = span(HAZE);
  app.drag(node("card", HAZE), { dx: (middle - (haze.start + haze.length / 2)) * ZOOM, dy: 0 });
  app.frames(20);
  settled();
  const straddling = span(HAZE);
  const header = node("card", HAZE).bounds;
  app.drag({ x: header.x + header.width / 2, y: header.y + 40 }, { dx: 0, dy: 0 }, { steps: 1 });
  app.frames(2);
  app.key("secondary-e");
  app.frames(20);
  reopen();
  const halves = spans(HAZE);
  expect(halves.length).toBe(2);
  assert(near(halves[0].start, straddling.start) && near(halves[0].length + halves[1].length, straddling.length, 0.05),
    `the halves do not cover the clip they came from: ${JSON.stringify({ straddling, halves })}`);

  // A vertical drag is a z-index write: the half pulled up lands a lane
  // above and stays there across a reopen. Other clips keep their timing.
  const lane = laneHeight();
  const strobeBefore = span(STROBE);
  const gap = () => { const h = spans(HAZE); return Math.abs(h[1].y - h[0].y); };
  assert(Math.abs(gap()) < 1, "the two halves did not start in one lane");
  app.drag(node("card", HAZE), { dx: 0, dy: -lane });
  app.frames(20);
  settled();
  const lifted = gap();
  assert(lifted >= lane - 1, `an upward drag did not lift one half a lane: gap ${lifted}, lane ${lane}, ${JSON.stringify(spans(HAZE))}`);
  // It stays separately editable beside Wash.
  const liftedHalf = Math.min(...spans(HAZE).map((h) => h.y));
  expect(Math.abs(liftedHalf - span(WASH).y) >= lane - 1).toBe(true);
  reopen();
  assert(Math.abs(gap() - lifted) < 1, "the lane change did not survive a reopen");
  const strobeAfter = span(STROBE);
  assert(near(strobeAfter.start, strobeBefore.start) && near(strobeAfter.length, strobeBefore.length),
    "Strobe's timing changed when another clip changed layer");

  // Alt-drag: the copy stays where the press was, the original moves.
  const from = span(STROBE).start;
  app.drag(node("card", STROBE), { dx: -10 * ZOOM, dy: 0 }, { modifiers: ["alt"] });
  app.frames(20);
  reopen();
  const strobes = spans(STROBE);
  expect(strobes.length).toBe(2);
  assert(near(strobes[1].start, from) && near(strobes[0].start, from - 10), `alt-drag: ${JSON.stringify(strobes)}`);

  // Two clips may share a layer and a span: a move across a neighbour
  // survives the write rather than being rolled back on the next visit.
  const leftmost = shot().findAll({ role: "card", label: STROBE }).sort((a, b) => a.bounds.x - b.bounds.x)[0];
  app.drag(leftmost, { dx: 7 * ZOOM, dy: 0 });
  app.frames(20);
  const crossed = spans(STROBE);
  expect(crossed.length).toBe(2);
  assert(near(crossed[0].start, strobes[0].start + 7), `the drag did not slide the clip 7 s: ${JSON.stringify(crossed)}`);
  assert(crossed[0].start + crossed[0].length > crossed[1].start, "the two clips do not overlap, so this proves nothing");
  assert(!status().some((label) => label.includes("overlap")), "the editor reported an overlap refusal");
  reopen();
  const restored = spans(STROBE);
  expect(restored.length).toBe(2);
  assert(near(restored[0].start, crossed[0].start), "the overlapping move did not survive a reopen");
  assert(!status().some((label) => label.includes("overlap")), "the editor reported an overlap refusal after a reopen");
});

// A right-click offers the shipped presets, not the score's own patterns,
// and commits one onto the lane it pointed at: row 0 opens a layer above
// everything. The menu answers the keyboard, and Escape closes it.
test("the insertion menu lists presets and answers pointer and keyboard", () => {
  begin();
  const fitLanes = () => {
    app.action("luma::FitLanes");
    app.frames(2);
  };
  const shipped = library.presets().map((p) => p.name);
  fitLanes();
  const before = total();
  // The menu's rows are the rows drawn inside its dialog, after it: the
  // lanes lie under the dialog, and the inspector's preset browser lists the
  // same names.
  app.click(node("row", "Lane 0"), { button: "right" });
  const opened = until("the insertion menu", (s) => s.find({ role: "card", label: "Insert pattern dialog" }));
  const box = opened.find({ role: "card", label: "Insert pattern dialog" }).bounds;
  const from = opened.nodes.findIndex((n) => n.label === "Insert pattern dialog");
  const menu = opened.nodes.slice(from)
    .filter((n) => n.role === "row" && n.bounds.x >= box.x && n.bounds.x < box.x + box.width && n.bounds.y >= box.y && n.bounds.y < box.y + box.height)
    .map((n) => n.label);
  expect(menu.slice(0, 2)).toEqual(shipped.slice(0, 2));
  assert(!menu.includes("Haze"), "the insertion menu offered a score pattern");

  // The first preset is Wash, a clip of Constant color.
  app.type(node("input", "Search patterns…"), shipped[0]);
  app.frames(2);
  // The dialog's row, painted last: the browser behind it has one too.
  app.click(shot().findAll({ role: "row", label: shipped[0] }).at(-1));
  app.frames(20);
  reopen();
  expect(total()).toBe(before + 1);
  const placed = spans("Constant color");
  expect(placed.length).toBe(1);
  // Row 0 opened a lane of its own above the rest.
  assert(placed[0].y < span(WASH).y - laneHeight() + 1, "an insertion on row 0 did not open a lane above the rest");

  // ArrowDown moves the active row and Enter commits that one: the second
  // preset, read back from the score by its values.
  const presets = library.presets();
  const isPreset = (clip, preset) => clip.graph === preset.form && JSON.stringify(clip.inputs) === JSON.stringify(preset.inputs);
  const copies = (preset) => Object.values(library.score().clips).filter((clip) => isPreset(clip, preset)).length;
  const chosen = copies(presets[1]);
  fitLanes();
  app.click(node("row", "Lane 0"), { button: "right" });
  until("the insertion menu", (s) => s.find({ role: "card", label: "Insert pattern dialog" }));
  app.key("down");
  app.frames(2);
  app.key("enter");
  app.frames(20);
  reopen();
  expect(total()).toBe(before + 2);
  expect(copies(presets[1])).toBe(chosen + 1);

  fitLanes();
  app.click(node("row", "Lane 0"), { button: "right" });
  until("the insertion menu", (s) => s.find({ role: "card", label: "Insert pattern dialog" }));
  app.key("escape");
  until("the menu to close", (s) => s.find({ role: "card", label: "Insert pattern dialog" }) === undefined);
  expect(node("card", "Ruler") !== undefined).toBe(true);
  expect(total()).toBe(before + 2);
});

// Follow re-centres while the transport is stopped: the playhead moves
// because the pointer moved it, and the eye still has to keep up.
test("follow re-centres a scrub while stopped", () => {
  begin();
  // The key comes before the wheel on purpose: a wheel after a keystroke
  // must still reach the canvas, and the assertion needs the zoom.
  app.key("f");
  app.frames(2);
  app.scroll(waveform(), { dy: 120, steps: 10, modifiers: ["control"] });
  app.scroll(waveform(), { dx: 80000, steps: 20 });
  app.frames(2);
  const strip = node("card", "Ruler");
  // One step: the press lands under the pointer and the move is 300 px
  // right of it, which a following eye pulls back to the middle.
  app.drag(strip, { dx: 300, dy: 0 }, { steps: 1 });
  app.frames(6);
  const centre = strip.bounds.x + strip.bounds.width / 2;
  assert(Math.abs(playhead() - centre) <= 2, `a followed scrub left the playhead at ${playhead()}, not at ${centre}`);
});
