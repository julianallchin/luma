// The room on the venue page.
//
// An indoor venue can be given walls and a ceiling. Its size is the venue's
// own: it is still there when the venue is opened again, and undo takes the
// walls away as it takes away anything else built in the room.

fixture({ seconds: 20, rig: 4, window: [1500, 1100], sql: ["INSERT INTO venues (id, uid, name) VALUES ('venue-spare', '$PRINCIPAL', 'Spare Venue')"] });

test("an indoor room gets walls, keeps its size and loses them to undo", () => {
  const button = (label) => app.snapshot().find({ role: "button", label });
  const side = (name) => {
    const hit = app.snapshot().findAll({ role: "slider" }).find((n) => n.label === name);
    return hit === undefined ? undefined : hit;
  };
  const scrollPanel = (dy) => {
    app.scroll(app.snapshot().find({ role: "card", label: "Venue environment" }), { dy, steps: 8 });
    app.frames(4);
  };

  nav.stage("Test Venue");
  nav.agentOff();
  until("the environment", (s) => s.find({ role: "card", label: "Venue environment" }));
  until("the room's offer", () => button("Add walls and ceiling") !== undefined);
  expect(side("Width (m)")).toBe(undefined);

  nav.step("walls", "button", "Add walls and ceiling");
  until("the room's size", () => side("Width (m)") !== undefined && side("Height (m)") !== undefined);
  until("the way back", () => button("Remove walls") !== undefined);

  // Leave and come back: the walls are the venue's, not the page's.
  app.frames(20);
  nav.leaveVenue("Test Venue");
  nav.stage("Test Venue");
  until("the kept room", () => button("Remove walls") !== undefined && side("Depth (m)") !== undefined);

  nav.step("no walls", "button", "Remove walls");
  until("open floor", () => button("Add walls and ceiling") !== undefined);
  app.action("luma::Undo");
  until("the walls back", () => button("Remove walls") !== undefined);
});
