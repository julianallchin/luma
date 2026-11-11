// The composer's model picker, after Zeron's compact picker: a panel whose
// title is the reasoning level over the model's name, with the level's
// slider; a press on the name turns it to a page of every service's models —
// back, a tab per service, a search, one list. A pick turns back to the
// panel; Escape turns back from the models and closes the panel; a press
// outside closes it.

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
const panel = (s) => s.find({ role: "button", label: "Choose model" });
const slider = (s) => s.find({ role: "slider", label: "Reasoning effort" });
const level = (s) => s.find((n) => n.role === "text" && n.label.startsWith("Effort · "));
const back = (s) => s.find({ role: "button", label: "Back" });
// The search is named by its placeholder until it holds a query, which it is
// named by.
const search = (s, query) =>
  s.find((n) => n.role === "input" && n.label === (query ?? "Search models…"));
// A model row is a button in the card under the search, labelled by its name
// and then its service. (The composer's suggestions sit behind the card,
// wider than it.)
const rows = (s) => {
  const field = s.find((n) => n.role === "input" && n.label !== "Do anything…" && n.bounds.height > 0);
  if (!field) return [];
  const left = field.bounds.x - 1;
  const right = field.bounds.x + field.bounds.width + 1;
  return s.findAll((n) => n.role === "button" && n.bounds.height > 0
    && n.bounds.y > field.bounds.y + field.bounds.height - 1
    && n.bounds.x >= left && n.bounds.x + n.bounds.width <= right
    && n.label.includes(" · "));
};
// Every service's list has landed: each has a tab, or says why it has none.
// Lists land one by one, each moving the rows of the ones after it.
const SERVICES = ["Claude Code", "Codex", "OpenRouter", "Vercel AI Gateway"];
const allListed = (s) => SERVICES.every((service) =>
  s.find({ role: "button", label: service }) || s.find((n) => n.role === "text" && n.label.startsWith(`${service}: `)));
// A row is labelled "name · service"; the chip "service · name".
const name = (row) => row.label.split(" · ")[0];
const chosenName = (s) => chip(s).label.split(" · ")[1];

// The card's height is tweened as it turns between pages, so what is in it
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
  until("the panel", (s) => panel(s));
};

test("the chip opens a panel titled by the model's level, and Escape closes it", () => {
  openChat();
  open();
  // A model with levels runs at one of them: the panel's title is that level,
  // and the chip says it too.
  const s = until("the level", (s) => (level(s) && slider(s) ? s : undefined));
  const title = level(s).label.replace("Effort · ", "");
  // The panel: no search, no back.
  expect(search(s)).toBe(undefined);
  expect(back(s)).toBe(undefined);
  assert(title.length > 0, "the panel has no level");
  app.key("escape");
  until("the card closed", (s) => !panel(s));
});

// A service's default is its first row, under the model's own name: no row
// stands in for "whatever the service picks", and the chip names a model the
// list holds.
test("each service's first row names a model, and the chip names a listed one", () => {
  openChat();
  open();
  const chosen = chosenName(app.snapshot());
  app.click(panel(app.snapshot()));
  // The models open on the chosen one.
  until(`a row for ${chosen}`, (s) => rows(s).some((r) => name(r) === chosen));
  until("every service's list", allListed);
  const tabs = (s) => s.findAll((n) => n.role === "button" && SERVICES.includes(n.label));
  const services = still("the tabs", (s) => tabs(s).at(-1)) && tabs(app.snapshot()).map((t) => t.label);
  assert(services.length > 0, "no service listed its models");
  for (const service of services) {
    app.click(still(`the ${service} tab`, (s) => tabs(s).find((t) => t.label === service)));
    const top = (s) => {
      const first = rows(s)[0];
      return first && first.label.endsWith(` · ${service}`) ? first : undefined;
    };
    const first = top(until(`${service}'s models at the top`, top));
    assert(!/^default\b/i.test(name(first)), `${service}'s first row names no model: ${first.label}`);
  }
  app.key("escape");
  app.key("escape");
  until("closed", (s) => !panel(s));
});

