// A form clip's alpha on the timeline: the fade handles, the bend handle and
// the level line write the clip's stored `alpha`, undo takes the edit back,
// and a clip moved over the end of another crosses the two with fades.

// One Chase clip over beats 2–6 (seconds 1–3), keyed `form-clip`.
const CHASE = { pattern: "form-clip", name: "Chase", start: 1, end: 3, preset: ["color@1", "Chase"] };
fixture({ seconds: 20, clips: [CHASE], rig: 4, window: [1400, 900] });

const node = (role, label) => until(label, (s) => s.find({ role, label })).find({ role, label });
const settle = () => app.frames(20, { waitMs: 60 });
const close = (a, b) => Math.abs(a - b) < 0.02;
const stored = () => library.score();
const alpha = (clip = "form-clip") => stored().clips[clip].inputs.alpha;
const FIXED = { type: "proportion", value: 1.0 };

// The points of a stored alpha curve, as [x, y] or [x, y, ease].
function points(value) {
  expect(value.type).toBe("time");
  return value.value.points;
}

function open() {
  nav.venue("Test Venue");
  nav.track("Aurora");
  nav.expand();
  nav.stageOff();
  until("the waveform", (s) => s.find({ role: "card", label: "Waveform" }));
  settle();
}

// The level line's vertical travel inside its lane.
function travel() {
  const lane = app.snapshot().findAll({ role: "row" }).find((n) => n.label === "Lane 1").bounds;
  return lane.height - 2 - 18 - 6;
}

test("fade, bend and level handles write the clip alpha", () => {
  open();
  // Drag the fade-in handle a quarter of the way into the clip.
  const card = node("card", "Color").bounds;
  const handle = node("slider", "Color fade in");
  // It starts at the clip's left edge.
  expect(Math.abs(handle.bounds.x + handle.bounds.width / 2 - card.x)).toBeLessThan(2);
  app.drag(handle, { dx: card.width / 4, dy: 0 }, { steps: 6 });
  settle();
  until("the form inputs", (s) => s.find({ role: "row", label: "Travel" }));
  const faded = points(alpha());
  expect(faded.length).toBe(3);
  assert(close(faded[0][0], 0) && close(faded[0][1], 0), `fade start: ${JSON.stringify(faded)}`);
  assert(close(faded[1][0], 0.25) && close(faded[1][1], 1), `fade end: ${JSON.stringify(faded)}`);
  assert(close(faded[2][0], 1) && close(faded[2][1], 1), `hold: ${JSON.stringify(faded)}`);
  const handles = app.snapshot().findAll({ role: "slider" }).map((n) => n.label).filter((l) => l.startsWith("Color "));
  expect(handles).toContain("Color fade in bend");
  // Alpha lives on the timeline, not in the sheet.
  assert(!app.snapshot().findAll({ role: "row" }).some((n) => n.label === "Alpha"), "the sheet has an Alpha row");

  // Bend the fade up: its ease becomes a drawn Bézier [x1, y1, x2, y2],
  // local to the fade, with its handles above the straight line's thirds.
  app.drag(node("slider", "Color fade in bend"), { dx: 0, dy: -20 }, { steps: 6 });
  settle();
  const bent = alpha();
  const ease = points(bent)[0][2];
  assert(Array.isArray(ease) && ease.length === 4, `bent: ${JSON.stringify(bent)}`);
  expect(ease[1]).toBeGreaterThan(1 / 3 + 1e-3);

  // Pull the hold of the line halfway down; the bend stays.
  const drop2 = travel() / 2;
  app.drag(node("slider", "Color alpha 2"), { dx: 0, dy: drop2 }, { steps: 6 });
  settle();
  const lowered = alpha();
  const curve = points(lowered);
  assert(close(curve[1][1], 0.5) && close(curve[2][1], 0.5), `lowered: ${JSON.stringify(curve)}`);
  assert(JSON.stringify(curve[0][2]) === JSON.stringify(ease), "the bend went with the level");

  // Undo takes the level back.
  app.key("secondary-z");
  settle();
  expect(alpha()).toEqual(bent);

  // Drag the fade back to the clip's edge: alpha is a plain value again.
  const edge = node("card", "Color").bounds.x;
  const fade = node("slider", "Color fade in");
  app.drag(fade, { dx: edge - (fade.bounds.x + fade.bounds.width / 2), dy: 0 }, { steps: 8 });
  settle();
  expect(alpha()).toEqual(FIXED);
});

