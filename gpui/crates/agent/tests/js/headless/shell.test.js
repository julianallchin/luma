// The shell: a venue in one of three modes, the song list over Arrange, and
// the agent panel beside them (`docs/specs/comet-shell.md`).
//
// The claims are relations a person sees: which body is up after a key, what
// closes what, and which side of the agent panel the window's corner
// controls sit on.

fixture({
  seconds: 8,
  rig: 4,
  clips: [{ pattern: "shell-clip", name: "Chase", start: 1, end: 3, preset: "Chase" }],
  window: [1400, 900],
});

const box = (shot, role, label) => shot.find({ role, label })?.bounds;
const songList = (shot) => box(shot, "card", "Song list") !== undefined;

test("1, 2 and 3 switch between Rig, Arrange and Live", () => {
  nav.trackEditor("Test Venue", "Aurora");
  expect(nav.shownMode(app.snapshot())).toBe("Arrange");
  for (const [key, mode] of [["1", "Rig"], ["3", "Live"], ["2", "Arrange"], ["3", "Live"], ["1", "Rig"]]) {
    app.key(key);
    until(`${mode} after ${key}`, (s) => nav.shownMode(s) === mode);
  }
  // Each mode keeps its body: back in Arrange, the score is still open.
  app.key("2");
  until("the score again", (s) => box(s, "card", "Waveform") && !songList(s));
});

test("a key in a text field types instead of switching mode", () => {
  nav.venue("Test Venue");
  nav.songs();
  app.click(app.snapshot().find({ role: "input", label: "Search tracks" }));
  app.type(app.snapshot().find({ role: "input", label: "Search tracks" }), "2");
  app.frames(4);
  const after = app.snapshot();
  expect(nav.shownMode(after)).toBe("Arrange");
  // A field with a query is labelled by it.
  assert(after.findAll({ role: "input" }).some((n) => n.label === "2"), "the key did not type into the search field");
});

test("choosing a score on the song list opens it and closes the list", () => {
  nav.venue("Test Venue");
  // With nothing open, Arrange is the song list.
  until("the song list", (s) => songList(s));
  nav.scores("Aurora");
  const score = app.snapshot().findAll({ role: "row" }).find((n) => n.label.startsWith("#"));
  app.click(score ?? app.snapshot().find({ role: "button", label: "New score" }));
  const open = until("the score, and the list gone", (s) =>
    box(s, "card", "Waveform") && !songList(s) ? s : undefined);
  // The title bar names the open score, and pressing it brings the list back.
  const title = open.findAll({ role: "button" }).find((n) => n.label.startsWith("Aurora · #"));
  assert(title, "the title bar does not name the open score");
  app.click(title);
  until("the song list over the score", (s) => songList(s) && !box(s, "card", "Waveform"));
});

test("Escape backs out of the clip graph, then the song list", () => {
  nav.trackEditor("Test Venue", "Aurora");
  const rest = until("the timeline", (s) => box(s, "card", "Waveform") && box(s, "card", "Chase") ? s : undefined);
  const timeline = box(rest, "card", "Waveform");

  // A double-click opens the clip's graph where the timeline was.
  app.click(rest.find({ role: "card", label: "Chase" }), { count: 2 });
  const page = until("the graph page", (s) =>
    box(s, "button", "Back to timeline") && box(s, "card", "Graph canvas") ? s : undefined);
  assert(!box(page, "card", "Waveform"), "the timeline is still up under the graph page");
  const graph = box(page, "card", "Pattern graph");
  assert(Math.abs(graph.x - timeline.x) <= 2 && Math.abs(graph.width - timeline.width) <= 2,
    `the graph is not where the timeline was: ${JSON.stringify({ timeline, graph })}`);
  // The page is the graph's key scope: Delete there does not delete the clip.
  app.key("delete");
  app.frames(4);
  assert(library.score().clips["shell-clip"], "Delete on the graph page deleted the clip it shows");

  app.key("escape");
  until("the timeline after Escape", (s) => box(s, "card", "Waveform") && !box(s, "button", "Back to timeline"));

  app.action("luma::ToggleSongList");
  until("the song list", (s) => songList(s));
  app.key("escape");
  until("the score after Escape", (s) => box(s, "card", "Waveform") && !songList(s));
  // With nothing left to back out of, Escape does not bring the list back.
  app.key("escape");
  app.frames(4);
  assert(!songList(app.snapshot()), "Escape on the timeline opened the song list");
});

test("hiding the agent panel moves the window's corner controls into the title bar", () => {
  nav.trackEditor("Test Venue", "Aurora");
  const shown = until("the agent panel", (s) => box(s, "card", "Agent") && box(s, "button", "Hide agent") ? s : undefined);
  const panel = box(shown, "card", "Agent");
  const bar = box(shown, "card", "Title bar");
  const toggle = box(shown, "button", "Hide agent");
  // Over the agent panel: the toggle sits in the panel's head, and the title
  // bar starts where the panel ends.
  assert(toggle.x + toggle.width <= panel.x + panel.width, `the toggle is not over the agent panel: ${JSON.stringify({ toggle, panel })}`);
  assert(bar.x >= panel.x + panel.width - 1, `the title bar overlaps the agent panel: ${JSON.stringify({ bar, panel })}`);

  app.action("luma::ToggleAgent");
  const hidden = until("the agent panel hidden", (s) =>
    !box(s, "card", "Agent") && box(s, "button", "Show agent") ? s : undefined);
  const wide = box(hidden, "card", "Title bar");
  const moved = box(hidden, "button", "Show agent");
  expect(wide.x).toBeLessThan(bar.x);
  // The toggle now sits inside the title bar, ahead of everything it holds.
  assert(moved.x >= wide.x && moved.x + moved.width <= wide.x + wide.width,
    `the toggle is not in the title bar: ${JSON.stringify({ moved, wide })}`);
  const venue = box(hidden, "button", "Test Venue");
  assert(venue.x >= moved.x + moved.width, `the venue name sits under the corner controls: ${JSON.stringify({ venue, moved })}`);

  app.action("luma::ToggleAgent");
  until("the agent panel back", (s) => box(s, "card", "Agent") && box(s, "button", "Hide agent"));
});
