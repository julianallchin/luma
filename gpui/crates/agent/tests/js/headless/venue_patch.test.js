// The patch page: addresses, footprint, outputs, auto patch, unpatching,
// adding fixtures and modes, and the venue page's groups, render settings,
// haze and view settings.

// Twenty seconds of track and the four-mover rig on a wide window.
fixture({ seconds: 20, rig: 4, window: [1500, 950] });

// What a gesture cannot make: a hand-set collision in universe 1 (`rig`
// patches movers at 1.1, 1.9, 1.17 and 1.25, eight channels each; this one
// lands on the first two), a fixture out in universe 17, and a binding for
// each universe whose only difference is the node it names.
const EXTRAS = [
  `INSERT INTO fixtures (id, uid, venue_id, universe, address, num_channels, manufacturer, model, mode_name,
                         fixture_path, label, pos_x, pos_y, pos_z, rot_x, rot_y, rot_z)
   VALUES ('fixture-clash', '$PRINCIPAL', 'venue-main', 1, 5, 8, 'Luma', 'Mover', 'Default', 'Luma/Mover.qxf', 'Clash 1', 0.0, 0.0, 3.0, 0.0, 0.0, 0.0),
          ('fixture-far', '$PRINCIPAL', 'venue-main', 17, 1, 8, 'Luma', 'Mover', 'Default', 'Luma/Mover.qxf', 'Far 1', 0.0, 0.0, 3.0, 0.0, 0.0, 0.0)`,
  `INSERT INTO universe_outputs (universe, node_ip, node_port, port_address, node_name)
   VALUES (1, '10.0.0.5', 6454, 0, 'Node A'), (17, '10.0.0.6', 6454, 16, 'Node B')`,
];

const texts = () => app.snapshot().findAll({ role: "text" }).map((n) => n.label);
function reading(prefix) {
  const hit = texts().find((l) => l.startsWith(prefix));
  return hit === undefined ? null : hit.slice(prefix.length);
}
// The mode cell is a picker, so it lands under `select`, not `text`.
function chosen(prefix) {
  const hit = app.snapshot().findAll({ role: "select" }).map((n) => n.label).find((l) => l.startsWith(prefix));
  return hit === undefined ? null : hit.slice(prefix.length);
}
const rowLabels = () => app.snapshot().findAll({ role: "row" }).map((n) => n.label);
const movers = () => rowLabels().filter((l) => l.startsWith("Mover "));
// The last of several same-labelled buttons: a dialog's confirm is painted
// over the control that opened it.
const lastButton = (label) => app.snapshot().findAll({ role: "button", label }).at(-1);

function openPatch() {
  nav.patch("Test Venue");
  nav.step("patch details", "button", "Patch details");
  // Takeover, and the stage off: the table is nine columns beside a rail.
  nav.expand();
  nav.stageOff();
  until("the patch table", (s) => s.find({ role: "row", label: "Mover 0" }) !== undefined);
  app.frames(4);
}

// Click an address cell and type a new address.
function typeAddress(row, from, digits) {
  app.click(app.snapshot().find({ role: "text", label: `${row} address = ${from}` }));
  until("the address field", (s) => s.findAll({ role: "input" }).some((n) => n.label.startsWith(`${row} address = `)));
  app.key(`secondary-a ${digits.split("").join(" ")} enter`);
}

// A typed address the allocator refuses changes nothing and says why.
test("a refused address leaves the row where it was", () => {
  openPatch();
  const before = reading("Mover 1 address = ");
  // Where Mover 0 already is.
  typeAddress("Mover 1", before, "1");
  until("the refusal", (s) => s.findAll({ role: "text" }).some((n) => n.label.includes("collides with")));
  app.frames(4);
  const refusal = texts().find((l) => l.includes("collides with"));
  expect(reading("Mover 1 address = ")).toBe(before);
  // Named, not identified: the row in the way is `Mover 0` on screen.
  expect(refusal).toContain("collides with Mover 0");
  assert(!refusal.includes("fixture-0"), "the refusal put a row id in front of a person");
  // No frame between the keystroke and now showed the row anywhere else.
  const everMoved = app.painted().some((s) => s.nodes.some((n) =>
    n.role === "text" && n.label.startsWith("Mover 1 address = ") && n.label !== `Mover 1 address = ${before}`));
  assert(!everMoved, "the row moved on some frame before settling back");
  // The page puts the cursor on the conflicting row.
  until("the conflicting row to be picked", (s) => s.findAll({ role: "row" }).some((n) => n.label === "Mover 0" && n.focused));
  app.frames(2);
  expect(app.snapshot().findAll({ role: "row" }).filter((n) => n.focused).map((n) => n.label)).toEqual(["Mover 0"]);
});