test(
  "moving a clip over the end of another crosses them",
  {
    fixture: {
      clips: [
        // Beats 0–4 and 8–12.
        { pattern: "first", name: "Wash", start: 0, end: 2 },
        { pattern: "second", name: "Chase", start: 4, end: 6, preset: ["color.sparkle@1", "Random heads"] },
      ],
    },
  },
  () => {
    open();
    const beat = node("card", "Sparkle").bounds.width / 4;
    // From beats 8–12 to 2–6: two beats over the end of the first clip.
    app.drag(node("card", "Sparkle"), { dx: -6 * beat, dy: 0 }, { steps: 8 });
    settle();
    let score = stored();
    expect(close(score.clips.second.start, 2)).toBe(true);
    const out = points(score.clips.first.inputs.alpha);
    expect(out.length).toBe(3);
    assert(close(out[1][0], 0.5) && close(out[2][0], 1) && close(out[2][1], 0), `fade out: ${JSON.stringify(out)}`);
    const into = points(score.clips.second.inputs.alpha);
    assert(close(into[0][1], 0) && close(into[1][0], 0.5) && close(into[1][1], 1), `fade in: ${JSON.stringify(into)}`);

    // One undo takes back the move and both fades.
    app.key("secondary-z");
    settle();
    score = stored();
    expect(close(score.clips.second.start, 8)).toBe(true);
    expect(score.clips.first.inputs.alpha).toEqual(FIXED);
    expect(score.clips.second.inputs.alpha).toEqual(FIXED);
  },
);

test("a resize keeps the fade lengths", () => {
  open();
  // A one-beat fade-in on the four-beat clip, then its end pulled out four
  // beats further.
  const beat = node("card", "Color").bounds.width / 4;
  app.drag(node("slider", "Color fade in"), { dx: beat, dy: 0 }, { steps: 6 });
  settle();
  app.drag(node("slider", "Color end"), { dx: 4 * beat, dy: 0 }, { steps: 8 });
  settle();
  let clip = stored().clips["form-clip"];
  expect(close(clip.duration, 8)).toBe(true);
  const curve = points(clip.inputs.alpha);
  assert(close(curve[1][0], 0.125) && close(curve[1][1], 1), `the fade did not stay one beat long: ${JSON.stringify(curve)}`);

  // One undo takes back the resize and the refit together.
  app.key("secondary-z");
  settle();
  clip = stored().clips["form-clip"];
  expect(close(clip.duration, 4)).toBe(true);
  expect(close(points(clip.inputs.alpha)[1][0], 0.25)).toBe(true);
});

test("a segment of the line moves up and down", () => {
  open();
  // A fixed alpha is one segment. Dragged below the clip it stops at 0.
  const line = node("slider", "Color alpha 1");
  app.drag(line, { dx: 0, dy: 899 - (line.bounds.y + line.bounds.height / 2) }, { steps: 8 });
  settle();
  expect(alpha()).toEqual({ type: "proportion", value: 0.0 });

  // Back up to half, then a fade-in over the first quarter.
  const drop2 = -travel() / 2;
  app.drag(node("slider", "Color alpha 1"), { dx: 0, dy: drop2 }, { steps: 6 });
  settle();
  const card = node("card", "Color").bounds;
  app.drag(node("slider", "Color fade in"), { dx: card.width / 4, dy: 0 }, { steps: 6 });
  settle();
  const faded = points(alpha());
  assert(close(faded[0][1], 0) && close(faded[1][0], 0.25) && close(faded[1][1], 0.5), `faded: ${JSON.stringify(faded)}`);

  // Lift the ramp a quarter: both of its points move, and the line is a
  // custom curve with no fade handles.
  const drop4 = -travel() / 4;
  app.drag(node("slider", "Color alpha 1"), { dx: 0, dy: drop4 }, { steps: 6 });
  settle();
  const lifted = points(alpha());
  assert(close(lifted[0][1], 0.25) && close(lifted[1][1], 0.75) && close(lifted[2][1], 0.5), `lifted: ${JSON.stringify(lifted)}`);
  const sliders = app.snapshot().findAll({ role: "slider" }).map((n) => n.label).filter((l) => l.startsWith("Color "));
  assert(!sliders.some((l) => l.includes("fade in")), `a custom curve kept fade handles: ${sliders}`);
  expect(sliders).toContain("Color alpha 2");

  // One undo takes the lift back.
  app.key("secondary-z");
  settle();
  const undone = points(alpha());
  assert(close(undone[0][1], 0) && close(undone[1][1], 0.5), `undone: ${JSON.stringify(undone)}`);
});
