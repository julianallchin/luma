// The stage's pointer plane belongs to whatever is drawn on top of it.
//
// Two gestures that used to reach the camera through something else: dragging
// the stage/editor seam orbited (the 5px grip overhangs the viewport and gpui
// reports every hitbox under the pointer), and turning the wheel over the
// floating View settings card dollied. Pixel-only because the stage's pointer
// handlers exist only where there is a renderer. The readings are the CAMERA
// node, not screenshots: a frame diff would carry everything else the scene
// does on its own.

fixture({ seconds: 20, rig: 4 });

function camera() {
  const node = app.snapshot().findAll({ role: "text" }).find((n) => n.label.startsWith("CAMERA "));
  return node === undefined ? null : node.label;
}
const stage = () => app.snapshot().find({ role: "card", label: "Stage" }).bounds;

function open() {
  nav.patch("Test Venue");
  app.frames(6, { waitMs: 60 });
  until("the stage's camera", () => camera() !== null);
}

test("a seam drag resizes the stage and does not orbit it", () => {
  open();
  const before = { camera: camera(), stage: stage() };
  // The grip's leading pixel, not its centre: the strip that overhangs the
  // stage is where a shared press would land.
  const grip = app.snapshot().find({ role: "slider", label: "Stage height" }).bounds;
  app.drag({ x: grip.x + grip.width / 2, y: grip.y + 1 }, { dx: 0, dy: -90 }, { steps: 10, restale: "match" });
  app.frames(4, { waitMs: 60 });
  const after = stage();
  // The stage lost about what the pointer travelled.
  expect(Math.abs(before.stage.height - after.height - 90)).toBeLessThan(8);
  expect(camera()).toBe(before.camera);
});

// The second scroll, over bare stage, is the control: without it a wheel the
// harness never delivered would pass the first assertion as loudly as an
// occluder that works.
test("a wheel over the view settings does not dolly the camera", () => {
  open();
  nav.step("view settings", "toggle", "Render settings");
  const card = until("the view settings card", (s) => s.find({ role: "card", label: "Render settings" }))
    .find({ role: "card", label: "Render settings" }).bounds;
  // The card's legend row, not a control: a scrub under the pointer would take
  // the wheel as a value change and prove nothing about the surface.
  const legend = { x: card.x + card.width / 2, y: card.y + 12 };
  const before = camera();
  app.scroll(legend, { dy: -160, steps: 8, restale: "match" });
  app.frames(4, { waitMs: 60 });
  expect(camera()).toBe(before);

  const bounds = stage();
  const bare = { x: (bounds.x + card.x) / 2, y: bounds.y + bounds.height / 2 };
  assert(bare.x < card.x, "the card leaves no bare stage to aim at");
  app.scroll(bare, { dy: -160, steps: 8, restale: "match" });
  app.frames(4, { waitMs: 60 });
  assert(camera() !== before, "a wheel over the bare stage did not dolly — the wheel never reached the viewport");
});