// The footprint strip shows the collision the allocator computed, and the
// universe chips tell 1 from 17.
test("the footprint reports a collision and the universes it holds", { fixture: { sql: EXTRAS } }, () => {
  openPatch();
  const collisions = () => texts().filter((l) => l.startsWith("Collision at"));
  app.click(app.snapshot().find({ role: "toggle", label: "Footprint" }));
  until("the footprint", (s) => s.find({ role: "select", label: "Universe 1" }) !== undefined);
  app.frames(4);
  // The clash covers 5–12 across movers at 1–8 and 9–16: one run, not eight
  // cells.
  expect(collisions()).toEqual(["Collision at 1:5–12"]);

  app.click(app.snapshot().find({ role: "select", label: "Universe 1" }));
  until("the universe menu", (s) => s.find({ role: "button", label: "Universe 17" }) !== undefined);
  expect(app.snapshot().findAll({ role: "button" }).map((n) => n.label).filter((l) => l.startsWith("Universe ")))
    .toEqual(["Universe 1", "Universe 17"]);
  // Universe 17 is a different strip, and it is not colliding.
  app.click(app.snapshot().find({ role: "button", label: "Universe 17" }));
  until("the seventeenth universe", (s) => s.find({ role: "select", label: "Universe 17" }) !== undefined);
  app.frames(4);
  expect(collisions()).toEqual([]);
});

// Universes 1 and 17 resolve to different rows — asserted on what the page
// resolved, not on the formula that used to alias them.
test("universe one and seventeen are different outputs", { fixture: { sql: EXTRAS } }, () => {
  openPatch();
  app.click(app.snapshot().find({ role: "toggle", label: "Outputs" }));
  until("the outputs table", (s) => s.findAll({ role: "row" }).some((n) => n.label.startsWith("Universe 1 →")));
  app.frames(4);
  const rows = rowLabels().filter((l) => l.startsWith("Universe "));
  const one = rows.find((l) => l.startsWith("Universe 1 →")).slice("Universe 1 →".length);
  const seventeen = rows.find((l) => l.startsWith("Universe 17 →")).slice("Universe 17 →".length);
  assert(one !== seventeen, `the two universes resolved to the same output: ${rows}`);

  // Unbinding one leaves the other alone, and names the fallback.
  app.click(app.snapshot().find({ role: "button", label: "Unbind universe 17" }));
  until("the unbound row", (s) => s.findAll({ role: "row" }).some((n) => n.label === "Universe 17 → Not bound"));
  app.frames(4);
  expect(rowLabels().filter((l) => l.startsWith("Universe ")).length).toBe(2);
  expect(texts().find((l) => l.startsWith("Universe 17 has no node"))).toContain("aliases it onto 1");
});

// Auto Patch re-derives, asks before discarding an override, and says how
// much it moved.
test("auto patch moves and reports", () => {
  openPatch();
  // Pin one address by hand so Auto Patch has an override to ask about.
  typeAddress("Mover 1", reading("Mover 1 address = "), "100");
  until("the moved row", (s) => s.findAll({ role: "text" }).some((n) => n.label === "Mover 1 address = 100"));
  app.frames(4);

  app.click(app.snapshot().find({ role: "button", label: "Auto patch" }));
  until("the confirmation", (s) => s.findAll({ role: "text" }).some((n) => n.label.startsWith("Re-derive")));
  app.click(lastButton("Auto patch"));
  until("the report", (s) => s.findAll({ role: "text" }).some((n) => n.label.startsWith("Auto patch moved")));
  app.frames(6);
  expect(texts().find((l) => l.startsWith("Auto patch moved"))).toContain("discarded 1 overrides");
  // It moved the pinned row back, which is the whole claim.
  assert(reading("Mover 1 address = ") !== "100", "auto patch left the override in place");
});

