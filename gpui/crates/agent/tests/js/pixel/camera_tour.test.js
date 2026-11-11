// The camera walkthrough moves on when the reader makes the move it asks
// for, and not for any other. Pixel-only because the stage's pointer
// handlers exist only where there is a renderer.

fixture({ camera_tour: true, seconds: 20, rig: 4 });

const card = (s) => s.find({ role: "card", label: "Camera walkthrough" });
const asking = (line) => until(line, (s) => card(s) && texts(s).includes(line) ? s : undefined);
// Bare stage, clear of the card at its bottom edge.
const bare = () => {
  const b = app.snapshot().find({ role: "card", label: "Stage" }).bounds;
  return { x: b.x + b.width / 2, y: b.y + b.height / 3 };
};

test("each move made is the next one asked for", () => {
  nav.trackEditor("Test Venue", "Aurora");
  asking("Drag to look around");
  app.frames(6, { waitMs: 60 });
  image.keep(app.screenshot(), "camera-tour/orbit");

  // A wheel is not what is being asked for yet.
  app.scroll(bare(), { dy: -120, steps: 4, restale: "match" });
  app.frames(4, { waitMs: 60 });
  expect(texts(app.snapshot())).toContain("Drag to look around");

  app.drag(bare(), { dx: 120, dy: 20 }, { steps: 10, restale: "match" });
  asking("Scroll to zoom");
  app.scroll(bare(), { dy: -120, steps: 4, restale: "match" });
  asking("Drag with the middle button to slide");
  app.drag(bare(), { dx: 60, dy: 0 }, { steps: 8, button: "middle", restale: "match" });
  const last = asking("Hold the right button and press W A S D to fly");
  image.keep(app.screenshot({ node: card(last) }), "camera-tour/fly");

  // The right button's fly takes keys the harness cannot hold down with it,
  // so the walk is skipped from here; the unit test walks every step.
  app.click(last.find({ role: "button", label: "Skip camera walkthrough" }));
  until("the card gone", (s) => card(s) === undefined);
});
