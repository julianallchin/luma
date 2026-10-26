// The window's two layouts, switched by the pill at the head band's trailing
// edge. Direct: the chat beside the stage over the timeline, and no
// inspector. Program: no chat, the timeline beside the stage over the
// inspector. In Direct a double-click on a clip puts its graph where the
// timeline was, and the way back is a button or Escape.
//
// The claims are relations between boxes, not pixel values: which side of the
// timeline the inspector is on, whether it is under the stage, whether the
// chat is there at all. The choice goes through the seam to the settings
// table, and the layout follows it without a restart.

fixture({
  seconds: 8,
  rig: 4,
  clips: [{ pattern: "mode-clip", name: "Chase", start: 1, end: 3, preset: "Chase" }],
  window: [1400, 900],
});

const box = (shot, role, label) => shot.find({ role, label })?.bounds;
const chatShown = (shot) => box(shot, "slider", "Workspace width") !== undefined;
const stored = () => library.query("SELECT value FROM settings WHERE key = 'view_mode'")[0]?.value;

// Press one half of the pill.
function choose(mode) {
  const shot = until(`the ${mode} cell`, (s) => s.find({ role: "toggle", label: mode }));
  app.click(shot.find({ role: "toggle", label: mode }));
  app.frames(2);
}

// Direct: the chat and the panel share the room, and the panel is the stage
// over the timeline with nothing beside either.
function assertDirect(shot) {
  assert(chatShown(shot), "Direct has no chat beside the editor");
  const timeline = box(shot, "card", "Waveform");
  const stage = box(shot, "slider", "Stage height");
  assert(timeline && stage, "Direct lost the timeline or the stage");
  assert(timeline.y >= stage.y, `the timeline is not under the stage: ${JSON.stringify({ stage, timeline })}`);
  assert(!box(shot, "card", "Presets") && !box(shot, "card", "Clip graph"), "Direct shows an inspector");
}

test("the pill switches between Direct and Program, and remembers", () => {
  nav.trackEditor("Test Venue", "Aurora");
  const direct = until("the Direct editor", (s) =>
    box(s, "card", "Waveform") && box(s, "slider", "Stage height") && chatShown(s));
  assertDirect(direct);
  const pill = box(direct, "card", "View mode");
  assert(pill.x + pill.width <= 1400 && 1400 - (pill.x + pill.width) <= 16,
    `the pill is not at the window's trailing edge: ${JSON.stringify(pill)}`);

  choose("Program");
  const program = until("the Program layout", (s) =>
    box(s, "slider", "Score editor width") && box(s, "card", "Waveform") && box(s, "card", "Presets"));
  assert(!chatShown(program), "Program kept the chat");
  const timeline = box(program, "card", "Waveform");
  const presets = box(program, "card", "Presets");
  const stage = box(program, "slider", "Stage height");
  assert(stage, "the stage is gone in Program");
  assert(presets.x >= timeline.x + timeline.width - 1,
    `the inspector is not right of the timeline: ${JSON.stringify({ timeline, presets })}`);
  assert(presets.y >= stage.y, `the inspector is not under the stage: ${JSON.stringify({ stage, presets })}`);
  // The stage seam spans the right column only.
  assert(stage.x >= timeline.x + timeline.width - 1, "the stage seam reaches over the timeline");
  // The switch is an anchor: the layout it changes does not move it.
  expect(box(program, "card", "View mode").x).toBe(pill.x);
  until("the stored mode", () => stored() === "program");

  // The column seam moves the timeline's edge, and a double-click restores it.
  const dx = -80;
  app.drag(app.snapshot().find({ role: "slider", label: "Score editor width" }), { dx, dy: 0 }, { steps: 10 });
  const narrowed = until("the narrower timeline", (s) =>
    box(s, "card", "Waveform")?.width < timeline.width - 40);
  assert(box(narrowed, "card", "Presets").width > presets.width, "the right column did not take the room");
  app.click(narrowed.find({ role: "slider", label: "Score editor width" }), { count: 2 });
  until("the default split again", (s) =>
    Math.abs(box(s, "card", "Waveform").width - timeline.width) <= 2);

  // The stage seam divides the right column: pulling it up grows the inspector.
  app.drag(app.snapshot().find({ role: "slider", label: "Stage height" }), { dx: 0, dy: -60 }, { steps: 10 });
  until("the taller inspector", (s) => box(s, "card", "Presets")?.y < presets.y - 30);

  // A double-click in Program selects the clip into the inspector beside the
  // timeline; there is no page to go to.
  app.click(app.snapshot().find({ role: "card", label: "Chase" }), { count: 2 });
  const picked = until("the clip in the inspector", (s) => box(s, "card", "Clip graph"));
  assert(box(picked, "card", "Waveform"), "a double-click in Program took the timeline away");
  expect(box(picked, "button", "Back to timeline")).toBe(undefined);

  // And back, live.
  choose("Direct");
  assertDirect(until("Direct again", (s) =>
    !box(s, "slider", "Score editor width") && box(s, "card", "Waveform") && chatShown(s)));
  until("the stored mode", () => stored() === "direct");
});

test("a double-click in Direct opens the clip's graph where the timeline was", () => {
  nav.trackEditor("Test Venue", "Aurora");
  const rest = until("the Direct editor", (s) => box(s, "card", "Waveform") && box(s, "card", "Chase"));
  const timeline = box(rest, "card", "Waveform");

  const open = () => {
    app.click(app.snapshot().find({ role: "card", label: "Chase" }), { count: 2 });
    return until("the graph page", (s) =>
      box(s, "button", "Back to timeline") && box(s, "card", "Graph canvas") ? s : undefined);
  };
  const page = open();
  assert(!box(page, "card", "Waveform"), "the timeline is still up under the graph page");
  // In the timeline's place: the same column, under the same stage, with the
  // chat still beside it.
  const graph = box(page, "card", "Clip graph");
  assert(Math.abs(graph.x - timeline.x) <= 2 && Math.abs(graph.width - timeline.width) <= 2,
    `the graph is not where the timeline was: ${JSON.stringify({ timeline, graph })}`);
  assert(box(page, "slider", "Stage height") && chatShown(page), "the graph page took the stage or the chat");
  // The page is the graph's key scope, not the timeline's: Delete there is
  // not the timeline's "delete the selected clips".
  app.key("delete");
  app.frames(4);
  assert(library.score().clips["mode-clip"], "Delete on the graph page deleted the clip it shows");

  // The button goes back.
  app.click(app.snapshot().find({ role: "button", label: "Back to timeline" }));
  const back = until("the timeline again", (s) => box(s, "card", "Waveform") ? s : undefined);
  expect(box(back, "button", "Back to timeline")).toBe(undefined);

  // So does Escape, and it keeps the clip: a second double-click is not
  // needed to see the same clip's graph in Program.
  open();
  app.key("escape");
  until("the timeline after Escape", (s) =>
    box(s, "card", "Waveform") && !box(s, "button", "Back to timeline"));
  nav.program();
  until("the selected clip in Program's inspector", (s) => box(s, "card", "Clip graph"));
});
