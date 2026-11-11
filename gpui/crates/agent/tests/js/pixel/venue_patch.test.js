// What the patch page looks like, for a person to inspect: the inventory
// table, and the patch view with a collision on one universe's strip and a
// clean strip beside it. The captures are the point; the
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
                           fixture_path, label)
     VALUES ('fixture-clash', '$PRINCIPAL', 'venue-main', 1, 5, 8, 'Luma', 'Mover', 'Default', 'Luma/Mover.qxf', 'Clash 1'),
            ('fixture-far', '$PRINCIPAL', 'venue-main', 17, 1, 8, 'Luma', 'Mover', 'Default', 'Luma/Mover.qxf', 'Far 1')`,
    `INSERT INTO venue_nodes (id, uid, venue_id, kind, catalog_ref, label, parent_id, my_socket, their_socket, roll)
     VALUES ('fixture-clash', '$PRINCIPAL', 'venue-main', 'fixture', 'Luma/Mover.qxf', 'Clash 1', 'venue-main:venue', 'clamp', 'rig', 0.0),
            ('fixture-far', '$PRINCIPAL', 'venue-main', 'fixture', 'Luma/Mover.qxf', 'Far 1', 'venue-main:venue', 'clamp', 'rig', 0.0)`,
    `INSERT INTO venue_node_params (node_id, uid, venue_id, key, value)
     VALUES ('fixture-clash', '$PRINCIPAL', 'venue-main', 'trim', 3.0), ('fixture-far', '$PRINCIPAL', 'venue-main', 'trim', 3.0)`,
  ],
});

const red = (shot) => image.tint(shot, { channel: "red", margin: 40 });
const card = (label) => app.snapshot().find({ role: "card", label });

test("a collision is red and a clean universe is not", { timeoutMs: 120000 }, () => {
  nav.patch("Test Venue");
  nav.agentOff();
  nav.stageOff();
  until("the patch table", (s) => s.find({ role: "row", label: "Mover 0" }) !== undefined);
  app.frames(6);
  image.keep(app.screenshot(), "venue-patch/table");

  nav.step("the patch view", "toggle", "Patch");
  until("the universes", (s) => s.findAll({ role: "text" }).some((n) => n.label.startsWith("Collision at")));
  app.frames(6);
  image.keep(app.screenshot(), "venue-patch/patch");
  // A strip is one node, not 512: it is the smallest thing that can be
  // pointed at.
  const colliding = app.screenshot({ node: card("Universe 1 channels") });
  image.keep(colliding, "venue-patch/strip-collision");
  const clean = app.screenshot({ node: card("Universe 17 channels") });
  image.keep(clean, "venue-patch/strip-clean");

  // Red is on the strip with a collision, and nowhere near as much of it on
  // the universe that has none — which is what makes the first number mean
  // "collision" rather than "this is what a strip looks like".
  expect(red(colliding)).toBeGreaterThan(0.0005);
  expect(red(clean)).toBeLessThan(red(colliding) / 4);
});
