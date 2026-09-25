// The shell's edge regions: what ⌘B and ⌘⇧B do, and what the workspace seam
// does.
//
// A panel closes the first time and never comes back, because hiding it left
// the keyboard with the tab it stopped rendering. A slide stalls part-way,
// because a manually driven tween only advances while somebody asks for the
// next frame. A seam drags the wrong way or by the wrong amount, because the
// gutter arithmetic between the pointer and the card's edge is off by a gap.

// Wide enough for a 150 px drag before chat reaches its minimum.
const WINDOW = [1600, 950];
fixture({
  seconds: 20,
  clips: [{ pattern: "pattern-strobe", name: "Strobe", start: 2, end: 6 }],
  window: WINDOW,
});

function read() {
  const shot = app.snapshot();
  // By label: the composer is an input too.
  const search = shot.find({ role: "input", label: "Search tracks" });
  return {
    seam: shot.find({ role: "slider", label: "Workspace width" })?.bounds.x ?? null,
    tab: shot.find({ role: "card", label: "Waveform" })?.bounds.width ?? null,
    // The sidebar is on screen exactly when its search field is.
    sidebar: search?.bounds.width ?? null,
  };
}

function open() {
  nav.trackEditor("Test Venue", "Aurora");
  until("the timeline", (s) => s.find({ role: "card", label: "Waveform" }) !== undefined);
}

const toggle = (action) => {
  app.action(action);
  app.frames(2);
  return read();
};
const dialog = (s = app.snapshot()) => s.find({ role: "card", label: "Settings dialog" });

test("the edge regions toggle both ways and the seam resizes the panel", () => {
  open();
  const opened = read();
  assert(opened.sidebar > 0 && opened.tab > 0, `the walk did not end with both regions open: ${JSON.stringify(opened)}`);

  // 1. Twice each, because a toggle that breaks the keyboard's home breaks on
  //    the second press.
  expect(toggle("luma::ToggleSidebar").sidebar).toBe(null);
  expect(toggle("luma::ToggleSidebar").sidebar).toBeGreaterThan(0);
  // 2. The workspace panel, likewise.
  expect(toggle("luma::ToggleWorkspace").tab).toBe(null);
  const workspaceReopened = toggle("luma::ToggleWorkspace");
  expect(workspaceReopened.tab).toBeGreaterThan(0);

  // 3. The seam, dragged left, widens the panel by what the pointer moved.
  //    Sidebar closed first, and read after it went: closing it hands its
  //    width to both neighbours.
  //    Right first, to make room: at rest the thread sits near its minimum.
  const seamDrag = (dx) => {
    const before = read();
    app.drag(app.snapshot().find({ role: "slider", label: "Workspace width" }), { dx, dy: 0 }, { steps: 10 });
    app.frames(2);
    const after = read();
    assert(Math.abs(before.tab - after.tab - dx) <= 2,
      `a ${dx}px seam drag did not resize the panel by what the pointer moved: ${before.tab} → ${after.tab}`);
    return after;
  };
  toggle("luma::ToggleSidebar");
  seamDrag(150);
  const widened = seamDrag(-150);

  // 3b. Dragged as far left as the window goes, the seam stops where the
  //     thread column's minimum begins instead of following the pointer.
  const grip = app.snapshot().find({ role: "slider", label: "Workspace width" });
  app.drag(grip, { dx: -Math.round(grip.bounds.x) + 4, dy: 0 }, { steps: 10 });
  app.frames(2);
  const maxed = read();
  assert(maxed.seam > 100 && maxed.seam < widened.seam,
    `the seam did not stop at the thread's minimum: ${widened.seam} → ${maxed.seam}`);
  app.action("luma::ToggleSidebar");
  until("the sidebar after the clamped drag", (s) => s.find({ role: "input", label: "Search tracks" })?.bounds.width > 0);

  // 4. Double-clicking the seam puts the panel back at its default width,
  //    compared in the same arrangement.
  app.click(app.snapshot().find({ role: "slider", label: "Workspace width" }), { count: 2 });
  app.frames(2);
  assert(Math.abs(read().tab - workspaceReopened.tab) <= 2, "double-clicking the seam did not restore the default width");

  // 5. An overlay owns its pointer plane: pressing the covered sidebar
  //    toggle's coordinates lands on the scrim, which dismisses the dialog
  //    and toggles nothing.
  app.click(app.snapshot().find({ role: "input", label: "Search tracks" }));
  app.action("luma::OpenSettings");
  const shown = dialog(until("the settings dialog", dialog)).bounds;
  assert(shown.x >= 0 && shown.x + shown.width <= WINDOW[0] && shown.y + shown.height <= WINDOW[1],
    `the dialog does not fit its window: ${JSON.stringify(shown)}`);
  app.click(app.snapshot().find({ role: "button", label: "sidebar-toggle" }));
  app.frames(2);
  expect(read().sidebar).toBeGreaterThan(0);
  expect(dialog()).toBe(undefined);

  // The same modal boundary applies to key bindings, and the trap holds
  // both ways; escape restores the search field that opened it.
  app.action("luma::OpenSettings");
  until("the reopened settings dialog", dialog);
  app.key("secondary-b");
  app.frames(2);
  assert(read().sidebar > 0, "the overlay let a shell shortcut close the covered sidebar");
  app.key("tab");
  app.frames(2);
  assert(dialog()?.focused === true, "Tab escaped the modal focus plane");
  app.key("shift-tab");
  app.frames(2);
  app.key("shift-tab");
  app.frames(2);
  assert(dialog()?.focused === true, "Shift-Tab escaped the modal focus trap");
  app.key("escape");
  app.frames(2);
  expect(app.snapshot().find({ role: "input", label: "Search tracks" })?.focused).toBe(true);
});

