// The shell's agent panel: what ⌘B does, and what its seam does.
//
// A panel closes the first time and never comes back, because hiding it left
// the keyboard with the body it stopped rendering. A seam drags the wrong way
// or by the wrong amount, because the gutter arithmetic between the pointer
// and the panel's edge is off by a gap.

// Wide enough for a 150 px drag either way inside both floors.
const WINDOW = [1600, 950];
fixture({
  seconds: 20,
  clips: [{ pattern: "pattern-strobe", name: "Strobe", start: 2, end: 6 }],
  window: WINDOW,
});

function read() {
  const shot = app.snapshot();
  return {
    seam: shot.find({ role: "slider", label: "Agent width" })?.bounds.x ?? null,
    body: shot.find({ role: "card", label: "Waveform" })?.bounds.width ?? null,
    agent: shot.find({ role: "card", label: "Agent" })?.bounds.width ?? null,
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

test("the agent panel toggles both ways and the seam resizes it", () => {
  open();
  const opened = read();
  assert(opened.agent > 0 && opened.body > 0 && opened.seam !== null, `the walk did not end with the panel open: ${JSON.stringify(opened)}`);

  // 1. Twice, because a toggle that breaks the keyboard's home breaks on the
  //    second press. Hidden, the panel takes its seam with it and the body
  //    gets the room.
  const hidden = toggle("luma::ToggleAgent");
  expect(hidden.agent).toBe(null);
  expect(hidden.seam).toBe(null);
  expect(hidden.body).toBeGreaterThan(opened.body);
  const reopened = toggle("luma::ToggleAgent");
  expect(reopened.seam).toBe(opened.seam);

  // 2. The seam, dragged, moves the body's edge by what the pointer moved.
  const seamDrag = (dx) => {
    const before = read();
    app.drag(app.snapshot().find({ role: "slider", label: "Agent width" }), { dx, dy: 0 }, { steps: 10 });
    app.frames(2);
    const after = read();
    assert(Math.abs(before.body - after.body - dx) <= 2,
      `a ${dx}px seam drag did not resize the body by what the pointer moved: ${before.body} → ${after.body}`);
    return after;
  };
  seamDrag(150);
  const narrowed = seamDrag(-150);

  // 2b. Dragged as far right as the window goes, the seam stops where the
  //     body's minimum begins instead of following the pointer.
  const grip = app.snapshot().find({ role: "slider", label: "Agent width" });
  app.drag(grip, { dx: WINDOW[0] - Math.round(grip.bounds.x) - 4, dy: 0 }, { steps: 10 });
  app.frames(2);
  const maxed = read();
  assert(maxed.seam > narrowed.seam && maxed.seam < WINDOW[0] - 100,
    `the seam did not stop at the body's minimum: ${narrowed.seam} → ${maxed.seam}`);

  // 3. Double-clicking the seam puts the panel back at its default width.
  app.click(app.snapshot().find({ role: "slider", label: "Agent width" }), { count: 2 });
  app.frames(2);
  assert(Math.abs(read().body - opened.body) <= 2, "double-clicking the seam did not restore the default width");

  // 4. An overlay owns its pointer plane: pressing the covered agent
  //    toggle's coordinates lands on the scrim, which dismisses the dialog
  //    and toggles nothing.
  app.action("luma::OpenSettings");
  const shown = dialog(until("the settings dialog", dialog)).bounds;
  assert(shown.x >= 0 && shown.x + shown.width <= WINDOW[0] && shown.y + shown.height <= WINDOW[1],
    `the dialog does not fit its window: ${JSON.stringify(shown)}`);
  app.click(app.snapshot().find({ role: "button", label: "Hide agent" }));
  app.frames(2);
  expect(read().agent).toBeGreaterThan(0);
  expect(dialog()).toBe(undefined);

  // The same modal boundary applies to key bindings, and the trap holds
  // both ways; escape restores the search field that opened it.
  nav.songs();
  app.click(app.snapshot().find({ role: "input", label: "Search tracks" }));
  app.action("luma::OpenSettings");
  until("the reopened settings dialog", dialog);
  // The song list has stood the panel aside; under the overlay ⌘B must not
  // bring it back either.
  const covered = read().agent;
  app.key("secondary-b");
  app.frames(2);
  assert(read().agent === covered, "the overlay let a shell shortcut toggle the covered panel");
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
// titlebar strip, whose window controls keep their corner. On macOS the
// corner is AppKit's and holds no node of ours; elsewhere the painted lights
// are in it and keep their hit targets.
test("a compact window clamps the settings dialog below the titlebar", { fixture: { window: [640, 480] } }, () => {
  open();
  app.action("luma::OpenSettings");
  const shot = until("the compact dialog", dialog);
  const box = dialog(shot).bounds;
  assert(box.x >= 0 && box.x + box.width <= 640 && box.y + box.height <= 480, `the dialog does not fit: ${JSON.stringify(box)}`);
  const corner = shot.find({ role: "card", label: "Window controls" });
  assert(corner !== undefined && corner.bounds.width > 0, "the window controls lost their corner above the compact modal");
  assert(box.y >= corner.bounds.y + corner.bounds.height, "the dialog covers the window controls' corner");
  const lights = ["close", "minimize", "maximize"].map((label) => shot.find({ role: "button", label }));
  assert(lights.every((node) => node === undefined) || lights.every((node) => node !== undefined && node.bounds.width > 0),
    "some painted lights lost their hit target above the compact modal");
});
