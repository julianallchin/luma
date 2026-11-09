// The status bar's right end says what the app is computing in the background,
// in one short phrase, and nothing when it is idle. Pressing it opens a short
// list of all of it; a press outside the list or Escape closes it, as every
// popover in the app closes, and so does the sync item's list beside it.
//
// The harness sets no Python up and runs no analysis, so the busy test states
// its work (`background_work`). The paused test does not: its failure is a
// real row in the library, read by the real query.

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

/** Press the empty middle of the title bar, well clear of the lists. */
const pressOutside = (s) => {
  const title = s.find({ role: "card", label: "Title bar" }).bounds;
  app.drag({ x: title.x + title.width / 2, y: title.y + title.height / 2 }, { dx: 0, dy: 0 }, { steps: 1 });
};

const job = (fields) => ({ steps: [], trackId: null, track: null, ...fields });
const BUSY = {
  jobs: [
    job({ kind: "analysis", what: "Separating stems", trackId: "track-aurora", track: "Aurora", steps: ["stems"], state: { state: "running" } }),
    job({ kind: "analysis", what: "Queued", trackId: "t2", track: "Borealis", state: { state: "queued" } }),
    job({ kind: "analysis", what: "Queued", trackId: "t3", track: "Cirrus", state: { state: "queued" } }),
    job({ kind: "analysis", what: "Reading chords paused", trackId: "t4", track: "Dusk", steps: ["roots"], state: { state: "paused", reason: "No chord model" } }),
  ],
};
const SUMMARY = "Analyzing 3 tracks";

test("an open song is not repeated in the bar, and idle says nothing", () => {
  nav.trackEditor("Test Venue", "Aurora");
  const s = until("the editor's status bar", (s) => s.find({ role: "card", label: "Status bar" }) && s);
  const words = barWords(s);
  assert(!words.some((w) => /Aurora|BPM|\d:\d\d/.test(w)), `the bar repeats the song: ${words}`);
  assert(!words.some((w) => /Analyzing|Setting up|paused/.test(w)), `idle reads as busy: ${words}`);
  assert(list(s) === undefined, "a list is open while idle");
});

test("busy work reads as one phrase and opens a short list", { fixture: { background_work: BUSY } }, () => {
  nav.trackEditor("Test Venue", "Aurora");
  const busy = until("the summary", (s) => s.find({ role: "button", label: SUMMARY }) && s);
  assert(inBar(busy, busy.find({ role: "button", label: SUMMARY })), "the summary is not in the bar");

  app.click(busy.find({ role: "button", label: SUMMARY }));
  const open = until("the list", (s) => list(s) && s);
  expect(rows(open)).toEqual([
    "Separating stems · Aurora",
    "2 more queued",
    "Reading chords paused · Dusk",
  ]);
  // The list stands above the bar it hangs from.
  const bar = open.find({ role: "card", label: "Status bar" }).bounds;
  const card = list(open).bounds;
  assert(card.y + card.height <= bar.y + 1, "the list covers the bar");

  // Its own press closes it.
  app.click(open.find({ role: "button", label: SUMMARY }));
  until("closed by its press", (s) => !list(s));
});

test("the background list closes on a press outside it and on Escape", { fixture: { background_work: BUSY } }, () => {
  nav.trackEditor("Test Venue", "Aurora");
  const busy = until("the summary", (s) => s.find({ role: "button", label: SUMMARY }) && s);
  app.click(busy.find({ role: "button", label: SUMMARY }));
  pressOutside(until("the list", (s) => list(s) && s));
  until("closed by a press outside", (s) => !list(s));

  app.click(app.snapshot().find({ role: "button", label: SUMMARY }));
  until("open again", (s) => list(s));
  app.key("escape");
  until("closed by Escape", (s) => !list(s));
});

test("the sync item's list closes on a press outside it and on Escape", {
  fixture: {
    sync_status: {
      connected: true,
      uploading: false,
      downloading: false,
      lastSyncedAt: null,
      pendingUploads: 0,
      refused: [{ id: "r1", table: "tracks", rowId: "tracks-1", op: "PATCH", status: 204, at: "2026-11-08T12:00:00Z" }],
    },
  },
}, () => {
  nav.trackEditor("Test Venue", "Aurora");
  const refused = (s) => s.find({ role: "card", label: "Not saved" });
  const shown = until("not saved", (s) => s.find({ role: "button", label: "1 not saved" }) && s);
  app.click(shown.find({ role: "button", label: "1 not saved" }));
  pressOutside(until("the refused list", (s) => refused(s) && s));
  until("closed by a press outside", (s) => !refused(s));

  app.click(app.snapshot().find({ role: "button", label: "1 not saved" }));
  until("open again", (s) => refused(s));
  app.key("escape");
  until("closed by Escape", (s) => !refused(s));
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
  expect(rows(open)).toEqual(["Separating stems paused · Aurora"]);
});