// The dialog clamps rather than crops on a compact window and leaves the
// titlebar strip, whose window controls keep their hit targets.
test("a compact window clamps the settings dialog below the titlebar", { fixture: { window: [640, 480] } }, () => {
  open();
  app.action("luma::OpenSettings");
  const shot = until("the compact dialog", dialog);
  const box = dialog(shot).bounds;
  assert(box.x >= 0 && box.x + box.width <= 640 && box.y + box.height <= 480, `the dialog does not fit: ${JSON.stringify(box)}`);
  for (const control of ["close", "minimize", "maximize"]) {
    const node = shot.find({ role: "button", label: control });
    assert(node !== undefined && node.bounds.width > 0, `${control} lost its hit target above the compact modal`);
    assert(box.y >= node.bounds.y + node.bounds.height, `the dialog covers the titlebar's ${control}`);
  }
});

// ⌘B takes its width from the thread and the panel in the ratio they were
// already at: the split is stored as a proportion. Motion is on for the
// frames in between: a derived width that tweened toward its own moving
// target would trail the sidebar and then snap.
test("toggling the sidebar keeps the thread and panel at the same ratio", { fixture: { window: [1600, 800], motion: true } }, () => {
  const width = 1600;
  // Where the pair begins: past the sidebar's live edge, or at the window's
  // edge while the sidebar is away.
  const sidebarEdge = (shot) => {
    const sidebar = shot.find({ role: "card", label: "Sidebar" });
    return sidebar === undefined || sidebar.bounds.width <= 0 ? 0 : sidebar.bounds.x + sidebar.bounds.width + 1;
  };
  const share = (shot = app.snapshot()) => {
    const seam = shot.find({ role: "slider", label: "Workspace width" });
    if (seam === undefined) return null;
    const thread = seam.bounds.x - sidebarEdge(shot);
    const panel = width - seam.bounds.x - 1;
    return thread <= 0 || panel <= 0 ? null : panel / (thread + panel);
  };
  const slide = () => {
    app.action("luma::ToggleSidebar");
    const seen = [];
    for (let i = 0; i < 14; i++) {
      app.frames(1, { waitMs: 12 });
      const now = share();
      if (now !== null) seen.push(now);
    }
    return seen;
  };
  open();
  app.frames(8, { waitMs: 40 });
  const rest = share();
  expect(typeof rest).toBe("number");
  const closing = slide();
  const closed = share();
  const opening = slide();
  const reopened = share();
  for (const [state, value] of Object.entries({ closed, reopened })) {
    assert(Math.abs(value - rest) < 0.01, `the split moved when the sidebar did: ${rest} then ${value} (${state})`);
  }
  // …and it never wandered on the way.
  for (const [phase, frames] of Object.entries({ closing, opening })) {
    assert(frames.length >= 4, `too few ${phase} frames: ${frames}`);
    frames.forEach((value, i) =>
      assert(Math.abs(value - rest) < 0.02, `the split wandered on frame ${i} of ${phase}: wanted ${rest}, got ${value}`));
  }
});
