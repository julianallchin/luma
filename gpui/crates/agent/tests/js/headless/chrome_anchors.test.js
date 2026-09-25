// The window's two fixed corners: the toggles hold still, the clusters move.
//
// Three regressions live here, and all three read as "a button moved on its
// own". The sidebar toggle used to be rendered by whichever region was
// leftmost, so closing the sidebar clipped it away inside that region's
// shrinking pane and re-mounted it beside back/forward. The `+` had two homes
// for the same reason. And a cluster that reserved its room by asking "am I
// the leftmost region?" snapped left on the first frame of a slide. The `+`
// has one home now — the strip, which is the panel's — so what is asserted
// about it is that it leaves with the panel.
//
// What is asserted is *position across a state change*: an anchor whose x
// moves at all has stopped being an anchor.

const WIDTH = 1280;

fixture({
  seconds: 20,
  clips: [{ pattern: "pattern-strobe", name: "Strobe", start: 2, end: 6 }],
  window: [WIDTH, 800],
});

// Both toggles, the `+`, the strip's left edge and the thread's left cluster.
function read() {
  const shot = app.snapshot();
  const x = (role, label) => shot.find({ role, label })?.bounds.x ?? null;
  const sidebar = shot.find({ role: "button", label: "sidebar-toggle" });
  const panel = shot.find({ role: "button", label: "panel-toggle" });
  const search = shot.find({ role: "input", label: "Search tracks" });
  return {
    sidebarToggle: sidebar?.bounds.x ?? null,
    sidebarToggleRight: sidebar === undefined ? null : sidebar.bounds.x + sidebar.bounds.width,
    panelToggle: panel?.bounds.x ?? null,
    panelToggleRight: panel === undefined ? null : panel.bounds.x + panel.bounds.width,
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

test("the toggles do not move when the panels they open do", () => {
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
  const panelClosed = toggle("luma::ToggleWorkspace");
  const panelReopened = toggle("luma::ToggleWorkspace");
  const states = { bothOpen, sidebarClosed, sidebarReopened, panelClosed, panelReopened };

  // Every reading is a different panel configuration; the anchors are the
  // same pixel in all of them.
  for (const [name, state] of Object.entries(states)) {
    assert(state.sidebarToggle === bothOpen.sidebarToggle, `the sidebar toggle moved in ${name}: ${JSON.stringify(state)}`);
    assert(state.panelToggle === bothOpen.panelToggle, `the panel toggle moved in ${name}: ${JSON.stringify(state)}`);
  }
  // The right anchor sits at the window's trailing edge, inside a margin.
  const margin = WIDTH - bothOpen.panelToggleRight;
  assert(margin >= 0 && margin <= 16, `the right anchor is not at the window's edge: ${bothOpen.panelToggleRight}`);

  // The cluster is what moves. Opening the sidebar pushes back/forward right;
  // closing it returns the pair against the left anchor.
  expect(bothOpen.back).toBeGreaterThan(sidebarClosed.back);
  expect(sidebarReopened.back).toBe(bothOpen.back);
  assert(sidebarClosed.back >= sidebarClosed.sidebarToggleRight && sidebarClosed.back - sidebarClosed.sidebarToggleRight <= 16,
    `the cluster does not rest against the left anchor when the sidebar is shut: ${JSON.stringify(sidebarClosed)}`);

  // The strip is the panel's, and the `+` is the strip's: closing the panel
  // puts them away together.
  expect(panelClosed.strip).toBe(null);
  expect(panelClosed.add).toBe(null);
  for (const [name, state] of Object.entries({ bothOpen, panelReopened })) {
    assert(state.add > state.strip, `the add control parted company with its strip in ${name}`);
  }
});

// One frame per step of the slide, so a control clipped away part-way
// through is caught where it goes missing.
test("neither toggle blinks out part way through a slide", { fixture: { motion: true } }, () => {
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
    panelClosing: sample("luma::ToggleWorkspace", 16),
    panelOpening: sample("luma::ToggleWorkspace", 16),
  };
  for (const [phase, frames] of Object.entries(phases)) {
    frames.forEach((frame, i) => {
      assert(frame.sidebarToggle !== null, `the sidebar toggle vanished on frame ${i} of ${phase}`);
      assert(frame.panelToggle !== null, `the panel toggle vanished on frame ${i} of ${phase}`);
      assert(frame.sidebarToggle === frames[0].sidebarToggle, `the sidebar toggle drifted on frame ${i} of ${phase}`);
      assert(frame.panelToggle === frames[0].panelToggle, `the panel toggle drifted on frame ${i} of ${phase}`);
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
