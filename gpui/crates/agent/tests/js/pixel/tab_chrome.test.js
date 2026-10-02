// The `+` box over an empty workspace, under a real renderer: its rows are
// drawn, and drawn as rows a person can hit.

fixture({
  seconds: 20,
  clips: [{ pattern: "pattern-strobe", name: "Strobe", start: 2, end: 6 }],
  window: [1280, 800],
});

test("the + box's rows are visible", () => {
  nav.venue("Test Venue");
  app.action("luma::NewTab");
  until("the box over the empty workspace", (s) =>
    s.find({ role: "card", label: "Empty panel" }) !== undefined && s.find({ role: "row", label: "Venue" }) !== undefined);
  app.frames(4);
  const shot = app.screenshot();
  image.keep(shot, "tabs/empty-workspace");

  // Not a flat plate: something with contrast is on screen.
  const whole = image.stats(shot);
  expect(whole.max - whole.min).toBeGreaterThan(30);

  const row = app.snapshot().find({ role: "row", label: "Venue" }).bounds;
  // A row, not a chip: wider than tall, and tall enough to aim at.
  expect(row.width).toBeGreaterThan(row.height * 3);
  expect(row.height).toBeGreaterThan(23);
  // …and its own box is drawn, not an empty rectangle.
  const drawn = image.stats(shot, row);
  expect(drawn.max - drawn.min).toBeGreaterThan(30);
});
