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
// The dialogs these walk open: the thread's chat history over a score, and
// adding tracks from the song list.
const dialog = (s = app.snapshot()) =>
  s.find({ role: "card", label: "Chat history dialog" }) ?? s.find({ role: "card", label: "Add tracks dialog" });

// The window controls are the title bar's whether or not the agent is shown,
// and the page sits inset from the window's edge under it.
function framed(toggleLabel) {
  const shot = app.snapshot();
  const at = (role, label) => shot.find({ role, label })?.bounds;
  const bar = at("card", "Title bar");
  const page = at("card", "Page");
  for (const [role, label] of [["button", toggleLabel], ["card", "Modes"], ["button", "Test Venue"]]) {
    const control = at(role, label);
    assert(control && control.y >= bar.y - 1 && control.y + control.height <= bar.y + bar.height + 1
      && control.x >= bar.x - 1 && control.x + control.width <= bar.x + bar.width + 1,
      `${label} is not in the title bar: ${JSON.stringify({ control, bar })}`);
  }
  assert(page.x > 1 && page.x + page.width < WINDOW[0] - 1 && page.y >= bar.y + bar.height - 1
    && page.y + page.height < WINDOW[1] - 1,
    `the page is not inset from the window's edge: ${JSON.stringify({ page, bar })}`);
}

test("the agent panel toggles both ways and the seam resizes it", () => {
  open();
  const opened = read();
  assert(opened.agent > 0 && opened.body > 0 && opened.seam !== null, `the walk did not end with the panel open: ${JSON.stringify(opened)}`);

  // 1. Twice, because a toggle that breaks the keyboard's home breaks on the
  //    second press. Hidden, the panel takes its seam with it and the body
  //    gets the room.
  framed("Hide agent");
  const hidden = toggle("luma::ToggleAgent");
  expect(hidden.agent).toBe(null);
  expect(hidden.seam).toBe(null);
  expect(hidden.body).toBeGreaterThan(opened.body);
  until("the agent hidden", (s) => s.find({ role: "button", label: "Show agent" }));
  framed("Show agent");
  const reopened = toggle("luma::ToggleAgent");
  expect(reopened.seam).toBe(opened.seam);
  framed("Hide agent");

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
  app.click(app.snapshot().find({ role: "button", label: "Chat history" }));
  const shown = dialog(until("the chat history dialog", dialog)).bounds;
  assert(shown.x >= 0 && shown.x + shown.width <= WINDOW[0] && shown.y + shown.height <= WINDOW[1],
    `the dialog does not fit its window: ${JSON.stringify(shown)}`);
  app.click(app.snapshot().find({ role: "button", label: "Hide agent" }));
  app.frames(2);
  expect(read().agent).toBeGreaterThan(0);
  expect(dialog()).toBe(undefined);

  // The same modal boundary applies to key bindings, and the trap holds
  // both ways; escape closes the dialog.
  app.click(app.snapshot().find({ role: "button", label: "Chat history" }));
  until("the reopened chat history dialog", dialog);
  // Under the overlay ⌘B must not toggle the covered panel.
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
  expect(dialog()).toBe(undefined);
});

// The dialog clamps rather than crops on a compact window and leaves the
// titlebar strip, whose window controls keep their corner. On macOS the
// corner is AppKit's and holds no node of ours; elsewhere the painted lights
// are in it and keep their hit targets.
test("a compact window clamps a dialog below the titlebar", { fixture: { window: [640, 480] } }, () => {
  nav.venue("Test Venue");
  nav.songs();
  app.click(node("button", "Add track"));
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
