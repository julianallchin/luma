// The status bar's right end says what the app is doing in the background, in
// one short phrase, and nothing when it is idle. Pressing it opens a compact
// list of all of it, grouped, with held-back steps marked quietly.
//
// The harness sets no Python up, runs no analysis and moves no media, so the
// busy test states its work (`background_work`). The paused test does not:
// its failure is a real row in the library, read by the real query.

fixture({});

const inBar = (s, node) => {
  const bar = s.find({ role: "card", label: "Status bar" })?.bounds;
  return bar !== undefined && node.bounds.y >= bar.y && node.bounds.y < bar.y + bar.height;
};
const barWords = (s) => s.findAll((n) => n.bounds.width > 0 && inBar(s, n)).map((n) => n.label);
const list = (s) => s.find({ role: "card", label: "Background work" });
// The list's rows, top to bottom.
const rows = (s) => {
  const card = list(s).bounds;
  return s
    .findAll((n) => n.role === "row" && n.bounds.y >= card.y && n.bounds.y < card.y + card.height
      && n.bounds.x >= card.x && n.bounds.x < card.x + card.width)
    .map((n) => n.label);
};

const job = (fields) => ({ steps: [], progress: null, trackId: null, track: null, ...fields });
const BUSY = {
  jobs: [
    job({
      kind: "analysis",
      what: "Separating stems",
      trackId: "track-aurora",
      track: "Aurora",
      steps: ["stems"],
      state: { state: "running" },
    }),
    job({ kind: "analysis", what: "Queued", trackId: "t2", track: "Borealis", state: { state: "queued" } }),
    job({ kind: "analysis", what: "Queued", trackId: "t3", track: "Cirrus", state: { state: "queued" } }),
    job({
      kind: "analysis",
      what: "Reading chords paused after 2 failures",
      trackId: "t4",
      track: "Dusk",
      steps: ["roots"],
      state: { state: "paused", reason: "No chord model" },
    }),
    job({ kind: "transfer", what: "Uploading audio", progress: [0, 2], state: { state: "running" } }),
  ],
};

test("an open song is not repeated in the bar, and idle says nothing", () => {
  nav.trackEditor("Test Venue", "Aurora");
  const s = until("the editor's status bar", (s) => s.find({ role: "card", label: "Status bar" }) && s);
  const words = barWords(s);
  assert(!words.some((w) => /Aurora|BPM|\d:\d\d/.test(w)), `the bar repeats the song: ${words}`);
  assert(!words.some((w) => /Analyzing|Setting up|paused|Uploading/.test(w)), `idle reads as busy: ${words}`);
  assert(list(s) === undefined, "a list is open while idle");
});

test("busy work reads as one phrase and opens a grouped list", { fixture: { background_work: BUSY } }, () => {
  nav.trackEditor("Test Venue", "Aurora");
  const busy = until("the summary", (s) => s.find({ role: "button", label: "Analyzing 3 tracks" }) && s);
  assert(inBar(busy, busy.find({ role: "button", label: "Analyzing 3 tracks" })), "the summary is not in the bar");

  app.click(busy.find({ role: "button", label: "Analyzing 3 tracks" }));
  const open = until("the list", (s) => list(s) && s);
  expect(rows(open)).toEqual([
    "Aurora: Separating stems",
    "2 more queued",
    "Reading chords paused after 2 failures: Dusk · No chord model",
    "Uploading audio: 1 of 2",
  ]);
  // The list stands above the bar it hangs from.
  const bar = open.find({ role: "card", label: "Status bar" }).bounds;
  const card = list(open).bounds;
  assert(card.y + card.height <= bar.y + 1, "the list covers the bar");

  app.click(open.find({ role: "button", label: "Analyzing 3 tracks" }));
  until("the list closed", (s) => !list(s));
});

test("a step held back after failures reads as paused", {
  fixture: {
    sql: [
      `INSERT INTO preprocessing_failures (track_id, preprocessor, version, attempts, last_error, next_retry_at)
       VALUES ('track-aurora', 'stems', 1, 2, 'Out of memory
Traceback (most recent call last): …', '2999-01-01T00:00:00Z')`,
    ],
  },
}, () => {
  nav.trackEditor("Test Venue", "Aurora");
  const s = until("the paused step", (s) => s.find({ role: "button", label: "1 analysis step paused" }) && s);
  app.click(s.find({ role: "button", label: "1 analysis step paused" }));
  const open = until("the list", (s) => list(s) && s);
  expect(rows(open)).toEqual([
    "Separating stems paused after 2 failures: Aurora · Out of memory",
  ]);
});
