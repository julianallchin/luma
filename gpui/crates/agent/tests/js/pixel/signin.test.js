// The sign-in screen against a real renderer. What the pixels are read for
// is that it is a *state* and not a dialog: one centred column on the app's
// own ground, with no card, band or box drawn around it.
//
// Reached by signing out from the account foot: the fixture launches signed
// in, and the gate is where a sign-out lands. Only the Email route is driven:
// the Code route would send a real login code.

fixture({ window: [900, 650] });

test("the sign-in screen is one centred column on bare ground", () => {
  nav.venue("Test Venue");
  nav.step("the account foot", "button", "Account");
  nav.step("the sign-out row", "row", "Sign out");
  const gate = until("the sign-in screen", (s) =>
    s.find({ role: "text", label: "Sign in to Luma" }) !== undefined, { timeoutMs: 15000 });
  app.frames(4, { waitMs: 40 });
  const shot = app.screenshot();
  image.keep(shot, "signin/gate");

  const title = gate.find({ role: "text", label: "Sign in to Luma" }).bounds;
  const field = gate.find({ role: "input", label: "Email" }).bounds;
  const primary = gate.find({ role: "button", label: "Continue" }).bounds;
  const close = gate.find({ role: "button", label: "close" }).bounds;

  // One column, centred in the window.
  for (const box of [title, field, primary]) {
    expect(Math.abs(box.x + box.width / 2 - 900 / 2)).toBeLessThan(1.5);
  }
  // The field and the capsule under it are the column's one width.
  expect(Math.abs(field.width - primary.width)).toBeLessThan(1);
  // The column clears the window's own controls.
  expect(title.y).toBeGreaterThan(close.y + close.height);

  // No card: the ground beside the column is unbroken all the way across,
  // above the title and in the gap between the field and the capsule.
  const gapBelow = field.y + field.height;
  for (const y of [title.y - 10, (gapBelow + primary.y) / 2]) {
    const row = image.stats(shot, { x: 0, y, width: 900, height: 1 });
    assert(row.max - row.min < 1, `something is drawing a box across y = ${y}: ${JSON.stringify(row)}`);
  }
});
