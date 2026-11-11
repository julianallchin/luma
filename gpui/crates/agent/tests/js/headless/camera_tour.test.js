// The camera walkthrough: an account that has never opened a score finds a
// card over the stage that says how to move the eye. Skipping it is the end
// of it for that account. The moves themselves need a renderer's pointer
// plane: see the pixel test of the same name.

fixture({ camera_tour: true });

const card = (s) => s.find({ role: "card", label: "Camera walkthrough" });

test("a first score opens under the walkthrough, and skipping it is for good", () => {
  nav.trackEditor("Test Venue", "Aurora");
  const up = until("the walkthrough", (s) => card(s) ? s : undefined);
  expect(texts(up)).toContain("Drag to look around");

  app.click(up.find({ role: "button", label: "Skip camera walkthrough" }));
  until("the card gone", (s) => card(s) === undefined);

  // Leaving the score and opening it again does not bring it back.
  nav.songs();
  nav.track("Aurora");
  until("the score again", (s) => nav.openScore(s, "Aurora") !== undefined);
  app.frames(4);
  expect(card(app.snapshot())).toBe(undefined);
});

test("the room is built with no walkthrough over it", () => {
  nav.patch("Test Venue");
  until("the stage", (s) => s.find({ role: "card", label: "Stage" }));
  app.frames(4);
  expect(card(app.snapshot())).toBe(undefined);
});
