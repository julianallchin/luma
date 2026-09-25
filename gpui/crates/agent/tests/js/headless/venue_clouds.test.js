// Sky clouds on the venue page.
//
// The cloud preset is venue truth, like the sun: it sits on the venue's
// environment record, so it is only offered where there is a sky, and it is
// still there when the venue page is opened again.

fixture({ seconds: 20, rig: 4, window: [1500, 950] });

const PRESETS = ["Clear", "Fair weather", "Scattered", "Overcast", "Storm"];

test("cloud presets are outdoor only and survive reopening the venue", () => {
  const clouds = (s = app.snapshot()) => {
    const hit = s.findAll({ role: "text" }).map((n) => n.label).find((l) => l.startsWith("Clouds = "));
    return hit === undefined ? null : hit.slice("Clouds = ".length);
  };
  const offered = () => PRESETS.filter((name) => app.snapshot().find({ role: "toggle", label: name }) !== undefined);

  nav.patch("Test Venue");
  until("the environment", (s) => s.find({ role: "card", label: "Venue environment" }));
  nav.step("indoor", "toggle", "Indoor");
  app.frames(4);
  expect(clouds()).toBe(null);
  expect(offered()).toEqual([]);

  nav.step("outdoor", "toggle", "Outdoor");
  until("the cloud row", (s) => clouds(s) !== null);
  expect(clouds()).toBe("Clear");
  expect(offered()).toEqual(PRESETS);
  nav.step("overcast", "toggle", "Overcast");
  until("overcast chosen", (s) => clouds(s) === "Overcast");

  // Moving the sun keeps the weather.
  const elevation = app.snapshot().find({ role: "slider", label: "Sun elevation (°)" });
  app.drag(
    { x: elevation.bounds.x + elevation.bounds.width / 2, y: elevation.bounds.y + elevation.bounds.height / 2 },
    { dx: -30, dy: 0 },
    { steps: 6 },
  );
  app.frames(4);
  expect(clouds()).toBe("Overcast");

  // Let the write land, then leave the venue and come back.
  app.frames(20);
  nav.closeTab();
  nav.venuePage("Test Venue");
  until("the reopened environment", (s) => s.find({ role: "card", label: "Venue environment" }));
  expect(clouds(until("the saved clouds", (s) => clouds(s) !== null))).toBe("Overcast");
});
