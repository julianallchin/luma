// Split view: a General setting that lays the score editor out as the
// timeline on the left, and the stage over the inspector on the right.
//
// The claims are relations between boxes, not pixel values: which side of the
// timeline the inspector is on, whether it is under the stage, and that the
// column seam moves the timeline's edge and a double-click puts it back. The
// setting goes through the seam and back, and the layout follows it without a
// restart.

fixture({ seconds: 8, rig: 4 });

const box = (shot, role, label) => shot.find({ role, label })?.bounds;

// Press the Split view row on General, then leave settings. The automation
// tree carries no checked state, so the caller waits for the layout instead.
function toggleSplitView() {
  app.action("luma::OpenSettings");
  const shot = until("the loaded General settings", (s) =>
    s.find({ role: "checkbox", label: "Split view" }));
  app.click(shot.find({ role: "checkbox", label: "Split view" }));
  const settled = until("the re-read after the write", (s) =>
    s.find({ role: "checkbox", label: "Split view" }));
  const failed = settled.nodes.some((n) => (n.label || "").startsWith("Failed to save"));
  assert(!failed, "the Split view write reported a save error");
  nav.dismiss();
}

// The stacked layout: stage and inspector in one row over the timeline.
function assertStacked(shot) {
  const presets = box(shot, "card", "Presets");
  const timeline = box(shot, "card", "Waveform");
  assert(presets && timeline, "the inspector or the timeline is missing");
  assert(timeline.y >= presets.y + presets.height - 1,
    `the timeline is not under the inspector: ${JSON.stringify({ presets, timeline })}`);
}

test("split view puts the timeline beside the stage and the inspector", () => {
  nav.trackEditor("Test Venue", "Aurora");
  // Room for both columns above their floors, so the seam has somewhere to go.
  nav.expand();
  assertStacked(until("the stacked editor", (s) =>
    box(s, "card", "Waveform") && box(s, "card", "Presets") && box(s, "slider", "Stage height")));

  toggleSplitView();
  const split = until("the split layout", (s) =>
    box(s, "slider", "Score editor width") && box(s, "card", "Waveform") && box(s, "card", "Presets"));
  const timeline = box(split, "card", "Waveform");
  const presets = box(split, "card", "Presets");
  const stage = box(split, "slider", "Stage height");
  assert(stage, "the stage is gone in split view");
  assert(presets.x >= timeline.x + timeline.width - 1,
    `the inspector is not right of the timeline: ${JSON.stringify({ timeline, presets })}`);
  assert(presets.y >= stage.y,
    `the inspector is not under the stage: ${JSON.stringify({ stage, presets })}`);
  // The stage seam spans the right column only.
  assert(stage.x >= timeline.x + timeline.width - 1, "the stage seam reaches over the timeline");

  // The column seam moves the timeline's edge, and a double-click restores it.
  const dx = -80;
  app.drag(split.find({ role: "slider", label: "Score editor width" }), { dx, dy: 0 }, { steps: 10 });
  const narrowed = until("the narrower timeline", (s) =>
    box(s, "card", "Waveform")?.width < timeline.width - 40);
  assert(box(narrowed, "card", "Presets").width > presets.width, "the right column did not take the room");
  app.click(narrowed.find({ role: "slider", label: "Score editor width" }), { count: 2 });
  until("the default split again", (s) =>
    Math.abs(box(s, "card", "Waveform").width - timeline.width) <= 2);

  // The stage seam divides the right column: pulling it up grows the inspector.
  app.drag(app.snapshot().find({ role: "slider", label: "Stage height" }), { dx: 0, dy: -60 }, { steps: 10 });
  until("the taller inspector", (s) => box(s, "card", "Presets")?.y < presets.y - 30);

  // Off again, live: the stacked layout comes back.
  toggleSplitView();
  assertStacked(until("the stacked editor again", (s) =>
    !box(s, "slider", "Score editor width") && box(s, "card", "Waveform") && box(s, "card", "Presets")));
});
