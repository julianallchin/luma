// Escape inside a drafted field drops the draft and nothing else.
//
// gpui runs key bindings before key listeners. A field that heard escape as a
// plain key event lost it to any escape binding around it that does not
// exclude text fields — a dialog's dismiss, the stage's cancel. Under the
// field's draft context the field takes escape first.

fixture({
  seconds: 20,
  clips: [{ pattern: "pat-glow", name: "Glow", start: 2, end: 5 }],
});

const field = (name) => app.snapshot().findAll({ role: "input" }).find((n) => n.label.startsWith(`${name} = `));
const value = (name) => field(name).label.slice(name.length + 3);

// Draft `text` into the field and press escape; the field reads its value
// again. What else escape did is the caller's check.
function escapeDraft(name, text) {
  const before = value(name);
  app.click(field(name));
  app.key("secondary-a backspace");
  app.type(field(name), text, { restale: "match" });
  until(`the ${name} draft`, () => value(name) === text);
  app.key("escape");
  until(`the ${name} draft to drop`, () => value(name) === before);
  app.frames(4);
}

// A dialog binds escape to dismiss inside every child, text fields included.
test("escape in the add dialog's count drops the draft and keeps the page",
  { fixture: { seconds: 20, rig: 4, window: [1500, 950] } }, () => {
  nav.patch("Test Venue");
  nav.step("patch details", "button", "Patch details");
  nav.expand();
  nav.stageOff();
  until("the patch table", (s) => s.find({ role: "row", label: "Mover 0" }) !== undefined);
  app.click(app.snapshot().find({ role: "button", label: "Add fixtures" }));
  const bundle = until("the bundle", (s) => s.find({ role: "input", label: "Search fixtures…" }) !== undefined);
  app.type(bundle.find({ role: "input", label: "Search fixtures…" }), "Mover");
  until("the seeded definition", (s) => s.find({ role: "row", label: "Luma Mover" }) !== undefined);
  app.click(app.snapshot().find({ role: "row", label: "Luma Mover" }));
  until("the count page", () => field("count") !== undefined);
  app.frames(6);

  // Escape on the count page steps the dialog back to the bundle.
  escapeDraft("count", "7");
  assert(field("count") !== undefined, "escape in the count field also stepped the dialog back");

  // The field is still focused but clean, so the next escape is the dialog's.
  app.key("escape");
  until("the dialog to step back", () => field("count") === undefined);
});

// The expression field reverts and leaves; the clip stays selected.
test("escape drops an expression draft and keeps the selection", () => {
  nav.venue("Test Venue");
  nav.track("Aurora");
  until("the timeline", (s) => s.find({ role: "card", label: "Waveform" }) !== undefined);
  nav.expand();
  nav.stageOff();
  until("the clip", (s) => s.find({ role: "card", label: "Constant color" }) !== undefined);
  app.click(app.snapshot().find({ role: "card", label: "Constant color" }));
  until("the sheet", () => field("expression") !== undefined);

  escapeDraft("expression", "nowhere");
  assert(app.snapshot().find({ role: "card", label: "Clip inputs" }) !== undefined,
    "escape in the expression field also cleared the selection");
});
