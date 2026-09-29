// The edit-focus layout and the searchable dialogs, driven through input.

const CLIP = { pattern: "pat-glow", name: "Glow", start: 2, end: 5 };
fixture({ seconds: 20, clips: [CLIP], rig: 4, window: [1400, 900] });

const node = (role, label) => app.snapshot().find({ role, label });
// A clip is labelled by its form; the fixture's clip plays Color.
const CARD = "Wash";

test(
  "the inspector stays open and swaps between presets and clip inputs",
  { fixture: { motion: true, motion_scale: 4 } },
  () => {
    const read = () => {
      const shot = app.snapshot();
      const inspector = shot.find({ role: "card", label: "Clip graph" }) ?? shot.find({ role: "card", label: "Presets" });
      return {
        showing: inspector?.label ?? null,
        width: inspector?.bounds.width ?? 0,
        stage: shot.find({ role: "card", label: "Stage" }).bounds,
        waveform: shot.find({ role: "card", label: "Waveform" }).bounds,
      };
    };
    const sample = () => {
      const frames = [];
      for (let i = 0; i < 5; i++) {
        app.frames(1, { waitMs: 20 });
        frames.push(read());
      }
      return frames;
    };
    nav.trackEditor("Test Venue", "Aurora");
    nav.expand();
    until("the clip", () => node("card", CARD));
    until("the presets", () => node("card", "Presets"));
    const empty = read();
    expect(empty.showing).toBe("Presets");
    app.click(node("card", CARD));
    const selecting = sample();
    until("the clip's controls", () => node("button", "Pick fixtures"));
    const opened = read();
    expect(opened.showing).toBe("Clip graph");
    app.click(node("card", "Waveform"));
    const clearing = sample();
    until("the presets again", () => node("card", "Presets"));
    const cleared = read();
    expect(cleared.showing).toBe("Presets");

    // The inspector never slides, and the stage and timeline keep their space.
    for (const frame of [opened, cleared, ...selecting, ...clearing]) {
      assert(Math.abs(frame.width - empty.width) < 1, `the inspector changed width: ${empty.width} → ${frame.width}`);
      expect(frame.stage).toEqual(empty.stage);
      expect(frame.waveform).toEqual(empty.waveform);
    }
  },
);

test("the inspector stays above the timeline and pickers accept keyboard input", () => {
  nav.venue("Test Venue");
  nav.track("Aurora");
  nav.expand();
  until("the clip", () => node("card", CARD));
  app.click(node("card", CARD));
  until("the clip's controls", () => node("button", "Pick fixtures"));
  const before = node("card", "Clip graph").bounds;
  app.drag(node("slider", "Stage height"), { dx: 0, dy: 140 });
  app.frames(4);
  const inspector = node("card", "Clip graph").bounds;
  const wave = node("card", "Waveform").bounds;
  // A shorter stage gives the inspector room, and it stays above the
  // timeline with its controls on screen.
  expect(inspector.height >= before.height).toBe(true);
  assert(inspector.y + inspector.height <= wave.y, `the inspector overlaps the timeline: ${JSON.stringify({ inspector, wave })}`);
  expect(node("button", "Pick fixtures").bounds.height).toBeGreaterThan(0);

  app.click(node("button", "Pick fixtures"));
  until("the selection search", () => node("input", "Search groups…"));
  // Hovering a row previews it, and the preview holds just past its edge.
  app.scroll(node("checkbox", "left_movers"), { dy: 0 });
  app.frames(2);
  const left = node("checkbox", "left_movers").bounds;
  app.scroll({ x: left.x + left.width / 2, y: left.y + left.height + 1 }, { dy: 0 });
  assert(node("text", "Preview: left_movers"), "the preview did not hold");
  app.type(node("input", "Search groups…"), "right");
  app.frames(3);
  expect(app.snapshot().findAll({ role: "checkbox" }).map((n) => n.label)).toEqual(["right_movers"]);
  app.key("down enter");
  app.click(node("button", "Apply"));
  until("the selection applied", (s) => s.findAll({ role: "input" }).some((n) => n.label === "expression = right_movers"));

  app.click(node("row", "Lane 0"), { button: "right" });
  until("the pattern dialog", () => node("card", "Insert pattern dialog"));
  expect(node("card", "Insert pattern dialog").bounds.height).toBeGreaterThan(0);
  app.type(node("input", "Search patterns…"), "Wash");
  app.frames(3);
  expect(app.snapshot().findAll({ role: "row" }).filter((n) => n.label === "Wash").length).toBe(1);
  app.key("enter");
  until("the inserted clip", (s) => s.findAll({ role: "card", label: CARD }).length === 2);
  expect(node("card", "Insert pattern dialog")).toBe(undefined);
});
