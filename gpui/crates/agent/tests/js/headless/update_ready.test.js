// The update prompt: once a new version is installed, one dialog says so and
// lists what it brings, each release's lines under its version, newest first,
// up to a cap and then a count of the rest.

const CHANGES = [
  {
    version: "0.6.4",
    lines: ["Members can edit shared venues", "New Low quality for laptops"],
  },
  {
    version: "0.6.3",
    lines: [
      "Right-click a score to rename it",
      "See what's analyzing in the status bar",
      "Clearer sync status",
      "Drag clips between lanes",
    ],
  },
  {
    version: "0.6.2",
    lines: ["Faster venue loading", "Search scores by name", "Export a show to video", "Undo a deleted clip"],
  },
];
const LINES = CHANGES.flatMap((change) => change.lines);

fixture({ track: false, seconds: 1, update_ready: { version: "0.6.4", changes: CHANGES } });

const CARD = { role: "card", label: "Update ready dialog" };
const PROMPT = { role: "text", label: "New version ready" };

test("the prompt lists changes under their versions, newest first, inside the card", () => {
  // The update lands while the launch's venue picker is up; the prompt
  // waits for it to go rather than being lost under it.
  nav.venue("Test Venue");
  const s = until("the update dialog", (s) => s.find(CARD) && s.find(PROMPT));
  const text = (label) => s.find({ role: "text", label });
  const shown = LINES.filter((line) => text(line));
  // The first lines in order make the cap; the rest are counted, not listed.
  expect(shown).toEqual(LINES.slice(0, shown.length));
  assert(shown.length < LINES.length, "every line fit, so the cap was not exercised");
  const more = text(`and ${LINES.length - shown.length} more`);
  assert(more, "no count of the lines left out");

  // Reading order: title, then each version above its own lines.
  const order = ["New version ready"];
  for (const change of CHANGES) {
    const lines = change.lines.filter((line) => shown.includes(line));
    if (lines.length === 0) {
      assert(!text(change.version), `${change.version} heads no lines`);
      continue;
    }
    order.push(change.version, ...lines);
  }
  order.push(more.label);

  const card = s.find(CARD).bounds;
  const later = s.find({ role: "button", label: "Later" }).bounds;
  assert(s.find({ role: "button", label: "Relaunch" }), "no Relaunch button");
  let above = card.y;
  for (const label of order) {
    const b = text(label).bounds;
    assert(b.y >= above, `${label} is not below the line before it`);
    assert(b.x >= card.x && b.x + b.width <= card.x + card.width, `${label} leaves the card`);
    above = b.y + b.height;
  }
  assert(above <= later.y, "the list runs into the buttons");
  assert(later.y + later.height <= card.y + card.height, "the buttons leave the card");
});

test("Later closes the prompt", () => {
  nav.venue("Test Venue");
  until("the update dialog", (s) => s.find(CARD));
  nav.step("the later button", "button", "Later");
  until("the dialog to close", (s) => s.find(CARD) === undefined);
});
