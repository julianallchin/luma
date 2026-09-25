// The unified chat, seen from the outside: a turn, its chrome, its history
// and its dialogs.
//
// Motion is on, so the send flight and the spring are exercised; the last
// test covers the reduced-motion path.
//
// Only the model is scripted; the turn under it is real — a real thread, real
// rows — because the panel reads what the loop persisted. The `python` tool
// answers slowly on purpose: every scripted event is ready at once, so without
// a slow tool there is no moment at which a turn is half-finished.

const OPENING = "Chasing the **downbeat**. I'll sample the ramp and check where it peaks.";
const CLOSING =
  "Here is the curve it settled on:\n\n```python\nfor beat in range(4):\n    ramp(beat, 0.5)\n```\n\n" +
  "- peak at bar 3\n- release over two bars\n";
const PROMPT = "where does the ramp peak?";

// Several deltas rather than one: a single-delta reply would never exercise
// the path the streaming look depends on.
const chunks = (text) => text.match(/[\s\S]{1,24}/g).map((chunk) => ({ text: chunk }));

// A warm shape — most of the prompt from the cache — because that is what
// every turn past the first looks like, and a gauge fed from input tokens
// alone gets it wrong.
const USAGE = [
  { inputTokens: 12400, outputTokens: 96, cacheReadInputTokens: 300000, cacheCreationInputTokens: 12400 },
  { inputTokens: 4800, outputTokens: 512, cacheReadInputTokens: 610000, cacheCreationInputTokens: 12000 },
];

fixture({
  window: [1400, 900],
  motion: true,
  model_cadence_ms: 25,
  model: [
    [
      ...chunks(OPENING),
      {
        call: "python",
        id: "call-1",
        args: { verb: "Checking", verbPast: "Checked", purpose: "ramp peak", code: "ramp.peak()" },
      },
      { end: "tool_use", usage: USAGE[0] },
    ],
    [...chunks(CLOSING), { end: "end_turn", usage: USAGE[1] }],
  ],
  tools: [
    {
      name: "python",
      latency_ms: 1200,
      schema: {
        type: "object",
        required: ["verb", "verbPast", "purpose", "code"],
        properties: {
          verb: { type: "string" },
          verbPast: { type: "string" },
          purpose: { type: "string" },
          code: { type: "string" },
        },
      },
      result: {
        status: "ok",
        stdout: "peak=3.0\nrelease=2 bars\n",
        stderr: "",
        repr: "<Ramp peak=3.0 release=2.0>",
        traceback: null,
        notices: [],
        figures: [],
        durationMs: 247,
      },
    },
  ],
});

const texts = (s) => s.findAll({ role: "text" }).map((n) => n.label);
const chips = (s) => s.findAll({ role: "chip" }).map((n) => n.label);
const busy = (s) => texts(s).some((label) => label === "Working" || label === "Sending");
const composer = (s) => s.find({ role: "input", label: "Do anything…" });
const button = (label) => app.snapshot().find({ role: "button", label });

// Open the track and start a new conversation. Waiting on a pressable Send is
// the honest wait: an unattached centre has none.
const openChat = () => {
  nav.trackEditor("Test Venue", "Aurora");
  nav.step("new conversation", "button", "New chat");
  until("the conversation", (s) => s.find({ role: "button", label: "New chat" })?.enabled);
  until("the chat centre", (s) => {
    const send = s.find({ role: "button", label: "Send" });
    return send !== undefined && send.bounds.width > 0;
  });
};

// Pressing Send only starts a turn, so wait for it to begin: "no Working
// trailer" is equally true before the turn starts, and a caller waiting for
// the end could otherwise be answered by the pre-turn frame.
const send = () => {
  app.type(composer(app.snapshot()), PROMPT);
  app.click(button("Send"));
  until("the turn to begin", busy);
};

const turnEnd = () => until("the turn end", (s) => !busy(s));

