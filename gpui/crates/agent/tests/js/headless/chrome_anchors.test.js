// The window's two fixed corners: the anchors hold still, the clusters move.
//
// Three regressions live here, and all three read as "a button moved on its
// own". The sidebar toggle used to be rendered by whichever region was
// leftmost, so closing the sidebar clipped it away inside that region's
// shrinking pane and re-mounted it beside back/forward. The `+` had two homes
// for the same reason. And a cluster that reserved its room by asking "am I
// the leftmost region?" snapped left on the first frame of a slide. The `+`
// has one home now — the strip, which is the panel's — so what is asserted
// about it is that it stays with the strip.
//
// What is asserted is *position across a state change*: an anchor whose x
// moves at all has stopped being an anchor.

const WIDTH = 1280;

fixture({
  seconds: 20,
  clips: [{ pattern: "pattern-strobe", name: "Strobe", start: 2, end: 6 }],
  window: [WIDTH, 800],
});

// Both anchors, the `+`, the strip's left edge and the thread's left cluster.
function read() {
  const shot = app.snapshot();
  const x = (role, label) => shot.find({ role, label })?.bounds.x ?? null;
  const sidebar = shot.find({ role: "button", label: "sidebar-toggle" });
  const mode = shot.find({ role: "card", label: "View mode" });
  const search = shot.find({ role: "input", label: "Search tracks" });
  return {
    sidebarToggle: sidebar?.bounds.x ?? null,
    sidebarToggleRight: sidebar === undefined ? null : sidebar.bounds.x + sidebar.bounds.width,
    modeSwitch: mode?.bounds.x ?? null,
    modeSwitchRight: mode === undefined ? null : mode.bounds.x + mode.bounds.width,
    add: x("button", "new-tab"),
    strip: x("card", "Tab strip"),
    back: x("button", "Back"),
    sidebarOpen: search !== undefined && search.bounds.width > 0,
  };
}

function toTimeline() {
  nav.trackEditor("Test Venue", "Aurora");
  until("the timeline", (s) => s.find({ role: "card", label: "Waveform" }) !== undefined);
}

test("the anchors do not move when the panels and the layout they change do", () => {
  toTimeline();
  const bothOpen = read();
  assert(bothOpen.sidebarOpen && bothOpen.add !== null, `the walk did not end with both regions open: ${JSON.stringify(bothOpen)}`);

  const toggle = (action) => {
    app.action(action);
    app.frames(2);
    return read();
  };
  const sidebarClosed = toggle("luma::ToggleSidebar");
  const sidebarReopened = toggle("luma::ToggleSidebar");
  const program = toggle("luma::ProgramMode");
  const direct = toggle("luma::DirectMode");
  const states = { bothOpen, sidebarClosed, sidebarReopened, program, direct };

  // Every reading is a different configuration; the anchors are the same
  // pixel in all of them.
  for (const [name, state] of Object.entries(states)) {
    assert(state.sidebarToggle === bothOpen.sidebarToggle, `the sidebar toggle moved in ${name}: ${JSON.stringify(state)}`);
    assert(state.modeSwitch === bothOpen.modeSwitch, `the mode switch moved in ${name}: ${JSON.stringify(state)}`);
  }
  // The right anchor sits at the window's trailing edge, inside a margin.
  const margin = WIDTH - bothOpen.modeSwitchRight;
  assert(margin >= 0 && margin <= 16, `the right anchor is not at the window's edge: ${bothOpen.modeSwitchRight}`);

  // The cluster is what moves. Opening the sidebar pushes back/forward right;
  // closing it returns the pair against the left anchor.
  expect(bothOpen.back).toBeGreaterThan(sidebarClosed.back);
  expect(sidebarReopened.back).toBe(bothOpen.back);
  assert(sidebarClosed.back >= sidebarClosed.sidebarToggleRight && sidebarClosed.back - sidebarClosed.sidebarToggleRight <= 16,
    `the cluster does not rest against the left anchor when the sidebar is shut: ${JSON.stringify(sidebarClosed)}`);

  // The strip spans the tab's chat and its editor, so taking the chat away
  // leaves it where it was, and the `+` is the strip's: it stays with it.
  expect(program.strip).toBe(bothOpen.strip);
  for (const [name, state] of Object.entries({ bothOpen, program, direct })) {
    assert(state.add > state.strip, `the add control parted company with its strip in ${name}`);
    // …and the strip stops short of the switch rather than running under it.
    assert(state.add < state.modeSwitch, `the + ran under the mode switch in ${name}`);
  }
});

// One frame per step of the slide, so a control clipped away part-way
// through is caught where it goes missing.
test("neither anchor blinks out part way through a slide", { fixture: { motion: true } }, () => {
  toTimeline();
  const sample = (action, steps) => {
    app.action(action);
    const seen = [];
    for (let i = 0; i < steps; i++) {
      app.frames(1, { waitMs: 12 });
      seen.push(read());
    }
    return seen;
  };
  const phases = {
    closing: sample("luma::ToggleSidebar", 16),
    opening: sample("luma::ToggleSidebar", 16),
  };
  for (const [phase, frames] of Object.entries(phases)) {
    frames.forEach((frame, i) => {
      assert(frame.sidebarToggle !== null, `the sidebar toggle vanished on frame ${i} of ${phase}`);
      assert(frame.modeSwitch !== null, `the mode switch vanished on frame ${i} of ${phase}`);
      assert(frame.sidebarToggle === frames[0].sidebarToggle, `the sidebar toggle drifted on frame ${i} of ${phase}`);
      assert(frame.modeSwitch === frames[0].modeSwitch, `the mode switch drifted on frame ${i} of ${phase}`);
    });
  }
  // The pushed cluster tracks the panel's edge, one way per slide: the jump
  // this rework removed was a single frame going backwards.
  for (const [phase, forward] of [["closing", false], ["opening", true]]) {
    const xs = phases[phase].map((frame) => frame.back).filter((x) => x !== null);
    for (let i = 1; i < xs.length; i++) {
      assert(forward ? xs[i] >= xs[i - 1] : xs[i] <= xs[i - 1], `the cluster reversed during ${phase}: ${xs}`);
    }
  }
});
