// The value slider, dragged through the same pointer path a person uses.
//
// Every slider is one `luma_ui::luma_slider`, which once painted a value
// without accepting one. Art-Net's Max Brightness is the subject because its
// value goes through the settings write seam and comes back out of a fresh
// `get_settings`: a slider whose value lives in the view would pass on a
// control wired to nothing.

fixture({ track: false, seconds: 1 });

// Addressed by prefix: the slider's name carries its current reading.
const isMaxBrightness = (n) => n.role === "slider" && n.label.startsWith("Max Brightness");

// The reading is a node of its own ("<id> = <value>"), so the slider can be
// addressed by name while the value changes under the drag.
function reading(shot) {
  const node = shot.find((n) => n.role === "text" && n.label.startsWith("max_dimmer = "));
  return node === undefined ? null : Number(node.label.split(" = ")[1]);
}

function openArtnet() {
  app.action("luma::OpenSettings");
  nav.step("the Art-Net settings section", "toggle", "Art-Net / DMX");
  return until("the loaded Art-Net settings", (s) => s.find(isMaxBrightness) !== undefined);
}

test("dragging a slider moves its value and writes it through", () => {
  nav.venue("Test Venue");
  const opened = openArtnet();
  const slider = opened.find(isMaxBrightness);
  const before = reading(opened);

  // From the middle of the slab, a third of its width to the left. The
  // mapping is absolute — the value follows the pointer's position in the
  // box — so the end point says what the value must be.
  const width = slider.bounds.width;
  const dx = Math.round(width / 3);
  const expected = Math.round(100 * (0.5 - dx / width));
  app.drag(slider, { dx: -dx, dy: 0 }, { steps: 8 });

  // The value the pointer *ended* on, not merely a change: each move writes,
  // so "different now" could be a value from halfway along. One unit of
  // slack for the rounding between pixels and percent.
  const settled = until(`Max Brightness at ${expected}`, (s) => {
    const now = reading(s);
    return now !== null && Math.abs(now - expected) <= 1;
  });
  const dragged = reading(settled);
  expect(dragged).toBeLessThan(before);

  // Leave and come back: a fresh `get_settings` can only say what the
  // database says, so a slider that moved its own paint and nothing else
  // fails here.
  nav.dismiss();
  expect(reading(openArtnet())).toBe(dragged);
});