test("a turn streams markdown and shows its tool call", () => {
  openChat();
  const idle = app.snapshot();
  expect(texts(idle)).toContain("Luma");
  expect(chips(idle)).toEqual([]);

  send();
  // The scripted tool holds the turn open.
  const streaming = until("the tool call start", (s) => chips(s).some((c) => c.startsWith("Checking")));
  assert(texts(streaming).some((l) => l.includes("Chasing the downbeat")), "the streamed prose is not painted");
  expect(texts(streaming)).toContain("Working");
  expect(chips(streaming)).toEqual(["Checking ramp peak"]);

  const settled = until("the turn end", (s) => chips(s).includes("Checked ramp peak") && !busy(s));
  assert(texts(settled).some((l) => l.includes("release over two bars")), "the second step never landed");
  assert(texts(settled).some((l) => l.includes("ramp(beat, 0.5)")), "the code block is not painted");
  expect(chips(settled)).toEqual(["Checked ramp peak"]);
});

// Two samples inside the turn that differ are the only proof this is
// streaming and not one final paint. `send` returns once the turn has begun,
// so `first` is not the idle frame.
test("the transcript grows between frames", () => {
  openChat();
  send();
  const prose = (s) => texts(s).join("\n").length;
  const first = prose(app.snapshot());
  const last = prose(turnEnd());
  expect(last).toBeGreaterThan(first);
});

// With no score open the chat is there, unattached, and says what it could
// attach to; moving to another view about nothing keeps it.
test("the chat opens unattached on a screen with no subject", () => {
  const welcome = until("the unattached centre", (s) =>
    texts(s).includes("Luma") && texts(s).includes("Pick a track to chat"));
  expect(welcome.find({ role: "button", label: "Send" })).toBe(undefined);
  expect(composer(welcome)).toBe(undefined);

  app.action("luma::OpenSettings");
  const settings = until("the settings dialog", (s) => s.find({ role: "card", label: "Settings dialog" }));
  expect(texts(settings)).toContain("Luma");
});

test("a new chat has one identity across editors", () => {
  nav.trackEditor("Test Venue", "Aurora");
  nav.step("new conversation", "button", "New chat");
  const attached = until("the conversation", (s) => {
    const send = s.find({ role: "button", label: "Send" });
    return send && send.bounds.width > 0 && s.find({ role: "button", label: "New chat" })?.enabled;
  });
  expect(texts(attached)).toContain("Luma");
  const typed = texts(attached).filter((label) => label.endsWith(" agent"));
  expect(typed).toEqual([]);
});

// The composer declares TextInput, so a space is a space and not the
// transport's play/pause.
test("space typed into the composer is a space", () => {
  openChat();
  app.type(composer(app.snapshot()), "a b");
  const inputs = app.snapshot().findAll({ role: "input" }).map((n) => n.label);
  expect(inputs).toContain("a b");
});

// "New chat" always creates: routed through the ordinary resolve it would hand
// back the conversation already on screen.
test("the chat chrome offers history and starts a new conversation", () => {
  openChat();
  const chrome = app.snapshot().findAll({ role: "button" }).map((n) => n.label);
  expect(chrome).toContain("Chat history");
  expect(chrome).toContain("New chat");

  send();
  const reply = (l) => l.includes("release over two bars");
  assert(texts(turnEnd()).some(reply), "the turn never landed");

  app.click(button("New chat"));
  const fresh = texts(until("the fresh conversation", (s) =>
    texts(s).some((l) => l.includes("Where do you want to start?"))));
  assert(!fresh.some(reply), "the previous conversation is still on screen");
});

