// The fixture picker under a real renderer: the room, lit by the selection.
// Nothing ticked lights the whole rig (that is what `all` means); ticking one
// group must change the picture — a picture that did not change would mean
// the highlight never reached the renderer, which no headless test can see.

fixture({
  seconds: 20,
  clips: [{ pattern: "pat-glow", name: "Glow", start: 2, end: 5 }],
  rig: 4,
  window: [1400, 900],
});

const checkbox = (label) => app.snapshot().find({ role: "checkbox", label });
// The preview only exists once a frame has come back.
const lit = (s) => s.find({ role: "card", label: "Selection preview" }) !== undefined;

test("ticking a group changes the picture", { timeoutMs: 120000 }, () => {
  nav.trackEditor("Test Venue", "Aurora");
  until("the timeline", (s) => s.find({ role: "card", label: "Waveform" }) !== undefined);
  nav.expand();
  nav.stageOff();

  // A clip is labelled by its form; the fixture's clip plays Color.
  nav.step("the clip", "card", "Color");
  until("the strip", (s) => s.findAll({ role: "input" }).some((n) => n.label.startsWith("expression = ")));
  nav.step("the fixture picker", "button", "Pick fixtures");
  until("the picker", (s) => checkbox("left_movers") !== undefined);
  until("the whole rig lit", lit);
  app.frames(10, { waitMs: 40 });
  const whole = app.screenshot();
  image.keep(whole, "fixture-picker/picker-dialog");

  app.click(checkbox("left_movers"));
  app.frames(6, { waitMs: 40 });
  until("the half-rig frame", lit);
  app.frames(10, { waitMs: 40 });
  const half = app.screenshot();
  image.keep(half, "fixture-picker/picker-dialog-one-group");

  // Half the rig going dark is a large, structural change; this floor still
  // fails a picture that never re-rendered.
  expect(image.diff(whole, half, { threshold: 1 })).toBeGreaterThan(0.002);
});
