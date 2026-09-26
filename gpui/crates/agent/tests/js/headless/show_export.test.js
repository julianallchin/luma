// "Export show" raises a dialog over the score: a size, a file, and Start.
//
// Nothing is rendered here. The claims are the dialog's own: it opens from
// the score's toolbar, it offers the sizes with 4K chosen, it names an mp4 to
// write, a size can be chosen, and Cancel closes it without starting.

fixture({ seconds: 4, clips: [{ pattern: "pat-glow", name: "Glow", start: 1, end: 3 }], rig: 4 });

const DIALOG = { role: "card", label: "Export show dialog" };

test("export show opens a dialog with the sizes and the file, and cancel closes it", () => {
  nav.venue("Test Venue");
  nav.track("Aurora");
  // The button waits for the track's length, which the waveform brings.
  const ready = until("the export button to enable", (s) =>
    s.find({ role: "button", label: "Export show" })?.enabled === true);
  app.click(ready.find({ role: "button", label: "Export show" }));

  const opened = until("the export dialog", (s) => s.find(DIALOG) !== undefined);
  const chosen = (s) => ["1080p", "1440p", "4K"].filter((label) => s.find({ role: "toggle", label })?.focused);
  expect(chosen(opened)).toEqual(["4K"]);
  expect(opened.findAll({ role: "text" }).some((n) => n.label.endsWith(".mp4"))).toBe(true);
  assert(opened.find({ role: "button", label: "Start" }) !== undefined, "the dialog has no Start");

  app.click(opened.find({ role: "toggle", label: "1080p" }));
  until("1080p to be chosen", (s) => chosen(s).join() === "1080p");

  const shot = app.snapshot();
  const card = shot.find(DIALOG).bounds;
  const inside = (n) => n.bounds.x >= card.x && n.bounds.x + n.bounds.width <= card.x + card.width;
  app.click(shot.find((n) => n.role === "button" && n.label === "Cancel" && inside(n)));
  until("the dialog to close", (s) => s.find(DIALOG) === undefined);
});
