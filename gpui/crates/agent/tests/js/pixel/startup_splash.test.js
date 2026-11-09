// The startup splash against a real renderer: while the library opens, the
// window is the app's ground with the lit mark alone in its middle — no line
// of text under it, no box around it.
//
// The catalogue is held so the splash stays up to be looked at. Tests run with
// reduced motion, so the mark rests at the lit end of its breath.

fixture({ window: [900, 650], venues_delay_ms: 4000 });

test("the library opens behind the mark alone, centred on bare ground", () => {
  const splash = until("the startup splash", (s) =>
    s.find({ role: "text", label: "Loading" }), { timeoutMs: 15000 });
  app.frames(4, { waitMs: 40 });
  const shot = app.screenshot();
  image.keep(shot, "startup/splash");

  // The mark is the only thing the screen says.
  const words = splash.findAll((n) => n.role === "text").map((n) => n.label);
  expect(words).toEqual(["Loading"]);

  const mark = splash.find({ role: "text", label: "Loading" }).bounds;
  // Centred across the window, and in the body's upper-to-middle band.
  expect(Math.abs(mark.x + mark.width / 2 - 900 / 2)).toBeLessThan(1.5);
  expect(mark.y).toBeGreaterThan(0);
  expect(mark.y + mark.height).toBeLessThan(650);

  // Lit: the mark's box holds pixels far above the ground beside it…
  const ground = image.stats(shot, { x: 20, y: mark.y, width: 100, height: mark.height });
  const lit = image.stats(shot, mark);
  assert(lit.max > ground.max + 120, `the mark is not lit: ${JSON.stringify({ lit, ground })}`);
  // …and the ground across the window below it is unbroken: no caption.
  const below = mark.y + mark.height + 12;
  const row = image.stats(shot, { x: 0, y: below, width: 900, height: 40 });
  assert(row.max - row.min < 1, `something is drawn under the mark: ${JSON.stringify(row)}`);
});
