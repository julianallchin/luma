// The Rig page's Lasers view: a projector is a fixture with no DMX address;
// the venue says once what its projectors are and has one window, moved and
// resized on its graph; a projector has its own DAC output, limit and test.

// A projector on the rig beside two movers. It has no channels: its address
// is 1 and means nothing.
fixture({
  seconds: 20,
  rig: 2,
  window: [1500, 1300],
  sql: [
    `INSERT INTO fixtures (id, uid, venue_id, universe, address, num_channels, manufacturer, model, mode_name,
                           fixture_path, label)
     VALUES ('laser-1', '$PRINCIPAL', 'venue-main', 1, 1, 0, 'Generic', 'Laser Projector', 'ILDA',
             'builtin:laser-projector', 'Laser 1')`,
    `INSERT INTO venue_nodes (id, uid, venue_id, kind, catalog_ref, label, parent_id, my_socket, their_socket, roll)
     VALUES ('laser-1', '$PRINCIPAL', 'venue-main', 'fixture', 'builtin:laser-projector', 'Laser 1',
             (SELECT id FROM venue_nodes WHERE venue_id = 'venue-main' AND kind = 'venue'), 'clamp', 'rig', 0.0)`,
    `INSERT INTO venue_node_params (id, node_id, uid, venue_id, key, value)
     VALUES ('trim-laser-1', 'laser-1', '$PRINCIPAL', 'venue-main', 'trim', 3.0)`,
  ],
});

const find = (role, label) => app.snapshot().find({ role, label });
const labels = (role) => app.snapshot().findAll({ role }).map((n) => n.label);
const starting = (role, prefix) => app.snapshot().findAll({ role }).find((n) => n.label.startsWith(prefix));
const focused = (label) => find("toggle", label)?.focused ?? null;
const corners = () => labels("button").filter((l) => l.startsWith("Laser 1 limit corner "));
// A reading that carries a number: "Projectors: 412 lumens of white".
const lumens = (whose) => {
  const hit = labels("text").find((l) => l.startsWith(`${whose}: `) && l.endsWith(" lumens of white"));
  return hit === undefined ? null : Number(hit.split(": ")[1].split(" ")[0]);
};
// The window's width, off the graph: "Laser window is 40° wide · 20° tall".
const wide = () => {
  const hit = labels("text").find((l) => l.startsWith("Laser window is "));
  return hit === undefined ? null : Number(hit.split(" ")[3].replace("°", ""));
};
const stored = (node, key) =>
  library.query(`SELECT value FROM venue_node_params WHERE node_id = ${node} AND key = '${key}'`);
const ROOT = "(SELECT id FROM venue_nodes WHERE venue_id = 'venue-main' AND kind = 'venue')";

function openRig() {
  nav.patch("Test Venue");
  nav.agentOff();
  until("the fixture table", (s) => s.find({ role: "row", label: "Mover 0" }) !== undefined);
}

// The stage is hidden to give the page its height, and kept for the tests
// that undo: a step of undo is the stage's.
function openLasers(stage = false) {
  openRig();
  if (!stage) nav.stageOff();
  app.click(find("toggle", "Lasers"));
  until("the projector's row", (s) => s.find({ role: "row", label: "Projector Laser 1" }) !== undefined);
}

function openSetup() {
  app.click(find("button", "More about Laser 1"));
  until("the projector's limit", (s) => s.find({ role: "card", label: "Laser 1 limit" }) !== undefined);
}

test("a projector is listed with the fixtures and takes no DMX address", () => {
  openRig();
  expect(labels("row")).toContain("Laser 1");
  expect(labels("text")).toContain("Laser 1 has no DMX address");
  // The movers keep theirs: the projector is not in the way of the first.
  expect(labels("text")).toContain("Mover 0 range = 1–8");
});

test("a preset sets what every projector is, and is stored with the venue", () => {
  openLasers();
  expect(focused("Projectors 3 W RGB")).toBe(true);
  app.click(find("button", "Projector details"));
  until("the diodes", () => lumens("Projectors") !== null);
  const before = lumens("Projectors");

  app.click(find("toggle", "Projectors 10 W RGB"));
  until("the 10 W preset", () => focused("Projectors 10 W RGB") === true);
  expect(focused("Projectors 3 W RGB")).toBe(false);
  until("a brighter white", () => lumens("Projectors") > before);

  // It is the venue's, not the projector's: one place for every box.
  until("the stored diode power", () => stored(ROOT, "laser_power_green").length === 1);
  expect(Number(stored(ROOT, "laser_power_green")[0].value)).toBeGreaterThan(1);
  expect(stored("'laser-1'", "laser_power_green")).toEqual([]);
});

