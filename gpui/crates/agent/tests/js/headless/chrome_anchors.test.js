// The window's two fixed corners: the anchors hold still, the title moves.
//
// The agent toggle is the left anchor and the mode switch the right one. A
// toggle rendered inside the panel it hides would be clipped away with it,
// and a title that reserved its room by asking "is the panel open?" would
// snap on the first frame of a slide — both read as "a button moved on its
// own". What is asserted is *position across a state change*: an anchor whose
// x moves at all has stopped being an anchor.

const WIDTH = 1280;

fixture({
  seconds: 20,
  clips: [{ pattern: "pattern-strobe", name: "Strobe", start: 2, end: 6 }],
  window: [WIDTH, 800],
});

// Both anchors, and the title bar's venue name, which the panel pushes.
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
  }
  // The right anchor sits at the window's trailing edge, inside a margin.
  const margin = WIDTH - open.modeSwitchRight;
  assert(margin >= 0 && margin <= 16, `the right anchor is not at the window's edge: ${open.modeSwitchRight}`);

  // The title is what moves. The open panel pushes the venue name right;
  // closing it returns the name against the left anchor.
  expect(open.venue).toBeGreaterThan(closed.venue);
  expect(reopened.venue).toBe(open.venue);
  assert(closed.venue >= closed.agentToggleRight && closed.venue - closed.agentToggleRight <= 16,
    `the title does not rest against the left anchor when the panel is shut: ${JSON.stringify(closed)}`);
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
    });
  }
  // The pushed title tracks the panel's edge, one way per slide: a jump is a
  // single frame going backwards.
  for (const [phase, forward] of [["closing", false], ["opening", true]]) {
    const xs = phases[phase].map((frame) => frame.venue).filter((x) => x !== null);
    for (let i = 1; i < xs.length; i++) {
      assert(forward ? xs[i] >= xs[i - 1] : xs[i] <= xs[i - 1], `the title reversed during ${phase}: ${xs}`);
    }
  }
});
