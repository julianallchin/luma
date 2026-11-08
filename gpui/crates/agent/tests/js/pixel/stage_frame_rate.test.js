// Settings → General → "Show frame rate on the stage" hides the frame rate in
// the stage's corner, at once on the open stage and on every stage opened
// after, and brings it back. Pixel-only: headless has no live stage, so the
// corner readout never draws there.

fixture({
  seconds: 4,
  rig: 2,
  sql: ["INSERT INTO venues (id, uid, name) VALUES ('venue-spare', '$PRINCIPAL', 'Spare Venue')"],
});

const ROW = "Show frame rate on the stage";
const readout = (s) => s.find({ role: "toggle", label: "Frame stats" });
const live = (s) => s.findAll({ role: "text" }).some((n) => n.label.startsWith("CAMERA "));

// Settings, by its search: the row is found by what it says.
function toggleRow() {
  app.action("luma::OpenSettings");
  const field = until("the settings search", (s) => s.find({ role: "input", label: "Search settings" }))
    .find({ role: "input", label: "Search settings" });
  app.type(field, "frame rate");
  until("the search narrowed to the row", (s) =>
    s.find({ role: "checkbox", label: ROW }) && !s.find({ role: "checkbox", label: "HDR output" }));
  nav.step("the row", "checkbox", ROW);
  until("the write landed", (s) => s.find({ role: "checkbox", label: ROW }));
  app.frames(4, { waitMs: 30 });
  nav.step("Back", "button", "Back");
}

const stored = () => library.query("SELECT value FROM settings WHERE key = 'stage_frame_rate'")[0]?.value;

test("the frame rate on the stage turns off, stays off on the next stage, and comes back", () => {
  nav.patch("Test Venue");
  until("the readout on a live stage", (s) => live(s) && readout(s));

  toggleRow();
  until("the readout gone", (s) => live(s) && !readout(s));
  expect(stored()).toBe("false");

  // Another venue and back: a new stage reads the stored choice.
  nav.leaveVenue("Test Venue");
  nav.venuePage("Test Venue");
  until("the stage live again", live);
  app.frames(10, { waitMs: 30 });
  assert(readout(app.snapshot()) === undefined, "the readout came back on a new stage");

  toggleRow();
  until("the readout back", (s) => live(s) && readout(s));
  expect(stored()).toBe("true");
});
