// The composer's model picker: a card folded on the chosen model over its
// reasoning, opened out in place to a tab per service, a search scoped to the
// tab, and the tab's models. A pick folds it back; Escape folds an opened-out
// card and closes a folded one; a press outside closes it.

fixture({ window: [1400, 900], motion: true });

const openChat = () => {
  nav.trackEditor("Test Venue", "Aurora");
  nav.step("new conversation", "button", "New chat");
  until("the conversation", (s) => s.find({ role: "button", label: "New chat" })?.enabled);
  until("the chat centre", (s) => {
    const send = s.find({ role: "button", label: "Send" });
    return send !== undefined && send.bounds.width > 0;
  });
};

const chip = (s) => s.findAll({ role: "select" })[0];
const folded = (s) => s.find({ role: "button", label: "Choose model" });
const slider = (s) => s.find({ role: "slider", label: "Reasoning effort" });
const OWN_EFFORT = "This model manages its own effort";
const reasoning = (s) => slider(s) ?? s.find({ role: "text", label: OWN_EFFORT });
// The search is named by its placeholder, which names the tab, until it
// holds a query, which it is named by.
const search = (s, query) =>
  s.find((n) => n.role === "input" && (query ? n.label === query : /^Search .+ models…$/.test(n.label)));
// A model row is a button in the card between the search and the reasoning,
// labelled by its name first. (The composer's suggestions sit behind the
// card, wider than it.)
const rows = (s) => {
  const field = s.find((n) => n.role === "input" && n.label !== "Do anything…" && n.bounds.height > 0);
  const foot = reasoning(s);
  if (!field || !foot) return [];
  const left = field.bounds.x - 1;
  const right = field.bounds.x + field.bounds.width + 1;
  return s.findAll((n) => n.role === "button" && n.bounds.height > 0
    && n.bounds.y > field.bounds.y && n.bounds.y < foot.bounds.y
    && n.bounds.x >= left && n.bounds.x + n.bounds.width <= right && n.label !== "Retry models");
};
const name = (row) => row.label.split(" · ")[0];
const chosenName = (s) => chip(s).label.split(" · ").at(-1);

// The card's height is tweened as it opens out and folds, so what is in it
// moves; act on a node once it holds still on screen.
const still = (what, find) => {
  let last;
  const s = until(what, (s) => {
    const node = find(s);
    const key = node && node.bounds.width && node.bounds.height ? JSON.stringify(node.bounds) : undefined;
    const settled = key !== undefined && key === last;
    last = key;
    return settled ? s : undefined;
  });
  return find(s);
};
// The model rows, once they hold still and `pred` holds of them.
const settledRows = (what, pred = (r) => r.length > 0) => {
  let last;
  const s = until(what, (s) => {
    const now = rows(s);
    const key = JSON.stringify(now.map((r) => [r.label, r.bounds.y]));
    const settled = pred(now) && key === last;
    last = key;
    return settled ? s : undefined;
  });
  return rows(s);
};

const open = () => {
  app.click(chip(until("the model chip", chip)));
  until("the folded card", (s) => folded(s) && reasoning(s));
};

test("the chip opens a card folded on the chosen model, and Escape closes it", () => {
  openChat();
  open();
  const s = app.snapshot();
  // Folded: the chosen model and its reasoning, no tabs and no search.
  expect(search(s)).toBe(undefined);
  expect(s.find({ role: "button", label: "Codex" })).toBe(undefined);
  app.key("escape");
  until("the card closed", (s) => !folded(s));
});

test("opening out shows a tab per service and a search scoped to the tab", () => {
  openChat();
  open();
  app.click(folded(app.snapshot()));
  const s = until("the opened-out card", (s) => (search(s) !== undefined ? s : undefined));
  for (const service of ["Claude Code", "Codex", "OpenRouter", "Vercel AI Gateway"]) {
    assert(s.find({ role: "button", label: service }), `no ${service} tab`);
  }
  expect(folded(s)).toBe(undefined);
  assert(reasoning(s), "the reasoning left the card");
  // Another tab: the search names it.
  app.click(still("the Codex tab", (s) => s.find({ role: "button", label: "Codex" })));
  until("the Codex search", (s) => s.find({ role: "input", label: "Search Codex models…" }));
  // Escape folds it back onto the chosen model; Escape again closes it.
  app.key("escape");
  until("folded again", (s) => folded(s) !== undefined && search(s) === undefined);
  app.key("escape");
  until("closed", (s) => !folded(s));
});

