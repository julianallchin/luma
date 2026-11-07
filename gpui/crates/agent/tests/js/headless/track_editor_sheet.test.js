// The clip sheet on the graph page, driven end to end.
//
// 1. The selection's sheet reads the clip's blend mode and graph, and not its
//    span: bounds are edited on the timeline.
// 2. Every input is reachable: every row of the output card is inside the
//    sheet's own box.
// 3. A scalar input edit is a document write: it survives leaving the score
//    and coming back, which a repaint would not.
// 4. Each clip's sheet is its own.
// 5. A multi-selection of one graph shape batch-applies: the edit lands on
//    the clip that was not under the field.
// 6. A mixed selection offers only the name, and Escape leaves the page with
//    the selection kept.

// Two Wash clips and one Random heads. The Random heads sits early on its own
// lane: the sheet overlays the right of the canvas, and a clip the test has
// to click cannot live under it.
fixture({
  seconds: 20,
  clips: [
    { pattern: "pat-glow", name: "Glow", start: 2, end: 5 },
    { pattern: "pat-glow-2", name: "Glow", start: 8, end: 11 },
    { pattern: "pat-chase", name: "Chase", start: 4, end: 7, lane: 1, preset: "Random heads" },
  ],
});

const FORM = "Wash";
const BRIGHTNESS = "Color 1 brightness";

function open() {
  nav.track("Aurora");
  until("the timeline", (s) => s.find({ role: "card", label: "Waveform" }) !== undefined);
  nav.agentOff();
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
    sheet: shot.find({ role: "card", label: "Pattern graph" })?.bounds ?? null,
    waveform: shot.find({ role: "card", label: "Waveform" })?.bounds ?? null,
    inputs,
    rows,
    selects: shot.findAll({ role: "select" }).map((n) => n.label),
    texts: shot.findAll({ role: "text" }).map((n) => n.label),
  };
}

const untilInput = (name) =>
  until(`the sheet to show ${name}`, (s) => s.findAll({ role: "input" }).some((n) => n.label.startsWith(`${name} = `)));
const untilGone = () => until("the sheet to leave", (s) => s.find({ role: "card", label: "Pattern graph" }) === undefined);
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

// Show the selection's sheet on the graph page.
function sheetPage() {
  nav.graphPage();
}

// Back from the graph page to the timeline, the selection kept.
function timeline() {
  app.click(app.snapshot().find({ role: "button", label: "Back to timeline" }));
  until("the timeline", (s) => s.find({ role: "card", label: "Waveform" }) !== undefined);
}

test("the sheet arrives, writes, batches and narrows to what a selection shares", () => {
  nav.venue("Test Venue");
  app.frames(8);
  open();
  expect(readSheet().sheet).toBe(null);

  // 1. Select the first clip, open its sheet, and wait for the schema round
  //    trip.
  app.click(clip(FORM, 0));
  sheetPage();
  untilInput(BRIGHTNESS);
  app.frames(12, { waitMs: 30 });
  const populated = readSheet();
  const initial = populated.inputs[BRIGHTNESS];
  expect(populated.inputs.start).toBe(undefined);
  expect(populated.inputs.end).toBe(undefined);
  expect(populated.inputs.expression).toContain("all");
  expect(populated.selects).toContain("Replace");

  // 2. Every row of the output card is inside the sheet.
  for (const name of ["Blend", "Selection", "Color", "Brightness", "Alpha"]) {
    const row = populated.rows[name];
    assert(row !== undefined, `no ${name} row`);
    assert(row.x >= populated.sheet.x - 1 && row.x + row.width <= populated.sheet.x + populated.sheet.width + 1,
      `the ${name} row runs outside the sheet`);
  }

  // 3. The scalar edit shows at once, and survives a teardown and reopen.
  const next = String(Number(initial) === 50 ? 40 : 50);
  retype(BRIGHTNESS, next);
  const edited = readSheet();
  expect(edited.inputs[BRIGHTNESS]).toBe(next);
  timeline();
  nav.reopen("Aurora");
  app.frames(6);
  open();
  app.click(clip(FORM, 0));
  sheetPage();
  untilInput(BRIGHTNESS);
  app.frames(12, { waitMs: 30 });
  expect(readSheet().inputs[BRIGHTNESS]).toBe(next);

  // 4. Each clip has its own sheet: the second Wash still reads its own value.
  timeline();
  app.click(clip(FORM, 1));
  sheetPage();
  untilInput(BRIGHTNESS);
  expect(readSheet().inputs[BRIGHTNESS]).toBe(initial);

  // 5. Batch: select both clips of one shape, edit, then read the other one.
  timeline();
  app.click(clip(FORM, 0));
  app.click(clip(FORM, 1), { modifiers: ["shift"] });
  until("the batch selected", (s) => s.findAll({ role: "text" }).some((n) => n.label === "2 selected"));
  sheetPage();
  until("the batch count", (s) => s.findAll({ role: "text" }).some((n) => n.label === "2 patterns"));
  untilInput(BRIGHTNESS);
  const batch = String(Number(next) - 10);
  retype(BRIGHTNESS, batch);
  timeline();
  app.click(waveform());
  app.click(clip(FORM, 1));
  sheetPage();
  untilInput(BRIGHTNESS);
  expect(readSheet().inputs[BRIGHTNESS]).toBe(batch);

  // 6. Mixed: two graph shapes offer only the name.
  timeline();
  app.click(clip("Random heads", 0), { modifiers: ["shift"] });
  until("the mixed selection", (s) => s.findAll({ role: "text" }).some((n) => n.label === "2 selected"));
  sheetPage();
  until("the mixed readout", (s) => s.find({ role: "input", label: "Name" }) && !s.find({ role: "row", label: "Blend" }));
  expect(readSheet().inputs[BRIGHTNESS]).toBe(undefined);
  // Escape leaves the page and keeps the selection.
  app.key("escape");
  untilGone();
  until("the selection kept", (s) => s.findAll({ role: "text" }).some((n) => n.label === "2 selected"));
});