// Unpatching something standing in the room asks before it takes it down.
test("unpatching a placed fixture asks first", () => {
  openPatch();
  const before = movers();
  expect(reading("Mover 2 placement = ")).toBe("placed");
  const askToUnpatch = () => {
    app.click(app.snapshot().find({ role: "row", label: "Mover 2" }), { button: "right" });
    until("the row menu", (s) => s.find({ role: "button", label: "Unpatch" }) !== undefined);
    app.click(app.snapshot().find({ role: "button", label: "Unpatch" }));
    until("the confirmation", (s) => s.findAll({ role: "text" }).some((n) => n.label.startsWith("Unpatch this fixture")));
  };
  askToUnpatch();
  // Cancelling is not a quiet yes.
  app.click(app.snapshot().find({ role: "button", label: "Cancel" }));
  app.frames(8);
  expect(movers()).toEqual(before);

  askToUnpatch();
  app.click(lastButton("Unpatch"));
  until("the row to go", (s) => s.find({ role: "row", label: "Mover 2" }) === undefined);
  app.frames(6);
  expect(movers().length).toBe(before.length - 1);
});

// The add dialog morphs from the bundle to the count, and what it makes
// continues the venue's own numbering.
test("adding N fixtures morphs and continues the numbering", () => {
  openPatch();
  const before = movers();
  app.click(app.snapshot().find({ role: "button", label: "Add fixtures" }));
  const bundle = until("the bundle", (s) => s.find({ role: "input", label: "Search fixtures…" }) !== undefined);
  app.type(bundle.find({ role: "input", label: "Search fixtures…" }), "Mover");
  until("the seeded definition", (s) => s.find({ role: "row", label: "Luma Mover" }) !== undefined);
  app.frames(4);

  // The morph: page one leaves, page two arrives, and the table behind the
  // card keeps every row for the whole flight. Only frames with the dialog.
  app.click(app.snapshot().find({ role: "row", label: "Luma Mover" }));
  const flight = app.painted()
    .filter((s) => s.find({ role: "card", label: "Add fixtures dialog" }) !== undefined)
    .map((s) => s.nodes.filter((n) => n.role === "row" && n.label.startsWith("Mover ")).length);
  assert(flight.length > 0, "the morph drew no frames");
  assert(flight.every((count) => count === before.length), `the table flashed while the dialog morphed: ${flight}`);
  until("the count page", (s) => s.findAll({ role: "input" }).some((n) => n.label.startsWith("count = ")));
  app.frames(6);
  expect(texts().find((l) => l.startsWith("Lands in"))).toContain("unplaced");
  // The mode trigger is a plate wide enough for the word on it: handed no
  // modes to size by, it collapses to its chevron. Six pixels a character is
  // a floor on "the label fits", not a measurement.
  const chip = app.snapshot().find({ role: "select", label: "Default" }).bounds;
  assert(chip.width >= 6 * "Default".length && chip.width > chip.height, `the mode chip is narrower than its word: ${JSON.stringify(chip)}`);

  app.click(app.snapshot().find({ role: "input", label: "count = 1" }));
  app.key("secondary-a 3 enter");
  until("the count", (s) => s.find({ role: "input", label: "count = 3" }) !== undefined);
  app.frames(2);
  app.click(app.snapshot().find({ role: "button", label: "Add" }));
  // The rig ships Mover 0–3, so the mint continues at 4.
  until("three more rows", (s) => s.find({ role: "row", label: "Mover 6" }) !== undefined);
  app.frames(6);
  const after = movers();
  for (const name of ["Mover 4", "Mover 5", "Mover 6"]) expect(after).toContain(name);
  expect(after.length).toBe(before.length + 3);
  // What the button made true, in the page's word for a fixture with no place.
  expect(texts().find((l) => l.startsWith("Added "))).toBe("Added 3 fixtures, unplaced");
});

