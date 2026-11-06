// The composer that comes back when its tab does is the one that left, as it
// left it: not faded out and entering again. Opacity is only visible in
// pixels, which is why this half of `headless/composer_tab_switch.test.js`
// lives here.
//
// Stretched 10x so an entrance, if there were one, spans several shots.

fixture({
  seconds: 20,
  rig: 4,
  window: [1600, 900],
  motion: true,
  motion_scale: 10,
});

const SAMPLES = 5;
const GAP_MS = 200;

const chips = (s) => s.findAll({ role: "button" })
  .map((n) => n.label)
  .filter((label) => label.startsWith("Aurora · #") || label === "Venue");
const field = (s) => s.findAll({ role: "input" })
  .find((n) => n.label === "Do anything…" || n.label === DRAFT);
const DRAFT = "Light the stage";

// The pill: from the field's leading edge to past the send button, and a
// little above and below both.
const pill = (s) => {
  const input = field(s).bounds;
  const send = s.find({ role: "button", label: "Send" }).bounds;
  const x = input.x - 8;
  const y = Math.min(input.y, send.y) - 8;
  return {
    x,
    y,
    width: send.x + send.width + 8 - x,
    height: Math.max(input.y + input.height, send.y + send.height) + 8 - y,
  };
};

const burst = () => {
  const shots = [];
  for (let i = 0; i < SAMPLES; i += 1) {
    app.frames(1, { waitMs: GAP_MS });
    shots.push(app.screenshot());
  }
  return shots;
};

test("a tab's composer does not enter again when its tab comes back", { timeoutMs: 180000 }, () => {
  nav.trackEditor("Test Venue", "Aurora");
  until("the composer", (s) => field(s)?.bounds.width > 0);
  app.type(field(app.snapshot()), DRAFT);
  // Long enough for the panel's own entrance to have played out.
  app.frames(12, { waitMs: 400 });
  const rect = pill(app.snapshot());
  // What the pill looks like at rest, caret blink included: the noise floor.
  const rest = burst();
  const noise = Math.max(...rest.map((shot) => image.diff(rest[0], shot, { rect })));

  nav.venuePage("Test Venue");
  until("the venue tab", (s) => chips(s).includes("Venue"));
  app.frames(12, { waitMs: 400 });
  nav.step("Aurora's chip", "button", chips(app.snapshot()).find((label) => label !== "Venue"));
  until("Aurora's composer", (s) => field(s)?.label === DRAFT);
  const back = burst();
  const drift = back.map((shot) => image.diff(rest[0], shot, { rect }));
  // Allowance for a caret that blinks between the two bursts.
  for (const d of drift) {
    assert(d <= noise + 0.01, `the pill came back changed: ${JSON.stringify(drift)} against noise ${noise}`);
  }
});
