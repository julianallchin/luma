// The Perform tab's chat works on the venue's Perform set.
//
// The venue has one pattern, "Front wash", in the first cell of its one
// page; its graph's `pad` button, on its alpha, is its pad. The scripted agent answers from the Perform tab with one Python call
// that stores a second pattern, "Warm wash", in the empty cell beside it: the
// two writes a cell's `luma.perform.add` makes, through the same commands.
// The tab reads the set again when the turn says it changed. Perform chats
// are a workspace of their own, apart from the venue's.

const FRONT = "8c1d2b8e-0000-4000-8000-000000000001";
const WARM = "8c1d2b8e-0000-4000-8000-000000000002";
const PROMPT = "Put a warm wash next to the front wash";
const graph = {
  version: 3,
  nodes: {
    fader: { kind: "control", settings: { name: "drop" } },
    curve1: { kind: "curve", inputs: { x: { node: "fader" } } },
    pad: { kind: "button", settings: { name: "pad", mode: "toggle" } },
    curve2: { kind: "curve", inputs: { x: { node: "pad" } } },
    color1: {
      kind: "color",
      inputs: { brightness: { node: "curve1" }, alpha: { node: "curve2" } },
    },
  },
};
const cell = (id, column, content) => ({ id, column, row: 0, width: 1, height: 1, content });
const page = (cells) => ({
  pages: [{ id: "page-main", name: "Main", columns: 8, rows: 4, oneAtATime: [], cells }],
});
const seeded = [
  cell("cell-front", 0, { kind: "pattern", patternId: FRONT }),
  cell("cell-tap", 7, { kind: "tap" }),
];

fixture({
  seconds: 1,
  rig: 2,
  window: [1400, 1000],
  sql: [
    `INSERT INTO perform_patterns (id, uid, venue_id, name, selection_json, z_index,
                                   blend_mode, seed, graph_json)
       VALUES ('${FRONT}', '$PRINCIPAL', 'venue-main', 'Front wash',
               '{"expression":"all"}', 0, 'replace', '7', '${JSON.stringify(graph)}')`,
    `INSERT INTO perform_layouts (id, uid, venue_id, layout_json)
       VALUES ('venue-main', '$PRINCIPAL', 'venue-main', '${JSON.stringify(page(seeded))}')`,
  ],
  model: [
    [
      { text: "Warming the edge of the stage." },
      {
        call: "python",
        id: "call-1",
        args: {
          verb: "Adding",
          verbPast: "Added",
          purpose: "a warm wash",
          code: 'luma.perform.add(preset("Wash"), name="Warm wash", at=(1, 0))',
        },
      },
    ],
    [{ text: "A warm wash now sits beside the front wash." }],
  ],
  tools: [
    {
      name: "python",
      latency_ms: 200,
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
      dispatch: [
        {
          command: "save_perform_pattern",
          args: {
            venueId: "venue-main",
            pattern: {
              id: WARM, venueId: "venue-main", name: "Warm wash",
              selection: { expression: "all" }, zIndex: 1, blendMode: "replace",
              seed: 9, graph,
            },
          },
        },
        {
          command: "save_perform_layout",
          args: {
            venueId: "venue-main",
            layout: page([...seeded, cell("cell-warm", 1, { kind: "pattern", patternId: WARM })]),
          },
        },
      ],
      result: {
        status: "ok", stdout: "", stderr: "", repr: "<Pattern 'Warm wash'>",
        traceback: null, notices: [], figures: [], durationMs: 180,
      },
    },
  ],
});

const pad = (s, name) => s.find((n) => n.role === "button" && n.label.startsWith(`Pad: ${name}`));
const texts = (s) => s.findAll({ role: "text" }).map((n) => n.label);
const busy = (s) => texts(s).some((label) => label === "Working" || label === "Sending");
const composer = (s) => s.find({ role: "input", label: "Do anything…" });
const asked = (s) => s.findAll({ role: "card" }).some((n) => n.label.startsWith(PROMPT));

function openPerform() {
  nav.venue("Test Venue");
  nav.live();
  until("the pad", (s) => pad(s, "Front wash"));
  nav.step("new conversation", "button", "New chat");
  until("the chat centre", (s) => {
    const send = s.find({ role: "button", label: "Send" });
    return send !== undefined && send.bounds.width > 0;
  });
}

function ask() {
  app.type(composer(app.snapshot()), PROMPT);
  app.click(app.snapshot().find({ role: "button", label: "Send" }));
  until("the turn to begin", busy);
  until("the turn end", (s) => !busy(s) && texts(s).some((l) => l.includes("beside the front wash")));
}

// The front body's chats, as its history lists them once they have loaded.
function history() {
  nav.step("history", "button", "Chat history");
  const listed = until("the history", (s) =>
    s.find({ role: "input", label: "Search chats…" }) && !s.find({ role: "text", label: "Loading chats…" }));
  app.key("escape");
  until("history dismissed", (s) => !s.find({ role: "input", label: "Search chats…" }));
  return listed;
}

test("the agent's new pattern appears in its cell on the grid", () => {
  openPerform();
  expect(pad(app.snapshot(), "Warm wash")).toBe(undefined);
  ask();
  const shot = until("the new pattern's pad", (s) => (pad(s, "Warm wash") ? s : undefined));
  // In the place beside the front wash: right of it, on the same row.
  const [front, warm] = [pad(shot, "Front wash").bounds, pad(shot, "Warm wash").bounds];
  expect(warm.x).toBeGreaterThan(front.x + front.width - 1);
  expect(Math.abs(warm.y - front.y)).toBeLessThan(2);
});

test("Live chats are listed in Live and not in Rig", () => {
  openPerform();
  ask();
  expect(asked(history())).toBe(true);
  nav.venuePage("Test Venue");
  until("the venue chat", (s) => s.find({ role: "button", label: "Chat history" }));
  expect(asked(history())).toBe(false);
});