// Two modes over the rig's one, at eight and sixteen channels.
const TWO_MODES = `<?xml version="1.0" encoding="UTF-8"?>
<FixtureDefinition>
 <Manufacturer>Luma</Manufacturer>
 <Model>Mover</Model>
 <Type>Moving Head</Type>
 <Channel Name="Dimmer" Preset="IntensityMasterDimmer"/>
 <Mode Name="Default">
  <Channel Number="0">Dimmer</Channel>
  <Channel Number="1">Dimmer</Channel>
  <Channel Number="2">Dimmer</Channel>
  <Channel Number="3">Dimmer</Channel>
  <Channel Number="4">Dimmer</Channel>
  <Channel Number="5">Dimmer</Channel>
  <Channel Number="6">Dimmer</Channel>
  <Channel Number="7">Dimmer</Channel>
 </Mode>
 <Mode Name="Extended">
  <Channel Number="0">Dimmer</Channel>
  <Channel Number="1">Dimmer</Channel>
  <Channel Number="2">Dimmer</Channel>
  <Channel Number="3">Dimmer</Channel>
  <Channel Number="4">Dimmer</Channel>
  <Channel Number="5">Dimmer</Channel>
  <Channel Number="6">Dimmer</Channel>
  <Channel Number="7">Dimmer</Channel>
  <Channel Number="8">Dimmer</Channel>
  <Channel Number="9">Dimmer</Channel>
  <Channel Number="10">Dimmer</Channel>
  <Channel Number="11">Dimmer</Channel>
  <Channel Number="12">Dimmer</Channel>
  <Channel Number="13">Dimmer</Channel>
  <Channel Number="14">Dimmer</Channel>
  <Channel Number="15">Dimmer</Channel>
 </Mode>
 <Physical>
  <Dimensions Weight="10" Width="300" Height="400" Depth="300"/>
  <Lens Name="Fixed" DegreesMin="14" DegreesMax="14"/>
 </Physical>
</FixtureDefinition>
`;

// A wider mode does not fit where the fixture stands, so the refusal becomes
// the question, and answering it repatches.
test("a wider mode asks before it moves the fixture", { fixture: { files: { "fixtures/Luma/Mover.qxf": TWO_MODES } } }, () => {
  openPatch();
  const before = reading("Mover 0 address = ");
  app.click(app.snapshot().find({ role: "select", label: "Mover 0 mode = Default" }));
  until("the mode menu", (s) => s.find({ role: "button", label: "Extended · 16 ch" }) !== undefined);
  expect(app.snapshot().findAll({ role: "button" }).map((n) => n.label).filter((l) => l.includes(" ch")))
    .toEqual(["Default · 8 ch", "Extended · 16 ch"]);

  // Mover 1 sits eight channels on, so sixteen do not fit.
  app.click(app.snapshot().find({ role: "button", label: "Extended · 16 ch" }));
  until("the question", (s) => s.findAll({ role: "text" }).some((n) => n.label.startsWith("Move Mover 0")));
  app.frames(4);
  // Asked, not done: nothing is written while the question stands.
  expect(chosen("Mover 0 mode = ")).toBe("Default");
  expect(texts().find((l) => l.includes("collides with"))).toContain("collides with Mover 1");

  app.click(lastButton("Repatch"));
  until("the repatched row", (s) => s.findAll({ role: "select" }).some((n) => n.label === "Mover 0 mode = Extended"));
  app.frames(6);
  expect(chosen("Mover 0 mode = ")).toBe("Extended");
  // Sixteen channels wide, and somewhere they fit — which is somewhere else.
  const [start, last] = reading("Mover 0 range = ").split("–").map(Number);
  expect(last - start + 1).toBe(16);
  assert(reading("Mover 0 address = ") !== before, "it repatched without moving");
});

