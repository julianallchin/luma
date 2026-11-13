// The bounced light switch on the render settings' Debug page.
//
// It is a session view choice: a click must change what the renderer is
// handed, which the page reports as `Renderer bounce = on|off`.

fixture({ seconds: 20, rig: 4, window: [1500, 1100] });

test("the bounced light switch changes what the renderer is handed", () => {
  const toggled = () => app.snapshot().find({ role: "toggle", label: "Bounced light" });
  const sent = () => {
    const hit = app
      .snapshot()
      .findAll({ role: "text" })
      .map((n) => n.label)
      .find((l) => l.startsWith("Renderer bounce = "));
    return hit === undefined ? undefined : hit.slice("Renderer bounce = ".length);
  };

  nav.patch("Test Venue");
  nav.step("view settings", "toggle", "Render settings");
  nav.step("its debug page", "toggle", "Debug");
  until("the switch", () => toggled() !== undefined);
  expect(toggled().focused).toBe(true);
  expect(sent()).toBe("on");

  nav.step("bounce off", "toggle", "Bounced light");
  until("the renderer hears", () => sent() === "off");
  expect(toggled().focused).toBe(false);
});