test("the arrows on the panel step the level", () => {
  openChat();
  open();
  const at = (s) => level(s)?.label;
  // A step shows at once and saves behind it; the chip rests while it saves.
  const settled = (s) => chip(s)?.enabled && at(s);
  until("a level", settled);
  app.key("home");
  const lowest = at(until("the lowest level", settled));
  app.key("end");
  const highest = at(until("the highest level", (s) => settled(s) && at(s) !== lowest));
  app.key("home");
  until("back at the lowest", (s) => settled(s) && at(s) === lowest);
  app.key("right");
  until("a level above the lowest", (s) => settled(s) && at(s) !== lowest);
  assert(highest !== lowest, "the ends of the slider are one level");
  app.key("escape");
  until("closed", (s) => !panel(s));
});

test("the model's name turns the card to the models, and back returns", () => {
  openChat();
  open();
  app.click(panel(app.snapshot()));
  const s = until("the models page", (s) => (search(s) && back(s) ? s : undefined));
  // The models page leaves the panel behind: no slider, no level.
  expect(panel(s)).toBe(undefined);
  expect(slider(s)).toBe(undefined);
  // Every service whose list landed has a tab; one jumps the list to its
  // models.
  until("every service's list", allListed);
  const tabs = (s) => s.findAll((n) => n.role === "button" && SERVICES.includes(n.label));
  const last = still("the last tab", (s) => tabs(s).at(-1));
  assert(last, "no service tab");
  app.click(last);
  until(`${last.label}'s models at the top`, (s) => {
    const top = rows(s)[0];
    return top && top.label.endsWith(` · ${last.label}`);
  });
  // Back turns to the panel; Escape there closes it.
  app.click(back(app.snapshot()));
  until("the panel again", (s) => panel(s) !== undefined && search(s) === undefined);
  app.key("escape");
  until("closed", (s) => !panel(s));
});

test("typing on the panel opens the models searching, and Enter picks", () => {
  openChat();
  open();
  const before = chosenName(app.snapshot());
  // Enter opens the models on the chosen one: learn another model's name.
  app.key("enter");
  until("every service's list", allListed);
  const listed = settledRows("the models opened by Enter").filter((r) => name(r) !== before);
  assert(listed.length > 0, "no other model is listed");
  const target = name(listed[0]);
  app.key("escape");
  until("the panel again", (s) => panel(s) !== undefined);

  // A word of its name, typed on the panel, opens the models searching.
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
  // The pick turns back to the panel and the chip names it.
  until("the panel on the pick", (s) => panel(s) !== undefined && search(s) === undefined);
  until("the chip naming the pick", (s) => chosenName(s) === target);
  app.key("escape");
  until("closed", (s) => !panel(s));
});

test("a search that finds nothing says so, and a click picks a model", () => {
  openChat();
  open();
  app.click(panel(app.snapshot()));
  app.type(still("the search", (s) => search(s)), "zzqx nothing");
  until("nothing found", (s) => s.find({ role: "text", label: "No models found" }));
  app.key("secondary-a");
  app.key("backspace");
  const before = chosenName(app.snapshot());
  const pick = settledRows("the list again", (r) => r.some((row) => name(row) !== before))
    .find((r) => name(r) !== before);
  app.click(pick);
  until("the panel on the pick", (s) => panel(s) !== undefined);
  until("the chip naming the pick", (s) => chosenName(s) === name(pick));
});

test("a press outside the card closes it", () => {
  openChat();
  open();
  app.click(panel(app.snapshot()));
  const box = still("the models page", (s) => search(s)).bounds;
  // A point on the chat panel just right of the card.
  app.drag({ x: box.x + box.width + 30, y: box.y }, { dx: 0, dy: 0 }, { steps: 1 });
  until("the card closed", (s) => !panel(s) && search(s) === undefined);
});

// The card is a view of its own, mounted beside the chat panel: scrolling its
// list and typing in its search redraw the card, never the whole panel.
test("scrolling and searching the card redraw only the card", () => {
  openChat();
  open();
  // Opening the panel may save the model's level (a model runs at one of its
  // own); let that land before counting.
  until("the level saved", (s) => level(s) && chip(s)?.enabled);
  app.click(panel(app.snapshot()));
  const list = settledRows("the models", (r) => r.length > 3);
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
  // The search has had the keys since the models opened. The first key
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
