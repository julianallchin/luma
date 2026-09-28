// The curve strip from the outside: one editor for a number curve, a color
// curve and a gradient. A number's point moves in x and y and takes a typed
// value; over time the strip draws the clip's beats and the playhead; across
// space it draws one tick per head of the clip's selection.

const WASH = { pattern: "form-clip", name: "Wash", start: 1, end: 3, preset: ["color@1", "Wash"] };
fixture({ seconds: 20, clips: [WASH], rig: 4, window: [1400, 1400] });

const node = (role, label) => until(label, (s) => s.find({ role, label })).find({ role, label });
const settle = () => app.frames(16, { waitMs: 60 });
const formClip = () => library.score().clips["form-clip"];

function open() {
  nav.venue("Test Venue");
  nav.track("Aurora");
  nav.expand();
  nav.stageOff();
  app.click(node("card", "Color"));
  until("the form inputs", (s) => s.find({ role: "row", label: "Brightness" }));
}

// The control of `role` and `label` inside the row named `row`.
function inRow(row, role, label) {
  const r = node("row", row).bounds;
  const found = app.snapshot().findAll({ role, label }).find((n) => n.bounds.y >= r.y && n.bounds.y < r.y + r.height);
  if (!found) throw new Error(`no ${role} ${label} in ${row}`);
  return found;
}

function promote(row, to) {
  app.click(inRow(row, "select", "Fixed"));
  app.click(node("button", to));
  until(to, (s) => s.find({ role: "select", label: to }));
  settle();
}

// Move the playhead by pressing the ruler at window x `x`.
function scrubTo(x) {
  const ruler = node("card", "Ruler").bounds;
  app.drag({ x, y: ruler.y + ruler.height / 2 }, { dx: 0, dy: 0 }, { steps: 1 });
  settle();
}

const field = (label) => app.snapshot().findAll({ role: "input" }).find((n) => n.label.startsWith(`${label} = `));

test("a number strip edits a point", () => {
  open();
  promote("Brightness", "↗ Over time");
  node("card", "Brightness strip");
  // Select the end point and type its value.
  app.click(node("slider", "Brightness point 2"));
  settle();
  expect(field("Brightness position").label).toBe("Brightness position = 100");
  app.click(field("Brightness value"));
  app.key("secondary-a backspace");
  app.type(field("Brightness value"), "0.25");
  app.key("enter");
  settle();
  const points = () => formClip().inputs.brightness.value.points;
  expect(points().at(-1)[1]).toBe(0.25);
  // The point went down in the box, and kept its x.
  const end = node("slider", "Brightness point 2").bounds;
  const start = node("slider", "Brightness point 1").bounds;
  assert(end.y > start.y, `the end point is not below the start: ${end.y} vs ${start.y}`);

  // A double-click adds a point on the curve, which changes no value.
  app.click(node("card", "Brightness strip"), { count: 2 });
  until("a point added", (s) => s.find({ role: "slider", label: "Brightness point 3" }));
  settle();
  expect(points().length).toBe(3);
  const [a, mid, b] = points();
  assert(Math.abs(mid[1] - (a[1] + (b[1] - a[1]) * mid[0])) < 1e-6, `the new point is off the line: ${JSON.stringify(points())}`);
});

test("a time strip draws the clip's beats and follows the playhead", () => {
  open();
  promote("Brightness", "↗ Over time");
  const beats = () => app.snapshot().find((n) => n.label.startsWith("Brightness beat grid = "));
  until("the beat grid", () => beats());
  // Over time, the strip spans the clip: as many beats as the clip lasts.
  expect(Number(beats().label.split(" = ")[1])).toBe(formClip().duration);

  const playhead = () => app.snapshot().find({ role: "text", label: "Brightness playhead" });
  // The track starts before the clip: no playhead on the strip.
  expect(playhead()).toBe(undefined);
  const clip = node("card", "Color").bounds;
  const strip = node("card", "Brightness strip").bounds;
  const shareOf = () => {
    const p = playhead().bounds;
    return (p.x + p.width / 2 - strip.x) / strip.width;
  };
  scrubTo(clip.x + clip.width * 0.25);
  until("the playhead", () => playhead());
  const early = shareOf();
  scrubTo(clip.x + clip.width * 0.75);
  const late = shareOf();
  // The playhead moved with the track time, and sits where the clip is.
  assert(late > early, `the playhead did not move right: ${early} → ${late}`);
  assert(Math.abs(early - 0.25) < 0.05 && Math.abs(late - 0.75) < 0.05, `playhead at ${early} and ${late}`);
  // Past the clip it goes.
  scrubTo(clip.x + clip.width + 40);
  until("no playhead", () => !playhead());
});

test("a per-hit strip spans one hit", () => {
  open();
  promote("Brightness", "↗ Per hit");
  const beats = () => app.snapshot().find((n) => n.label.startsWith("Brightness beat grid = "));
  until("the beat grid", () => beats());
  const spanned = () => Number(beats().label.split(" = ")[1]);
  // Once over the clip, a hit is the whole clip.
  expect(formClip().inputs.every.value).toBe(0);
  expect(spanned()).toBe(formClip().duration);
  // A hit every beat spans one beat.
  app.click(node("button", "Every Beats"));
  const every = () => app.snapshot().findAll({ role: "input" }).find((n) => n.label.startsWith("Every: Beats = "));
  until("the every field", () => every());
  app.click(every());
  app.key("secondary-a backspace");
  app.type(every(), "1");
  app.key("enter");
  until("one beat", () => spanned() === 1);
  settle();
  expect(formClip().inputs.every.value).toBe(1);
});

test("a color across space shows one tick per head", () => {
  const heads = library.query("SELECT count(*) AS n FROM fixtures")[0].n;
  assert(heads > 1, `the rig has ${heads} heads`);
  open();
  promote("Color", "↗ Across space");
  const ticks = () => app.snapshot().findAll({ role: "text" }).filter((n) => n.label.startsWith("Color head "));
  until("the ticks", () => ticks().length > 0);
  expect(ticks().length).toBe(heads);
  // Along X over the whole selection, the ticks reach both ends of the strip.
  const strip = node("card", "Color strip").bounds;
  const xs = ticks().map((n) => n.bounds.x + n.bounds.width / 2);
  assert(Math.min(...xs) - strip.x < 4 && strip.x + strip.width - Math.max(...xs) < 4, `ticks at ${xs} on ${JSON.stringify(strip)}`);
  expect(new Set(xs.map(Math.round)).size).toBe(heads);

  // A narrower selection has fewer ticks.
  const expression = () => app.snapshot().find((n) => n.role === "input" && n.label.startsWith("expression = "));
  app.click(expression());
  app.key("secondary-a backspace");
  app.type(expression(), "left_movers");
  app.key("enter");
  until("fewer ticks", () => ticks().length > 0 && ticks().length < heads);
});
