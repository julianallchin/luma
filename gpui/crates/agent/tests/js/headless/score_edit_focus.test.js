// The searchable dialogs of score editing, driven through input.

const CLIP = { pattern: "pat-glow", name: "Glow", start: 2, end: 5 };
fixture({ seconds: 20, clips: [CLIP], rig: 4, window: [1400, 900] });

const node = (role, label) => app.snapshot().find({ role, label });
// A clip is labelled by its name; the fixture's clip is a Wash.
const CARD = "Wash";

test("the clip's pickers and the insertion dialog accept keyboard input", () => {
  nav.venue("Test Venue");
  nav.track("Aurora");
  nav.agentOff();
  until("the clip", () => node("card", CARD));
  nav.clipGraph(CARD);
  until("the clip's controls", () => node("button", "Pick fixtures"));
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

  // Back on the timeline, a right-click on a lane offers the patterns.
  app.click(node("button", "Back to timeline"));
  until("the timeline", () => node("row", "Lane 0"));
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