test("venue groups are editable in the narrow panel", { fixture: { window: [1100, 900] } }, () => {
  const included = () => app.snapshot().findAll({ role: "checkbox" }).filter((n) => n.focused).map((n) => n.label);
  nav.patch("Test Venue");
  until("the lights", (s) => s.find({ role: "row", label: "Mover 0" }));
  nav.step("a light", "row", "Mover 0");
  nav.step("make a group", "button", "Group selected lights");
  app.key("f r o n t space w a s h");
  nav.step("save the group", "button", "Save group");
  until("the saved group", (s) => s.find({ role: "row", label: "Group front wash" }));
  nav.step("edit the group", "button", "Edit group front wash");
  expect(included()).toEqual(["Include Mover 0"]);
  nav.step("include another", "checkbox", "Include Mover 1");
  nav.step("save the membership", "button", "Save group");
  until("saved", (s) => s.find({ role: "button", label: "Edit group front wash" }));
  nav.step("rename the group", "button", "Edit group front wash");
  app.key("secondary-a b a c k space w a s h");
  nav.step("save the rename", "button", "Save group");
  until("renamed", (s) => s.find({ role: "row", label: "Group back wash" }));
  nav.step("verify", "button", "Edit group back wash");
  expect(included()).toEqual(["Include Mover 0", "Include Mover 1"]);
  nav.step("cancel", "button", "Cancel group edit");
  until("the builder", (s) => s.find({ role: "button", label: "Add element" }));
  expect(app.snapshot().find({ role: "button", label: "Add fixtures" }) !== undefined).toBe(true);
});

test("fixture table edits and scene controls share selection", () => {
  nav.patch("Test Venue");
  // The old per-kind tabs are gone.
  expect(app.snapshot().findAll({ role: "toggle" }).filter((n) => ["Stage", "Fixtures", "Groups"].includes(n.label)).length).toBe(0);
  nav.step("a fixture", "row", "Mover 0");
  nav.step("rename it", "text", "Mover 0 label = Mover 0");
  app.key("secondary-a w a s h space 1 enter");
  until("renamed", (s) => s.find({ role: "row", label: "wash 1" }));
  nav.step("edit the universe", "text", "wash 1 universe = 1");
  app.key("secondary-a 2 enter");
  until("the universe saved", (s) => s.find({ role: "text", label: "wash 1 universe = 2" }));
  nav.step("edit the address", "text", "wash 1 address = 1");
  app.key("secondary-a 6 5 enter");
  until("the address saved", (s) => s.find({ role: "text", label: "wash 1 address = 65" }));
  nav.step("edit a group", "button", "Edit group right movers");
  nav.step("include the fixture", "checkbox", "Include wash 1");
  nav.step("save the membership", "button", "Save group");
  nav.step("patch details", "button", "Patch details");
  nav.expand();
  nav.stageOff();
  until("the same address in the patch", (s) => s.find({ role: "text", label: "wash 1 address = 65" }));
  nav.step("close the patch", "button", "Close patch details");
  nav.step("another fixture", "row", "Mover 1");
  nav.step("back to the fixture", "row", "wash 1");
  nav.step("verify the group", "button", "Edit group right movers");
  expect(app.snapshot().find({ role: "checkbox", label: "Include wash 1" }).focused).toBe(true);
  nav.step("cancel", "button", "Cancel group edit");
  expect(reading("wash 1 address = ")).toBe("65");
});

test("render settings follow the venue across a score and a reopen", () => {
  const sun = () => app.snapshot().findAll({ role: "text" }).find((n) => n.label.startsWith("Time of day = "))?.label;
  nav.patch("Test Venue");
  nav.step("the view settings", "toggle", "Render settings");
  app.click(app.snapshot().find({ role: "toggle", label: "Render settings" }));
  until("the settings to close", (s) => !s.find({ role: "card", label: "Render settings" }));
  // The room is venue truth: it sits on the venue page.
  nav.step("outdoor", "toggle", "Outdoor");
  until("the sun", () => sun() !== undefined);
  const before = sun();
  app.drag(app.snapshot().find({ role: "slider", label: "Time of day" }), { dx: 0, dy: -20 }, { steps: 12 });
  const changed = sun();
  assert(changed !== before, "the drag did not change the time of day");
  app.key("escape");
  until("the settings closed", (s) => !s.find({ role: "card", label: "Render settings" }));
  // Escape closes the popover above a draft first, then the draft.
  nav.step("draft a group", "button", "Create group");
  nav.step("the view over the draft", "toggle", "Render settings");
  until("the view open", (s) => s.find({ role: "card", label: "Render settings" }));
  app.key("escape");
  until("the view closed above the draft", (s) => !s.find({ role: "card", label: "Render settings" }) && s.find({ role: "button", label: "Save group" }));
  app.key("escape");
  until("the draft closed", (s) => !s.find({ role: "button", label: "Save group" }));
  nav.track("Aurora");
  nav.step("the score's view settings", "toggle", "Render settings");
  until("the score's sun", () => sun() !== undefined);
  expect(sun()).toBe(changed);
  app.key("escape");
  nav.closeTab();
  nav.closeTab();
  nav.venuePage("Test Venue");
  until("the loaded room", (s) => s.find({ role: "toggle", label: "Frame stats" }));
  nav.step("the reopened settings", "toggle", "Render settings");
  until("the saved sun", () => sun() !== undefined);
  expect(sun()).toBe(changed);
});

