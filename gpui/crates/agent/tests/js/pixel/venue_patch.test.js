// What the patch page looks like, for a person to inspect: the inventory
// table, the footprint strip with a collision in it, the same strip on a
// clean universe, and the outputs. The captures are the point; the
// assertions are the claims a picture makes — a collision is *red* rather
// than merely reported, and a clean universe is not.

// A hand-set collision no gesture could make in universe 1, and one fixture
// alone in universe 17, so there is a clean strip to compare.
fixture({
  seconds: 20,
  rig: 4,
  window: [1500, 950],
  sql: [
    `INSERT INTO fixtures (id, uid, venue_id, universe, address, num_channels, manufacturer, model, mode_name,
                           fixture_path, label, pos_x, pos_y, pos_z, rot_x, rot_y, rot_z)
     VALUES ('fixture-clash', '$PRINCIPAL', 'venue-main', 1, 5, 8, 'Luma', 'Mover', 'Default', 'Luma/Mover.qxf', 'Clash 1', 0.0, 0.0, 3.0, 0.0, 0.0, 0.0),
            ('fixture-far', '$PRINCIPAL', 'venue-main', 17, 1, 8, 'Luma', 'Mover', 'Default', 'Luma/Mover.qxf', 'Far 1', 0.0, 0.0, 3.0, 0.0, 0.0, 0.0)`,
  ],
});

const red = (shot) => image.tint(shot, { channel: "red", margin: 40 });
const card = (label) => app.snapshot().find({ role: "card", label });

test("a collision is red and a clean universe is not", { timeoutMs: 120000 }, () => {
  nav.patch("Test Venue");
  nav.step("patch details", "button", "Patch details");
  nav.expand();
  nav.stageOff();
  until("the patch table", (s) => s.find({ role: "row", label: "Mover 0" }) !== undefined);
  app.frames(6);
  image.keep(app.screenshot(), "venue-patch/table");

  nav.step("the footprint toggle", "toggle", "Footprint");
  until("the footprint", (s) => s.findAll({ role: "text" }).some((n) => n.label.startsWith("Collision at")));
  app.frames(6);
  image.keep(app.screenshot(), "venue-patch/footprint-panel");
  // The float's own card: 512 cells are not 512 nodes, so the panel is the
  // smallest thing that can be pointed at.
  const colliding = app.screenshot({ node: card("Footprint") });
  image.keep(colliding, "venue-patch/footprint-collision");

  nav.step("the universe picker", "select", "Universe 1");
  nav.step("universe 17", "button", "Universe 17");
  until("the clean universe", (s) => s.find({ role: "select", label: "Universe 17" }) !== undefined);
  app.frames(6);
  const clean = app.screenshot({ node: card("Footprint") });
  image.keep(clean, "venue-patch/footprint-clean");

  // Red is on the strip with a collision, and nowhere near as much of it on
  // the universe that has none — which is what makes the first number mean
  // "collision" rather than "this is what a strip looks like".
  expect(red(colliding)).toBeGreaterThan(0.0005);
  expect(red(clean)).toBeLessThan(red(colliding) / 4);

  // A press outside an open float is eaten by its dismissal, so panel to
  // panel is close then open.
  nav.step("the footprint toggle", "toggle", "Footprint");
  app.frames(4);
  nav.step("the outputs toggle", "toggle", "Outputs");
  until("the outputs", (s) => s.findAll({ role: "row" }).some((n) => n.label.startsWith("Universe 1 →")));
  app.frames(6);
  image.keep(app.screenshot(), "venue-patch/outputs");
});