test("typing on the folded card opens it out filtering, and Enter picks", () => {
  openChat();
  open();
  const before = chosenName(app.snapshot());
  // The arrow opens the chosen model's tab out: learn another model's name.
  app.key("down");
  const listed = settledRows("the list opened by the arrow").filter((r) => name(r) !== before);
  assert(listed.length > 0, "the chosen model's tab lists no other model");
  const target = name(listed[0]);
  app.key("escape");
  until("folded again", (s) => folded(s) !== undefined);

  // A word of its name, typed on the folded card, opens it out searching.
  const word = (target.split(/\s+/).find((w) => /^[a-z]{2,}$/i.test(w)) ?? target).toLowerCase();
  app.key(word.split("").join(" "));
  until("the search holding the word", (s) => search(s, word));
  const filtered = settledRows("the filtered list");
  assert(filtered.every((r) => r.label.toLowerCase().includes(word)),
    `a row that does not match "${word}": ${filtered.map((r) => r.label).join(", ")}`);
  const at = filtered.findIndex((r) => name(r) === target);
  assert(at >= 0, `${target} did not survive its own word`);
  // The cursor starts on the first match; walk down to the target.
  if (at > 0) app.key(Array(at).fill("down").join(" "));
  app.key("enter");
  // The pick folds the card back and the chip names it.
  until("folded on the pick", (s) => folded(s) !== undefined && search(s) === undefined);
  until("the chip naming the pick", (s) => chosenName(s) === target);
  app.key("escape");
  until("closed", (s) => !folded(s));
});

test("a search that finds nothing says so, and a click picks a model", () => {
  openChat();
  open();
  app.click(folded(app.snapshot()));
  app.type(still("the search", (s) => search(s)), "zzqx nothing");
  until("nothing found", (s) => s.find({ role: "text", label: "No models found" }));
  app.key("secondary-a");
  app.key("backspace");
  const before = chosenName(app.snapshot());
  const pick = settledRows("the list again", (r) => r.some((row) => name(row) !== before))
    .find((r) => name(r) !== before);
  app.click(pick);
  until("folded on the pick", (s) => folded(s) !== undefined);
  until("the chip naming the pick", (s) => chosenName(s) === name(pick));
});

test("a press outside the card closes it", () => {
  openChat();
  open();
  app.click(folded(app.snapshot()));
  const box = still("the opened-out card", (s) => search(s)).bounds;
  // A point on the chat panel just right of the card.
  app.drag({ x: box.x + box.width + 30, y: box.y }, { dx: 0, dy: 0 }, { steps: 1 });
  until("the card closed", (s) => !folded(s) && search(s) === undefined);
});

// The card is a view of its own, mounted beside the chat panel: scrolling its
// list and typing in its search redraw the card, never the whole panel.
test("scrolling and searching the card redraw only the card", () => {
  openChat();
  open();
  app.click(folded(app.snapshot()));
  const list = settledRows("the opened-out list", (r) => r.length > 3);
  const count = (label) => {
    const node = app.snapshot().find((n) => n.role === "text" && n.label.startsWith(`${label} renders = `));
    return Number(node.label.split(" = ")[1]);
  };
  const chat = count("Chat");
  let picker = count("Picker");
  const over = { x: list[1].bounds.x + 20, y: list[1].bounds.y + 10 };
  app.scroll(over, { dy: -120, steps: 4 });
  app.scroll(over, { dy: 120, steps: 4 });
  expect(count("Picker")).toBeGreaterThan(picker);
  expect(count("Chat")).toBe(chat);
  // The search has had the keys since the card opened out. The first key
  // switches gpui from pointer to keyboard input, which redraws the whole
  // window once; what is measured is the typing after it.
  app.key("g");
  until("the first letter", (s) => search(s, "g"));
  const settledChat = count("Chat");
  picker = count("Picker");
  app.key("p t");
  until("the search holding the query", (s) => search(s, "gpt"));
  expect(count("Picker")).toBeGreaterThan(picker);
  expect(count("Chat")).toBe(settledChat);
});