test(
  "the missing-group dialog repairs saved score selectors",
  {
    fixture: {
      clips: [{ pattern: "a", name: "Lost", start: 0, end: 2, preset: "Chase", selection: "lost_wash" }],
    },
  },
  () => {
    nav.patch("Test Venue");
    nav.step("repair", "button", "Resolve missing groups");
    until("the dialog", (s) => s.find({ role: "card", label: "Group repair dialog" }));
    nav.step("a replacement", "toggle", "Use group left_movers");
    nav.step("apply the repair", "button", "Fix affected scores");
    until("resolved", (s) => !s.find({ role: "button", label: "Resolve missing groups" }) && !s.find({ role: "button", label: "Fix affected scores" }));
    expect(library.score().clips.a.selection.expression).toBe("left_movers");
  },
);

test("procedural haze controls are editable and survive environment switches", () => {
  nav.patch("Test Venue");
  until("the environment", (s) => s.find({ role: "card", label: "Venue environment" }));
  const field = (name) => app.snapshot().findAll({ role: "slider" }).find((n) => n.label.startsWith(`${name} = `));
  const densityAt = (fraction) => {
    const box = app.snapshot().find({ role: "slider", label: "Haze density" }).bounds;
    app.drag({ x: box.x + box.width / 2, y: box.y + box.height / 2 }, { dx: box.width * (fraction - 0.5), dy: 0 }, { steps: 8 });
    app.frames(3);
    return Number(field("Haze density").label.split(" = ")[1]);
  };
  // The density maps position to value: off at the left, rising to the right.
  const [low, mid, high] = [densityAt(0), densityAt(0.2), densityAt(1)];
  expect(low).toBe(0);
  assert(mid > low && high > mid, `the density does not rise with the pointer: ${[low, mid, high]}`);
  const names = ["Haze density", "Cloudiness", "Cloud size (m)", "Turbulence", "Wind speed (m/s)", "Wind direction (°)"];
  const read = () => names.map((n) => field(n).label);
  const before = read();
  // The panel is shorter than its rows: wheel it until the slider is on
  // screen (a clipped node has no height).
  const reveal = (name) => {
    for (let i = 0; i < 12 && field(name).bounds.height === 0; i++) {
      app.scroll(app.snapshot().find({ role: "card", label: "Venue environment" }), { dy: -60, steps: 2 });
      app.frames(2);
    }
    assert(field(name).bounds.height > 0, `${name} never scrolled into view`);
    return field(name).bounds;
  };
  for (const name of names) {
    const box = reveal(name);
    app.drag({ x: box.x + 2, y: box.y + box.height / 2 }, { dx: (box.width - 4) * 0.7, dy: 0 }, { steps: 8 });
    app.frames(3);
  }
  const changed = read();
  names.forEach((name, i) => assert(changed[i] !== before[i], `a scrub did not change ${name}: ${before[i]}`));
  // Back to the top of the panel, where the kind of venue is chosen.
  app.scroll(app.snapshot().find({ role: "card", label: "Venue environment" }), { dy: 2000, steps: 4 });
  app.frames(2);
  nav.step("outdoor", "toggle", "Outdoor");
  app.frames(4);
  expect(read()).toEqual(changed);
  app.scroll(app.snapshot().find({ role: "card", label: "Venue environment" }), { dy: 2000, steps: 4 });
  app.frames(2);
  nav.step("indoor", "toggle", "Indoor");
  app.frames(4);
  expect(read()).toEqual(changed);
});

