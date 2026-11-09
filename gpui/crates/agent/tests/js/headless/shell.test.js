// The shell: a venue in one of three modes, the song list over Arrange, and
// the agent panel beside them (`docs/specs/comet-shell.md`).
//
// The claims are relations a person sees: which body is up after a key, what
// closes what, and that the window's frame — the title bar, the inset page,
// the status bar — holds still while the agent panel comes and goes.

const WINDOW = [1400, 900];
fixture({
  seconds: 8,
  rig: 4,
  clips: [{ pattern: "shell-clip", name: "Chase", start: 1, end: 3, preset: "Chase" }],
  window: WINDOW,
});

const box = (shot, role, label) => shot.find({ role, label })?.bounds;
const songList = (shot) => box(shot, "card", "Song list") !== undefined;
const inside = (inner, outer) => inner.x >= outer.x - 1 && inner.y >= outer.y - 1
  && inner.x + inner.width <= outer.x + outer.width + 1 && inner.y + inner.height <= outer.y + outer.height + 1;

// The window's frame: the title bar across the top holding the corner
// controls, the page inset from the window's edge below it, and the status
// bar under the page. Asserted, then returned for comparison.
function frame(shot, toggleLabel) {
  const parts = {
    bar: box(shot, "card", "Title bar"),
    page: box(shot, "card", "Page"),
    status: box(shot, "card", "Status bar"),
    toggle: box(shot, "button", toggleLabel),
    modes: box(shot, "card", "Modes"),
    venue: box(shot, "button", "Test Venue"),
  };
  const { bar, page, status, toggle, modes, venue } = parts;
  assert(bar.x <= 1 && bar.width >= WINDOW[0] - 1, `the title bar does not span the window: ${JSON.stringify(bar)}`);
  for (const [name, control] of [["agent toggle", toggle], ["mode switch", modes], ["venue menu", venue]]) {
    assert(inside(control, bar), `the ${name} is not in the title bar: ${JSON.stringify({ control, bar })}`);
  }
  assert(venue.x >= toggle.x + toggle.width, `the venue menu sits under the agent toggle: ${JSON.stringify({ venue, toggle })}`);
  assert(page.x > 1 && page.x + page.width < WINDOW[0] - 1,
    `the page is not inset from the window's sides: ${JSON.stringify(page)}`);
  assert(page.y >= bar.y + bar.height - 1, `the page overlaps the title bar: ${JSON.stringify({ page, bar })}`);
  assert(status.y >= page.y + page.height - 1 && status.y + status.height <= WINDOW[1] + 1,
    `the status bar is not under the page: ${JSON.stringify({ status, page })}`);
  return parts;
}

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
  // The title bar names the open song, and its crumb brings the list back.
  assert(open.findAll({ role: "button" }).some((n) => n.label.startsWith("Aurora · #")),
    "the title bar does not name the open score");
  nav.step("the song crumb", "button", "Aurora");
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

test("hiding the agent panel leaves the title bar and its controls where they are", () => {
  nav.trackEditor("Test Venue", "Aurora");
  const shown = until("the agent panel", (s) => box(s, "card", "Agent") && box(s, "button", "Hide agent") ? s : undefined);
  const before = frame(shown, "Hide agent");
  const panel = box(shown, "card", "Agent");
  // The chat sits flush at the page's left, under the title bar.
  assert(Math.abs(panel.x - before.page.x) <= 1 && panel.y >= before.bar.y + before.bar.height - 1,
    `the agent panel is not at the page's left under the title bar: ${JSON.stringify({ panel, page: before.page })}`);

  app.action("luma::ToggleAgent");
  const hidden = until("the agent panel hidden", (s) =>
    !box(s, "card", "Agent") && box(s, "button", "Show agent") ? s : undefined);
  const after = frame(hidden, "Show agent");
  // Hiding the chat takes its panel off the page and nothing else.
  for (const part of ["bar", "toggle", "modes", "venue", "page"]) {
    const [was, is] = [before[part], after[part]];
    assert(Math.abs(was.x - is.x) <= 1 && Math.abs(was.y - is.y) <= 1 && Math.abs(was.width - is.width) <= 1,
      `hiding the agent moved the ${part}: ${JSON.stringify({ was, is })}`);
  }

  app.action("luma::ToggleAgent");
  until("the agent panel back", (s) => box(s, "card", "Agent") && box(s, "button", "Hide agent"));
});

test("the song list takes the agent panel's room and gives it back", () => {
  nav.trackEditor("Test Venue", "Aurora");
  const agent = (s) => box(s, "card", "Agent") !== undefined;
  until("the agent beside the score", (s) => agent(s) && box(s, "button", "Hide agent"));

  // Opening the list puts the panel away; closing it brings it back.
  app.action("luma::ToggleSongList");
  const listed = until("the song list, full width", (s) => songList(s) && !agent(s) ? s : undefined);
  const list = box(listed, "card", "Song list");
  const page = box(listed, "card", "Page");
  assert(list.x <= page.x + 1, `the song list does not start at the page's edge: ${JSON.stringify({ list, page })}`);
  app.action("luma::ToggleSongList");
  until("the agent back", (s) => !songList(s) && agent(s));

  // A panel put away by hand stays away when the list closes.
  app.action("luma::ToggleAgent");
  until("the agent hidden", (s) => !agent(s));
  app.action("luma::ToggleSongList");
  until("the song list", (s) => songList(s));
  app.action("luma::ToggleSongList");
  until("the score again", (s) => !songList(s) && box(s, "card", "Waveform"));
  app.frames(4);
  assert(!agent(app.snapshot()), "closing the song list brought back a panel hidden by hand");
});

test("Live's deck panel sits beside the stage and keeps its dragged width", () => {
  nav.venue("Test Venue");
  nav.live();
  const shot = until("the deck panel", (s) =>
    box(s, "card", "Deck panel") && box(s, "slider", "Deck panel width") ? s : undefined);
  const panel = box(shot, "card", "Deck panel");
  const stage = box(shot, "card", "Stage");
  // Beside the stage, as tall as it.
  assert(panel.x + panel.width <= stage.x + 1, `the panel is not left of the stage: ${JSON.stringify({ panel, stage })}`);
  assert(Math.abs(panel.y - stage.y) <= 1 && Math.abs(panel.height - stage.height) <= 2,
    `the panel does not run the stage's height: ${JSON.stringify({ panel, stage })}`);
  // The old strip's contents moved: the tapped tempo is in the panel.
  const tempoOf = (s) => s.findAll({ role: "card" }).find((n) => n.label.startsWith("Tapped tempo"));
  const t = tempoOf(until("the tapped tempo", (s) => (tempoOf(s) ? s : undefined))).bounds;
  assert(t.x >= panel.x && t.x + t.width <= panel.x + panel.width + 1,
    `the tapped tempo is not in the deck panel: ${JSON.stringify({ t, panel })}`);

  const grip = box(shot, "slider", "Deck panel width");
  app.drag({ x: grip.x + grip.width / 2, y: grip.y + grip.height / 2 }, { dx: 80, dy: 0 }, { steps: 8 });
  const wider = until("the wider panel", (s) => box(s, "card", "Deck panel").width > panel.width + 40 ? s : undefined);
  const width = box(wider, "card", "Deck panel").width;
  expect(box(wider, "card", "Stage").width).toBeLessThan(stage.width);

  // Leaving Live and coming back keeps the width.
  nav.mode("Arrange");
  nav.live();
  const back = until("the panel again", (s) => box(s, "card", "Deck panel"));
  expect(Math.abs(box(back, "card", "Deck panel").width - width)).toBeLessThan(1);
});