test("a projector that differs is given its own kind, and gives it back", () => {
  openLasers();
  openSetup();
  expect(focused("Laser 1 has its own kind")).toBe(false);
  app.click(find("toggle", "Laser 1 has its own kind"));
  until("its own kind", () => focused("Laser 1 has its own kind") === true);
  until("its own presets", () => find("toggle", "Laser 1 3 W RGB") !== undefined);
  until("its stored kind", () => stored("'laser-1'", "laser_power_green").length === 1);

  // Its own preset leaves the venue's where it was.
  app.click(find("toggle", "Laser 1 10 W RGB"));
  until("its own 10 W", () => focused("Laser 1 10 W RGB") === true);
  expect(focused("Projectors 3 W RGB")).toBe(true);

  app.click(find("toggle", "Laser 1 has its own kind"));
  until("following the venue again", () => focused("Laser 1 has its own kind") === false);
  until("nothing of its own stored", () => stored("'laser-1'", "laser_power_green").length === 0);
});

test("the venue's window is fitted to its projectors until it is set", () => {
  openLasers();
  const fitted = (l) => l.startsWith("Fitted to the projectors");
  expect(labels("text").some(fitted)).toBe(true);
  expect(find("button", "Fit to projectors")).toBe(undefined);

  // A number is dragged a step every few pixels, however wide its range.
  app.drag(starting("slider", "Laser window Left"), { dx: -40, dy: 0 }, { steps: 4 });
  until("a window of its own", () => find("button", "Fit to projectors") !== undefined);
  expect(labels("text").some(fitted)).toBe(false);

  app.click(find("button", "Fit to projectors"));
  until("the fitted window again", () => labels("text").some(fitted));
});

test("the window is resized by its handles on the graph, and undo takes it back", () => {
  openLasers(true);
  const before = wide();
  app.drag(find("button", "Laser window right handle"), { dx: 60, dy: 0 }, { steps: 6 });
  until("a wider window", () => wide() > before);
  until("a window of its own", () => find("button", "Fit to projectors") !== undefined);
  const after = wide();

  // Its body moves it without resizing it.
  app.drag(find("card", `Laser window: ${labels("card").find((l) => l.startsWith("Laser window: ")).split(": ")[1]}`), { dx: 0, dy: -30 }, { steps: 6 });
  until("the window written", () => stored(ROOT, "laser_tilt_min").length === 1);
  expect(wide()).toBe(after);

  // The move, then the resize: each is one step of the stage's undo.
  app.action("luma::Undo");
  until("the resize alone", () => stored(ROOT, "laser_tilt_min").length === 1 && wide() === after);
  app.action("luma::Undo");
  until("the fitted window again", () => find("button", "Fit to projectors") === undefined);
  expect(wide()).toBe(before);
});

test("a test pattern is shown without a DAC, one projector at a time", () => {
  openLasers();
  expect(find("button", "Test Laser 1").focused).toBe(false);
  app.click(find("button", "Test Laser 1"));
  until("the test showing", () => find("button", "Stop test Laser 1")?.focused === true);
  app.click(find("button", "Stop test Laser 1"));
  until("the show again", () => find("button", "Test Laser 1") !== undefined);
});

test("a limit is added, grows a corner and is removed", () => {
  openLasers();
  openSetup();
  expect(corners()).toEqual([]);

  app.click(find("button", "Add a limit: Laser 1"));
  until("a limit of four corners", () => corners().length === 4);

  app.click(find("button", "Add a corner: Laser 1"));
  until("a fifth corner", () => corners().length === 5);

  // Lifting a limit is asked first, and "Cancel" keeps it.
  app.click(find("button", "Remove the limit: Laser 1"));
  until("the question", (s) => s.find({ role: "text", label: "Remove the limit?" }) !== undefined);
  app.click(find("button", "Cancel"));
  until("the question gone", (s) => s.find({ role: "text", label: "Remove the limit?" }) === undefined);
  expect(corners().length).toBe(5);

  app.click(find("button", "Remove the limit: Laser 1"));
  until("the question", (s) => s.find({ role: "text", label: "Remove the limit?" }) !== undefined);
  app.click(find("button", "Remove the limit"));
  until("no limit", () => corners().length === 0);
  expect(labels("text")).toContain("Laser 1: No limit: it may send light anywhere in its scan.");
});

test("a machine with no physical output says so in place of the switch", () => {
  openLasers();
  expect(labels("text")).toContain("Laser output is not running on this machine.");
  expect(find("toggle", "Laser output")).toBe(undefined);
});