// The pick is asserted by content, not by thread id: `resolve_thread` is
// newest-wins per subject, so a picker routed through it would open the
// newest conversation while the id looked right.
test("the history picker reopens the conversation that was picked", () => {
  openChat();
  // Only the dialog's own rows: `card` is a shared role, and the screen
  // behind the dialog is still in the tree. The dialog paints last, so its
  // rows follow it in the frame.
  const rows = () => {
    const nodes = app.snapshot().nodes;
    const start = nodes.findIndex((n) => n.label === "Chat history dialog");
    if (start < 0) return [];
    return nodes.slice(start).filter((n) =>
      n.role === "card" && n.label !== "Chat history dialog" && n.bounds.height > 0);
  };

  send();
  turnEnd();
  app.click(button("New chat"));
  const second = texts(until("the fresh conversation", (s) =>
    texts(s).some((l) => l.includes("Where do you want to start?"))));
  assert(!second.some((l) => l.includes(PROMPT)), "the new chat still shows the old one");

  app.click(button("Chat history"));
  until("the chat list", () => rows().length >= 2);
  // Rows are named by what was asked in them, an unspoken one by the
  // placeholder.
  const listed = rows().map((n) => n.label);
  expect(listed).toContain(PROMPT);
  expect(listed).toContain("New chat");

  // Typing greps the transcripts: the reply's line is the one hit.
  const search = (label) => app.snapshot().find({ role: "input", label });
  app.type(search("Search chats…"), "downbeat");
  until("the grep hits", () => rows().length >= 1
    && rows().every((n) => n.label.toLowerCase().includes("downbeat")));
  const hits = rows().map((n) => n.label);
  expect(hits.length).toBe(1);
  expect(hits[0]).toContain("Chasing the **downbeat**");
  app.type(search("downbeat"), " nothing-says-this");
  until("no matches", (s) => s.find({ role: "text", label: "No matches" }) !== undefined);
  app.key("secondary-a");
  app.key("backspace");
  until("the list again", () => rows().length >= 2);

  // Pick the older conversation by its name.
  app.click(rows().find((n) => n.label === PROMPT));
  const reopened = texts(until("the reopened conversation", (s) =>
    texts(s).some((l) => l.includes(PROMPT))));
  assert(reopened.some((l) => l.includes("Chasing the downbeat")), "the reopened conversation lost its reply");
});

test("a model choice is kept per conversation and carried into new chats", () => {
  openChat();
  const select = (s) => s.findAll({ role: "select" })[0];
  const original = until("the engine picker", select).findAll({ role: "select" })[0].label;
  app.click(select(app.snapshot()));
  nav.step("the model menu", "button", "Choose model");
  // Any model but the current one. A row is labelled by its name first.
  const current = original.split(" · ").at(-1);
  const other = (s) => {
    const search = s.find({ role: "input", label: "Search models…" });
    return search && s.findAll({ role: "button" }).find((n) =>
      n.bounds.y > search.bounds.y && n.bounds.height > 0 && n.label.includes(" · ")
        && n.label.split(" · ")[0] !== current);
  };
  // The menu slides in; a row is clicked once it has stopped moving.
  let last;
  const still = (s) => {
    const node = other(s);
    const settled = node && last && node.label === last.label && node.bounds.y === last.bounds.y;
    last = node;
    return settled;
  };
  const pick = other(until("another model in the list, at rest", still));
  const name = pick.label.split(" · ")[0];
  app.click(pick);
  const CHOSEN = until(`the saved engine ${pick.label}`, (s) => select(s)?.label.endsWith(` · ${name}`))
    .findAll({ role: "select" })[0].label;

  const slider = until("the effort slider", (s) => s.find({ role: "slider", label: "Reasoning effort" }))
    .find({ role: "slider", label: "Reasoning effort" });
  app.drag(slider, { dx: 170, dy: 0 }, { steps: 8 });
  until("dragged high effort", (s) => s.find({ role: "text", label: "Effort · High" }));
  app.key("home");
  until("auto effort", (s) => s.find({ role: "text", label: "Effort · Auto" }));
  app.key("right");
  until("the next effort", (s) => s.find({ role: "text", label: "Effort · Low" }));
  until("effort persisted", (s) => s.find({ role: "slider", label: "Reasoning effort" })?.enabled);
  app.key("escape");
  until("picker dismissed", (s) => !s.find({ role: "slider", label: "Reasoning effort" }));

  // A new chat starts from the last choice.
  app.click(button("New chat"));
  until("the new chat's selection", (s) => s.find({ role: "select", label: CHOSEN }));

  // Each conversation reads its own choice back when reopened.
  const rows = () => app.snapshot().findAll({ role: "card", label: "New chat" });
  app.click(button("Chat history"));
  until("both conversations", () => rows().length >= 2);
  const count = rows().length;
  for (let index = 0; index < count; index++) {
    if (index > 0) {
      app.click(button("Chat history"));
      until("both conversations again", () => rows().length >= 2);
    }
    app.click(rows()[index]);
    until("the reopened picker", (s) => s.find({ role: "select", label: CHOSEN }));
    nav.step("the reopened picker", "select", CHOSEN);
    until("the reopened effort", (s) => s.find({ role: "text", label: "Effort · Low" }));
    app.key("escape");
    until("picker dismissed", (s) => !s.find({ role: "slider", label: "Reasoning effort" }));
  }
});

