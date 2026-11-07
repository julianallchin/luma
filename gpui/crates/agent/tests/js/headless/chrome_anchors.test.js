// The window's two fixed corners: the anchors and the title bar between them
// hold still.
//
// The agent toggle is the left anchor and the mode switch the right one, on
// the title bar that spans the window above the page. A toggle rendered
// inside the panel it hides would be clipped away with it, and a venue name
// that rode the panel's edge would read as "a button moved on its own". What
// is asserted is *position across a state change*: an anchor whose x moves at
// all has stopped being an anchor.

const WIDTH = 1280;

fixture({
  seconds: 20,
  clips: [{ pattern: "pattern-strobe", name: "Strobe", start: 2, end: 6 }],
  window: [WIDTH, 800],
});

// Both anchors, and the title bar's venue name beside the left one.
function read() {
  const shot = app.snapshot();
  const toggle = shot.find({ role: "button", label: "Hide agent" }) ?? shot.find({ role: "button", label: "Show agent" });
  const mode = shot.find({ role: "card", label: "Modes" });
  return {
    agentToggle: toggle?.bounds.x ?? null,
    agentToggleRight: toggle === undefined ? null : toggle.bounds.x + toggle.bounds.width,
    modeSwitch: mode?.bounds.x ?? null,
    modeSwitchRight: mode === undefined ? null : mode.bounds.x + mode.bounds.width,
    venue: shot.find({ role: "button", label: "Test Venue" })?.bounds.x ?? null,
    agentOpen: shot.find({ role: "card", label: "Agent" }) !== undefined,
  };
}

function toTimeline() {
  nav.trackEditor("Test Venue", "Aurora");
  until("the timeline", (s) => s.find({ role: "card", label: "Waveform" }) !== undefined);
}

test("the anchors do not move when the panel and the mode they change do", () => {
  toTimeline();
  const open = read();
  assert(open.agentOpen && open.venue !== null, `the walk did not end with the agent open: ${JSON.stringify(open)}`);

  const toggle = (action) => {
    app.action(action);
    app.frames(2);
    return read();
  };
  const closed = toggle("luma::ToggleAgent");
  const reopened = toggle("luma::ToggleAgent");
  const rig = toggle("luma::ShowRig");
  const live = toggle("luma::ShowLive");
  const arrange = toggle("luma::ShowArrange");
  const states = { open, closed, reopened, rig, live, arrange };

  // Every reading is a different configuration; the anchors are the same
  // pixel in all of them.
  for (const [name, state] of Object.entries(states)) {
    assert(state.agentToggle === open.agentToggle, `the agent toggle moved in ${name}: ${JSON.stringify(state)}`);
    assert(state.modeSwitch === open.modeSwitch, `the mode switch moved in ${name}: ${JSON.stringify(state)}`);
    assert(state.venue === open.venue, `the venue name moved in ${name}: ${JSON.stringify(state)}`);
  }
  // The right anchor sits at the window's trailing edge, inside a margin.
  const margin = WIDTH - open.modeSwitchRight;
  assert(margin >= 0 && margin <= 16, `the right anchor is not at the window's edge: ${open.modeSwitchRight}`);

  // The venue name rests against the left anchor, panel open or shut.
  assert(open.venue >= open.agentToggleRight && open.venue - open.agentToggleRight <= 16,
    `the title does not rest against the left anchor: ${JSON.stringify(open)}`);
});

// One frame per step of the slide, so a control clipped away part-way
// through is caught where it goes missing.
test("neither anchor blinks out part way through a slide", { fixture: { motion: true } }, () => {
  toTimeline();
  const sample = (steps) => {
    app.action("luma::ToggleAgent");
    const seen = [];
    for (let i = 0; i < steps; i++) {
      app.frames(1, { waitMs: 12 });
      seen.push(read());
    }
    return seen;
  };
  const phases = { closing: sample(16), opening: sample(16) };
  for (const [phase, frames] of Object.entries(phases)) {
    frames.forEach((frame, i) => {
      assert(frame.agentToggle !== null, `the agent toggle vanished on frame ${i} of ${phase}`);
      assert(frame.modeSwitch !== null, `the mode switch vanished on frame ${i} of ${phase}`);
      assert(frame.agentToggle === frames[0].agentToggle, `the agent toggle drifted on frame ${i} of ${phase}`);
      assert(frame.modeSwitch === frames[0].modeSwitch, `the mode switch drifted on frame ${i} of ${phase}`);
      assert(frame.venue === frames[0].venue, `the venue name drifted on frame ${i} of ${phase}`);
    });
  }
});
