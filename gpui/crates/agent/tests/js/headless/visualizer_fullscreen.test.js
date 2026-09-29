// Fullscreen is presentation of the same score, with a reversible layout.

fixture({ seconds: 20, rig: 4, window: [1400, 900] });

const node = (role, label) => app.snapshot().find({ role, label });
const cards = (labels) => {
  const shot = app.snapshot();
  return Object.fromEntries(labels.map((label) => [label, shot.find({ role: "card", label })?.bounds ?? null]));
};

test(
  "fullscreen restores the selection and layout and keeps the transport running",
  { fixture: { seconds: 60, clips: [{ pattern: "pat-glow", name: "Glow", start: 2, end: 50 }] } },
  () => {
    // A clip is labelled by its form.
    const CLIP = "Wash";
    const LAYOUT = ["Sidebar", "Waveform", "Clip graph", "Stage"];
    nav.trackEditor("Test Venue", "Aurora");
    until("the clip", () => node("card", CLIP));
    // In the track editor the stage carries no visualizer toolbar.
    expect(node("card", "Visualizer toolbar")).toBe(undefined);
    app.click(node("card", CLIP));
    until("the inspector", () => node("card", "Clip graph"));
    const before = cards(LAYOUT);
    app.click(node("button", "Play"));
    until("playing", () => node("button", "Pause"));

    app.click(node("button", "Fullscreen visualizer"));
    until("fullscreen", () => !node("card", "Waveform"));
    // The whole window, one stage, the editors hidden, the transport kept.
    expect(node("card", "Fullscreen visualizer").bounds).toEqual({ x: 0, y: 0, width: 1400, height: 900 });
    expect(app.snapshot().findAll({ role: "card", label: "Stage" }).length).toBe(1);
    for (const label of ["Sidebar", "Waveform", "Clip graph"]) expect(node("card", label)).toBe(undefined);
    assert(node("button", "Pause") && node("card", "Visualizer toolbar"), "fullscreen lost the transport");
    const time = () => app.snapshot().findAll({ role: "text" }).find((n) => /^\d+:\d\d \/ \d+:\d\d$/.test(n.label))?.label;
    const firstTime = time();
    app.frames(16, { waitMs: 80 });
    assert(time() !== firstTime, `playback did not advance: ${firstTime}`);
    app.key("space");
    until("paused", () => node("button", "Play"));
    app.key("space");
    until("playing again", () => node("button", "Pause"));

    // These belong to hidden editors and must not delete the selected clip,
    // close its tab, or change the remembered panel layout.
    app.key("delete ctrl-w cmd-w ctrl-b cmd-b ctrl-shift-v cmd-shift-v");
    expect(node("button", "Exit fullscreen") !== undefined).toBe(true);
    // The first escape closes the settings popover, not fullscreen.
    app.click(node("toggle", "Render settings"));
    until("the render settings", () => node("card", "Render settings"));
    app.key("escape");
    until("the settings closed", () => !node("card", "Render settings"));
    expect(node("button", "Exit fullscreen") !== undefined).toBe(true);
    app.key("escape");
    until("the editor restored", () => node("card", "Waveform"));
    expect(cards(LAYOUT)).toEqual(before);
    expect(node("button", "Pause") !== undefined).toBe(true);
    expect(node("card", CLIP) !== undefined).toBe(true);

    // The keyboard door, and the button out.
    app.key("shift-f");
    until("keyboard fullscreen", () => node("button", "Exit fullscreen"));
    app.click(node("button", "Exit fullscreen"));
    until("the button's exit", () => node("card", "Waveform"));
  },
);

test("the fullscreen venue has no centre bar and restores the authoring controls", { fixture: { window: [1200, 800] } }, () => {
  nav.patch("Test Venue");
  until("the venue controls", () => node("toggle", "Stage objects"));
  expect(node("card", "Visualizer toolbar") !== undefined).toBe(true);
  app.click(node("button", "Fullscreen visualizer"));
  until("fullscreen", () => node("button", "Exit fullscreen"));
  expect(node("card", "Visualizer toolbar")).toBe(undefined);
  expect(node("toggle", "Stage objects")).toBe(undefined);
  // Authoring keys do nothing while presenting.
  app.key("a w e delete");
  expect(node("input", "Search elements")).toBe(undefined);
  app.key("escape");
  until("the controls restored", () => node("toggle", "Stage objects"));
});

test("fullscreen passes through intermediate geometry and reverses during entry", { fixture: { motion: true, motion_scale: 4 } }, () => {
  nav.trackEditor("Test Venue", "Aurora");
  app.frames(40, { waitMs: 50 });
  const before = node("card", "Stage").bounds;
  app.click(node("button", "Fullscreen visualizer"));
  app.frames(3, { waitMs: 25 });
  const middle = node("card", "Fullscreen visualizer").bounds;
  assert(middle.width > before.width && middle.width < 1400, `no intermediate frame: ${before.width} → ${middle.width}`);
  app.key("escape");
  until("reversed to the editor", () => !node("card", "Fullscreen visualizer"));
  expect(node("card", "Stage").bounds).toEqual(before);
  app.key("shift-f");
  until("fully expanded", () => node("card", "Fullscreen visualizer")?.bounds.width >= 1399.9);
  assert(Math.abs(node("card", "Stage").bounds.width - 1400) < 1, "the stage did not fill the window");
  app.key("escape");
  until("closed", () => !node("card", "Fullscreen visualizer"));
});
