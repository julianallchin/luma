// The stage asks for a frame on every render. The panes around it are cached
// regions (`shell::Region`): a stage frame must not rebuild them, and a change
// they show must still reach them.

fixture({ seconds: 20, rig: 4, window: [1500, 950] });

// Each region publishes how many times it has rendered.
function renders(snapshot, region) {
  const prefix = `${region} renders = `;
  const node = snapshot.find((n) => n.role === "text" && n.label.startsWith(prefix));
  return node === undefined ? null : Number(node.label.slice(prefix.length));
}
const counts = (s) => ({
  stage: renders(s, "Stage"),
  tab: renders(s, "Tab"),
  sidebar: renders(s, "Sidebar"),
  environment: renders(s, "Environment"),
});
const panes = ["tab", "sidebar", "environment"];

function openVenue() {
  nav.patch("Test Venue");
  until("the loaded room", (s) => s.find({ role: "toggle", label: "Frame stats" }) !== undefined);
  until("the fixture rows", (s) => s.find({ role: "row", label: "Mover 0" }) !== undefined);
}

// Wait until the stage has drawn several frames in a row while no pane
// rebuilt. A hover fade or a load landing rebuilds the panes for a moment,
// which is correct; a pane that rebuilds on *every* stage frame never gets
// here.
function settled() {
  let from = null;
  let last = null;
  until("stage frames that leave the panes alone", (s) => {
    const now = counts(s);
    if (from === null || panes.some((pane) => now[pane] !== from[pane])) {
      from = now;
      return false;
    }
    last = now;
    return now.stage >= from.stage + 5;
  });
  return { from, last };
}

test("a stage frame rebuilds the stage and not the panes around it", () => {
  openVenue();
  const { from, last } = settled();
  for (const pane of panes) {
    assert(from[pane] !== null, `the ${pane} region is not on screen`);
  }
  assert(last.stage > from.stage, "the stage stopped drawing");

  // Picking a row is an app change, and it reaches the cached table.
  app.click(app.snapshot().find({ role: "row", label: "Mover 1" }));
  until("the row to be picked", (s) =>
    s.findAll({ role: "row" }).some((n) => n.label === "Mover 1" && n.focused));
  expect(renders(app.snapshot(), "Tab")).toBeGreaterThan(last.tab);
});

test("the readout shows both rates and opens the frame stats", () => {
  openVenue();
  const texts = (s) => s.findAll({ role: "text" }).map((n) => n.label);
  until("the delivered rate", (s) => texts(s).some((l) => l.startsWith("FPS ")));
  nav.step("the readout", "toggle", "Frame stats");
  const open = until("the stats panel", (s) => texts(s).some((l) => l.startsWith("Rendered ")));
  for (const line of ["Low ", "Draw ", "UI ", "Present ", "Shadows "]) {
    assert(texts(open).some((l) => l.startsWith(line)), `no "${line}" line in the panel`);
  }
  // A press on the readout closes the panel again.
  app.click(open.find({ role: "toggle", label: "Frame stats" }));
  until("the stats panel to close", (s) => !texts(s).some((l) => l.startsWith("Rendered ")));
});
