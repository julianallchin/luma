// The inputs sheet, driven end to end.
//
// 1. Selecting a clip brings the sheet in; with nothing selected the
//    inspector shows the preset browser. The sheet reads the clip's blend
//    mode and inputs, and not its span: bounds are edited on the timeline.
// 2. Every input is reachable: every row the form declares is inside the
//    sheet's own box.
// 3. A scalar input edit is a document write: it survives leaving the screen
//    and coming back, which a repaint would not.
// 4. A same-form multi-selection batch-applies: the edit lands on the clip
//    that was not under the field.
// 5. Retarget, not reopen: clicking a second clip while the sheet is up
//    leaves the sheet's box where it was and swaps its contents, and the
//    timeline stays live underneath.
// 6. A mixed selection offers no inputs, and clearing the selection brings
//    the preset browser back. The timeline keeps its space throughout.

// Two Constant color clips and one Chase. The Chase sits early on its own
// lane: the sheet overlays the right of the canvas, and a clip the test has
// to click cannot live under it.
fixture({
  seconds: 20,
  clips: [
    { pattern: "pat-glow", name: "Glow", start: 2, end: 5 },
    { pattern: "pat-glow-2", name: "Glow", start: 8, end: 11 },
    { pattern: "pat-chase", name: "Chase", start: 4, end: 7, lane: 1, preset: ["color.chase@1", "Chase"] },
  ],
});

const FORM = "Constant color";

function open() {
  nav.track("Aurora");
  until("the timeline", (s) => s.find({ role: "card", label: "Waveform" }) !== undefined);
  nav.expand();
  nav.stageOff();
  // The waveform lands before the score does.
  until("the clips", (s) => s.findAll({ role: "card", label: FORM }).length === 2);
}

// Everything the sheet says about itself.
function readSheet() {
  const shot = app.snapshot();
  const inputs = {};
  for (const node of shot.findAll({ role: "input" })) {
    const at = node.label.indexOf(" = ");
    if (at > 0) inputs[node.label.slice(0, at)] = node.label.slice(at + 3);
  }
  const rows = {};
  for (const node of shot.findAll({ role: "row" })) rows[node.label] = node.bounds;
  return {
    sheet: shot.find({ role: "card", label: "Clip inputs" })?.bounds ?? null,
    waveform: shot.find({ role: "card", label: "Waveform" })?.bounds ?? null,
    inputs,
    rows,
    selects: shot.findAll({ role: "select" }).map((n) => n.label),
    texts: shot.findAll({ role: "text" }).map((n) => n.label),
  };
}

const untilInput = (name) =>
  until(`the sheet to show ${name}`, (s) => s.findAll({ role: "input" }).some((n) => n.label.startsWith(`${name} = `)));
const untilGone = () => until("the sheet to leave", (s) => s.find({ role: "card", label: "Clip inputs" }) === undefined);
const clip = (label, index) =>
  app.snapshot().findAll({ role: "card", label }).sort((a, b) => a.bounds.x - b.bounds.x)[index];
const waveform = () => app.snapshot().find({ role: "card", label: "Waveform" });
const field = (name) => app.snapshot().findAll({ role: "input" }).find((n) => n.label.startsWith(`${name} = `));

// Focus a drafted field, replace its content, commit. Characters go through
// `app.type`: a bare digit keystroke is not text input.
function retype(name, digits) {
  app.click(field(name));
  app.key("cmd-a backspace");
  app.type(field(name), digits, { restale: "match" });
  app.key("enter");
  // The live edit lands at once; the write trails a 250 ms debounce.
  app.frames(8, { waitMs: 80 });
}

test("the sheet arrives, writes, batches, retargets and leaves", () => {
  nav.venue("Test Venue");
  app.frames(8);
  open();
  const empty = readSheet();
  expect(empty.sheet).toBe(null);
  expect(app.snapshot().find({ role: "card", label: "Presets" }) !== undefined).toBe(true);

  // 1. Select the first clip and wait for the schema round trip.
  app.click(clip(FORM, 0));
  untilInput("Brightness");
  app.frames(12, { waitMs: 30 });
  const populated = readSheet();
  const initial = populated.inputs.Brightness;
  expect(populated.inputs.start).toBe(undefined);
  expect(populated.inputs.end).toBe(undefined);
  expect(populated.inputs.expression).toContain("all");
  expect(populated.selects).toContain("replace");
  expect(populated.texts).toContain(FORM);

  // 2. Every row the form declares is inside the sheet.
  for (const name of ["Blend", "Selection", "Color", "Brightness", "Every"]) {
    const row = populated.rows[name];
    assert(row !== undefined, `no ${name} row`);
    assert(row.x >= populated.sheet.x - 1 && row.x + row.width <= populated.sheet.x + populated.sheet.width + 1,
      `the ${name} row runs outside the sheet`);
  }

  // 3. The scalar edit shows at once, and survives a teardown and reopen.
  const next = String(Number(initial) === 50 ? 40 : 50);
  retype("Brightness", next);
  const edited = readSheet();
  expect(edited.inputs.Brightness).toBe(next);
  nav.closeTab();
  app.frames(6);
  open();
  app.click(clip(FORM, 0));
  untilInput("Brightness");
  app.frames(12, { waitMs: 30 });
  const reopened = readSheet();
  expect(reopened.inputs.Brightness).toBe(next);

  // 4. Retarget in place: the same box, a different subject.
  app.click(clip(FORM, 1));
  app.frames(6, { waitMs: 30 });
  const retargeted = readSheet();
  expect(retargeted.sheet).toEqual(reopened.sheet);
  expect(retargeted.inputs.Brightness).toBe(initial);

  // 5. The timeline is live under an open sheet: a press on the waveform
  //    band clears the selection.
  app.click(waveform());
  untilGone();
  const throughClick = readSheet();

  // 6. Batch: select both clips of the form, edit, then read the other one.
  app.click(clip(FORM, 0));
  untilInput("Brightness");
  app.click(clip(FORM, 1), { modifiers: ["shift"] });
  until("the batch count", (s) => s.findAll({ role: "text" }).some((n) => n.label === `${FORM} (2)`));
  const batch = String(Number(next) - 10);
  retype("Brightness", batch);
  app.click(waveform());
  untilGone();
  app.click(clip(FORM, 1));
  untilInput("Brightness");
  expect(readSheet().inputs.Brightness).toBe(batch);

  // 7. Mixed: one of each form offers no inputs. Escape clears it.
  app.click(clip("Chase", 0), { modifiers: ["shift"] });
  until("the mixed readout", (s) => s.findAll({ role: "text" }).some((n) => n.label === "2 patterns"));
  const mixed = readSheet();
  expect(mixed.texts).toContain("Mixed patterns");
  expect(mixed.inputs.Brightness).toBe(undefined);
  app.key("escape");
  untilGone();
  const cleared = readSheet();
  expect(app.snapshot().find({ role: "card", label: "Presets" }) !== undefined).toBe(true);

  // The inspector stays open whatever is selected (the preset browser when
  // nothing is), so the timeline keeps its space through every state.
  for (const state of [populated, edited, retargeted, mixed, throughClick, cleared]) {
    expect(state.waveform).toEqual(empty.waveform);
  }
});
