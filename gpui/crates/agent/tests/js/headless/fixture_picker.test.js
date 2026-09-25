// The fixture picker, driven from the strip it edits.
//
// The property pinned is the whole point of the dialog: ticking rows is a
// write. An LD opens the picker off a clip's selection cell, ticks two of the
// venue's groups and applies, and the clip's stored selection is the union of
// those two groups, read back through the strip's own expression field.
//
// The rig is on because the picker's rows are the venue's groups. No GPU
// here: the writing half of this dialog must work on a machine that cannot
// draw the room.

fixture({ seconds: 20, clips: [{ pattern: "pat-glow", name: "Glow", start: 2, end: 5 }], rig: 4, window: [1400, 900] });

test("ticking groups writes the union and escape writes nothing", () => {
  const expression = () => {
    const node = app.snapshot().findAll({ role: "input" }).find((n) => n.label.startsWith("expression = "));
    return node === undefined ? null : node.label.slice("expression = ".length);
  };
  const checkbox = (label) => app.snapshot().find({ role: "checkbox", label });
  const pickerOpen = (s) => s.find({ role: "checkbox", label: "left_movers" }) !== undefined;

  nav.trackEditor("Test Venue", "Aurora");
  until("the timeline", (s) => s.find({ role: "card", label: "Waveform" }) !== undefined);
  nav.expand();
  nav.stageOff();

  // A clip is labelled by its form; the fixture's clip plays Constant color.
  app.click(app.snapshot().find({ role: "card", label: "Constant color" }));
  until("the strip", (s) => s.findAll({ role: "input" }).some((n) => n.label.startsWith("expression = ")));
  // The default the pattern ships: the whole venue.
  expect(expression()).toBe("all");

  // The chip beside the expression field is the LD's way in.
  app.click(app.snapshot().find({ role: "button", label: "Pick fixtures" }));
  until("the picker's rows", pickerOpen);
  app.frames(4);
  // One row per group, and only the venue's groups.
  expect(app.snapshot().findAll({ role: "checkbox" }).map((n) => n.label)).toEqual(["left_movers", "right_movers"]);

  app.click(checkbox("left_movers"));
  app.frames(2);
  app.click(checkbox("right_movers"));
  app.frames(2);
  // The footer reads back what Apply would write, in tick order.
  const summary = app.snapshot().findAll({ role: "text" }).map((n) => n.label).filter((l) => l.includes("left_movers"));
  expect(summary).toEqual(["left_movers | right_movers"]);

  app.click(app.snapshot().find({ role: "button", label: "Apply" }));
  until("the picker to close", (s) => !pickerOpen(s));
  // The live edit lands at once; the write trails a 250 ms debounce.
  app.frames(8, { waitMs: 80 });
  expect(expression()).toBe("left_movers | right_movers");

  // Escape is not a quiet Apply: reopen, untick one, dismiss.
  app.click(app.snapshot().find({ role: "button", label: "Pick fixtures" }));
  until("the picker again", pickerOpen);
  app.click(checkbox("left_movers"));
  app.frames(2);
  app.key("escape");
  until("the picker to close", (s) => !pickerOpen(s));
  app.frames(8, { waitMs: 80 });
  expect(expression()).toBe("left_movers | right_movers");
});
