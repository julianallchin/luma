// The floor on the venue page.
//
// The floor is venue truth, like the clouds: it sits on the venue's
// environment record, each kind of venue offers its own floors, and the
// choice is still there when the venue page is opened again.

fixture({ seconds: 20, rig: 4, window: [1500, 1100] });

// Every floor either kind of venue might offer.
const FLOORS = ["Grass", "Dirt", "Fine sand", "Beach", "Gravelly sand", "Asphalt",
  "Concrete", "Gravel", "Stage deck", "Hall floor", "Black stage floor", "Carpet"];

test("floors follow the kind of venue and survive reopening it", () => {
  const floor = (s = app.snapshot()) => {
    const hit = s.findAll({ role: "text" }).map((n) => n.label).find((l) => l.startsWith("Floor = "));
    return hit === undefined ? null : hit.slice("Floor = ".length);
  };
  const offered = () => FLOORS.filter((name) => app.snapshot().find({ role: "toggle", label: name }) !== undefined);
  // The panel is shorter than its rows: scroll to the Floor rows and back.
  const scrollPanel = (dy) => {
    app.scroll(app.snapshot().find({ role: "card", label: "Venue environment" }), { dy, steps: 8 });
    app.frames(4);
  };
  const reopen = () => {
    // Let the write land, then leave the venue and come back.
    app.frames(20);
    nav.closeTab();
    nav.venuePage("Test Venue");
    until("the reopened environment", (s) => s.find({ role: "card", label: "Venue environment" }));
    return floor(until("the saved floor", (s) => floor(s) !== null));
  };
  const choose = (name) => {
    nav.step(name, "toggle", name);
    until(`${name} chosen`, (s) => floor(s) === name);
  };

  nav.patch("Test Venue");
  until("the environment", (s) => s.find({ role: "card", label: "Venue environment" }));

  nav.step("outdoor", "toggle", "Outdoor");
  until("an outdoor floor", (s) => floor(s) !== null && offered().includes(floor(s)));
  const outdoor = offered();
  scrollPanel(-400);
  choose("Gravel");

  // Moving the sun keeps the floor.
  scrollPanel(400);
  const elevation = app.snapshot().find({ role: "slider", label: "Sun elevation (°)" });
  app.drag(
    { x: elevation.bounds.x + elevation.bounds.width / 2, y: elevation.bounds.y + elevation.bounds.height / 2 },
    { dx: -30, dy: 0 },
    { steps: 6 },
  );
  app.frames(4);
  expect(floor()).toBe("Gravel");
  expect(reopen()).toBe("Gravel");

  scrollPanel(400);
  nav.step("indoor", "toggle", "Indoor");
  // Switching kind lands on a floor the new kind offers.
  until("an indoor floor", (s) => floor(s) !== null && floor(s) !== "Gravel" && offered().includes(floor(s)));
  const indoor = offered();
  scrollPanel(-400);
  choose("Carpet");
  expect(reopen()).toBe("Carpet");

  // Each kind offers its own floors: ground outside, stage floors inside.
  for (const name of ["Grass", "Gravel"]) {
    expect(outdoor).toContain(name);
    assert(!indoor.includes(name), `indoor offered ${name}`);
  }
  for (const name of ["Carpet", "Stage deck"]) {
    expect(indoor).toContain(name);
    assert(!outdoor.includes(name), `outdoor offered ${name}`);
  }
});