// The reading is the whole prompt, cache included, against the model's
// window; the card names every field the provider reported.
test("the context gauge reports the whole prompt and its card names every field", () => {
  openChat();
  const gauges = (s) => s.findAll((n) => n.label.startsWith("Context "));
  // No request yet: no gauge at all. An empty ring would claim zero.
  expect(gauges(app.snapshot()).length).toBe(0);

  send();
  const gauge = gauges(turnEnd()).find((n) => n.role === "button");
  assert(gauge, "no gauge after the turn");

  // Hovering alone leaves the card closed.
  app.scroll(gauge, { dx: 0, dy: 0 });
  const model = (s) => texts(s).some((l) => l.startsWith("Model "));
  expect(model(app.snapshot())).toBe(false);
  app.click(button(gauge.label));
  const rows = texts(until("the usage card", model));
  for (const row of ["Input 4,800", "Cache read 610,000", "Cache write 12,000", "Output 512", "Prompt 626,800"]) {
    expect(rows).toContain(row);
  }
  const windowSize = Number(rows.find((l) => l.startsWith("Window ")).slice("Window ".length).replaceAll(",", ""));
  const reading = Number(gauge.label.match(/^Context (\d+)%$/)[1]);
  expect(Math.abs(reading - (626800 / windowSize) * 100)).toBeLessThan(1);
  // Measured, not scripted: only its shape is assertable.
  assert(rows.some((l) => /^Took .+ (ms|s)$/.test(l)), "the card does not say how long the request took");
});

// The second press closes the card, and its exit finishes: the card leaves the
// frame instead of staying at the end of its fade.
test("clicking the context gauge again closes its card", () => {
  openChat();
  send();
  const gauge = turnEnd().find((n) => n.role === "button" && n.label.startsWith("Context "));
  const model = (s) => texts(s).some((l) => l.startsWith("Model "));
  app.click(gauge);
  until("the usage card", model);
  app.click(button(gauge.label));
  until("the usage card to close", (s) => !model(s));
});

// Reached from the chat, whose composer holds the keyboard when the dialog
// opens: the dismissal has to out-rank the field.
test("escape closes the history picker the thread opened", () => {
  openChat();
  app.click(composer(app.snapshot()));
  expect(composer(app.snapshot()).focused).toBe(true);
  app.click(button("Chat history"));
  const up = until("the history picker", (s) => s.find({ role: "card", label: "Chat history dialog" }));
  expect(up.find({ role: "card", label: "Chat history dialog" }).focused).toBe(true);
  app.key("escape");
  until("the picker to close", (s) => !s.find({ role: "card", label: "Chat history dialog" }));
});

test("under reduced motion a sent turn is painted as it lands",
  { fixture: { motion: false } }, () => {
  openChat();
  send();
  const settled = turnEnd();
  assert(texts(settled).some((l) => l.includes("release over two bars")), "the reply is not on screen");
});
