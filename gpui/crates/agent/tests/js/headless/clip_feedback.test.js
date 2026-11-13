// Marking a clip as bad from its menu.
//
// - A right-click on a clip raises the context menu, not the pattern picker
//   a right-click on empty lane opens.
// - "Mark as bad" records the verdict and leaves the clip where it is; the
//   row then reads "Unmark as bad", and still does after the score is opened
//   again.
// - "Unmark as bad" takes it back.

const CLIP = "Clouds";
fixture({
  seconds: 20,
  clips: [{ pattern: "pattern-haze", name: "Haze", start: 2, end: 6, lane: 0, preset: CLIP }],
  window: [1400, 900],
});

const clips = (s) => s.findAll({ role: "card", label: CLIP });
const menu = (s) => s.find({ role: "card", label: "Context menu" });
const item = (s, label) => s.find({ role: "button", label });
const picker = (s) => s.find({ role: "card", label: "Insert pattern dialog" });

function open() {
  nav.track("Aurora");
  until("the timeline", (s) => s.find({ role: "card", label: "Waveform" }) !== undefined);
  nav.stageOff();
  until("the clip", (s) => clips(s).length === 1);
}

function begin() {
  nav.venue("Test Venue");
  open();
}

// The menu's verdict row, whichever way it currently reads.
function verdictRow() {
  app.click(clips(app.snapshot())[0], { button: "right" });
  const shot = until("the clip menu", (s) => menu(s) !== undefined);
  expect(picker(shot)).toBe(undefined);
  return shot;
}

test("a clip's menu marks it as bad, keeps the clip, and remembers", () => {
  begin();
  const first = verdictRow();
  expect(item(first, "Unmark as bad")).toBe(undefined);
  app.click(item(first, "Mark as bad"));
  until("the menu to close", (s) => menu(s) === undefined);
  expect(clips(app.snapshot()).length).toBe(1);

  const marked = verdictRow();
  expect(item(marked, "Mark as bad")).toBe(undefined);
  assert(item(marked, "Unmark as bad") !== undefined, "a marked clip's menu does not offer to unmark it");
  app.key("escape");
  until("the menu to close", (s) => menu(s) === undefined);

  // Stored, not only shown: a fresh read of the score says the same.
  nav.reopen("Aurora");
  open();
  const reopened = verdictRow();
  app.click(item(reopened, "Unmark as bad"));
  until("the menu to close", (s) => menu(s) === undefined);
  expect(clips(app.snapshot()).length).toBe(1);

  nav.reopen("Aurora");
  open();
  const cleared = verdictRow();
  assert(item(cleared, "Mark as bad") !== undefined, "an unmarked clip's menu does not offer to mark it");
  expect(item(cleared, "Unmark as bad")).toBe(undefined);
});

// The picker is one row away on a clip, and where it always was off one.
test("a clip's menu still offers the insertion a right-click on empty lane opens", () => {
  begin();
  app.click(item(verdictRow(), "Insert pattern"));
  until("the pattern picker", (s) => picker(s) !== undefined);
  expect(menu(app.snapshot())).toBe(undefined);
  app.key("escape");
  until("the picker to close", (s) => picker(s) === undefined);

  app.click(app.snapshot().find({ role: "row", label: "Lane 0" }), { button: "right" });
  const direct = until("the pattern picker", (s) => picker(s) !== undefined);
  expect(menu(direct)).toBe(undefined);
});