// The three tiers of the View panel, each persisted where it belongs: grid,
// gizmos and render percent are local device settings, the haze is venue
// truth, and all four come back after the venue is closed and reopened. The
// grid used to be derived from the room, so switching it off lasted until the
// next environment change — hence the switch in the middle.
test("view settings persist per device and per venue", () => {
  const scrub = (name) => app.snapshot().findAll({ role: "slider" }).find((n) => n.label.startsWith(`${name} = `));
  const value = (name) => scrub(name)?.label.split(" = ")[1] ?? null;
  const toggled = (name) => app.snapshot().find({ role: "toggle", label: name })?.focused ?? null;
  const openView = () => {
    nav.step("the view settings", "toggle", "Render settings");
    until("the view panel", (s) => s.find({ role: "card", label: "Render settings" }) !== undefined && scrub("Render scale (%)") !== undefined);
  };
  const all = () => ({ grid: toggled("Grid"), gizmos: toggled("Gizmos"), scale: value("Render scale (%)"), density: value("Haze density") });

  nav.patch("Test Venue");
  openView();
  const before = all();
  expect([before.grid, before.gizmos]).toEqual([true, true]);
  // Haze is on the venue page, outside the popover: pressing it closes the
  // popover, so it goes first.
  const density = scrub("Haze density").bounds;
  app.drag({ x: density.x + 2, y: density.y + density.height / 2 }, { dx: (density.width - 4) * 0.8, dy: 0 }, { steps: 8 });
  openView();
  nav.step("grid off", "toggle", "Grid");
  nav.step("gizmos off", "toggle", "Gizmos");
  // Leftwards from the right edge: the mapping is absolute.
  const scale = scrub("Render scale (%)").bounds;
  app.drag({ x: scale.x + scale.width - 2, y: scale.y + scale.height / 2 }, { dx: -(scale.width - 4) * 0.6, dy: 0 }, { steps: 8 });
  app.frames(4);
  const changed = all();
  expect([changed.grid, changed.gizmos]).toEqual([false, false]);
  assert(changed.scale !== before.scale, "the percent did not move");
  assert(changed.density !== before.density, "the density did not move");

  // The room changes under them: allowed to change the light, not these.
  nav.step("outdoor", "toggle", "Outdoor");
  app.frames(4);
  openView();
  expect(all()).toEqual(changed);
  app.key("escape");
  until("the settings closed", (s) => !s.find({ role: "card", label: "Render settings" }));
  nav.venuePage("Test Venue");
  until("the loaded room", (s) => s.find({ role: "toggle", label: "Frame stats" }));
  openView();
  app.frames(4);
  expect(all()).toEqual(changed);
});

test("the footage look shows its dials only while on and keeps them", () => {
  const scrub = (name) => app.snapshot().findAll({ role: "slider" }).find((n) => n.label.startsWith(`${name} = `));
  const value = (name) => Number(scrub(name)?.label.split(" = ")[1]);
  const on = () => app.snapshot().find({ role: "toggle", label: "Footage look" })?.focused;
  const openView = () => {
    nav.step("the view settings", "toggle", "Render settings");
    until("the view panel", (s) => s.find({ role: "toggle", label: "Footage look" }) !== undefined);
  };

  nav.patch("Test Venue");
  openView();
  expect(on()).toBe(false);
  expect(scrub("Sensor noise")).toBe(undefined);
  nav.step("footage on", "toggle", "Footage look");
  until("its dials", () => scrub("Sensor noise") !== undefined);
  expect(value("Sensor noise")).toBe(0.3);
  for (const dial of ["Handheld shake", "Bass shake"]) {
    assert(scrub(dial) !== undefined, `no ${dial} dial`);
  }
  // The shutter angle and the rolling readout are gone.
  for (const dial of ["Shutter angle", "Readout time"]) {
    expect(app.snapshot().findAll({ role: "slider" }).some((n) => n.label.startsWith(dial))).toBe(false);
  }

  app.key("escape");
  until("the settings closed", (s) => !s.find({ role: "card", label: "Render settings" }));
  nav.venuePage("Test Venue");
  until("the loaded room", (s) => s.find({ role: "toggle", label: "Frame stats" }));
  openView();
  expect(on()).toBe(true);
  expect(value("Sensor noise")).toBe(0.3);
});
