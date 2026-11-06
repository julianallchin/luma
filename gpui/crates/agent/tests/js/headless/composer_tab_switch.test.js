// A composer lives as long as its chat. Switching tabs only changes which
// tab's composer is on screen: the one that comes back is the one that left,
// with its draft and its height. That it does not fade in again is only
// visible in pixels: `pixel/composer_tab_switch.test.js`.

fixture({
  seconds: 20,
  rig: 4,
  window: [1600, 900],
  motion: true,
});

const chips = (s) => s.findAll({ role: "button" })
  .map((n) => n.label)
  .filter((label) => label.startsWith("Aurora · #") || label === "Venue");
const field = (s) => s.findAll({ role: "input" })
  .find((n) => n.label === "Do anything…" || n.label.startsWith("Light the"));
const DRAFT = "Light the stage\nwarm, then cold";

test("a tab's composer survives switching away and back", () => {
  nav.trackEditor("Test Venue", "Aurora");
  until("the composer", (s) => {
    const input = field(s);
    return input && input.bounds.width > 0;
  });
  // Two lines, so the pill has grown past its compact height: a composer
  // rebuilt from scratch would show at the compact height and morph back.
  app.type(field(app.snapshot()), DRAFT);
  const drafted = until("the draft", (s) => field(s)?.label === DRAFT ? s : undefined);
  const before = field(until("the pill to settle", (s) => {
    const input = field(s);
    return input && s.frame > drafted.frame + 20 ? s : undefined;
  }));

  nav.venuePage("Test Venue");
  until("the venue tab", (s) => chips(s).includes("Venue"));
  nav.step("Aurora's chip", "button", chips(app.snapshot()).find((label) => label !== "Venue"));
  const back = until("Aurora's composer", (s) => field(s)?.label === DRAFT ? s : undefined);

  // From the first frame it is back, and in every frame after, the pill is
  // where and how big it was: no entry, no morph.
  app.frames(20);
  const shown = app.painted().filter((s) => s.frame >= back.frame && field(s));
  assert(shown.length > 1, "too few frames to see a morph");
  for (const s of shown) {
    const after = field(s);
    expect(after.label).toBe(DRAFT);
    expect(after.bounds).toEqual(before.bounds);
  }
});
