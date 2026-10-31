// The source browser's covers and its audio preview: the cover is the play
// button, one row plays at a time, the scrub line seeks, and a preview never
// outlives the dialog that started it.

// Two source rows on one real 40 s file (the fixture's second track), one with
// a library thumbnail and one with nothing to show. Aurora, the venue's own
// song, is 20 s — so which file the player holds is readable off a duration.
const row = (uuid, title, art) => ({
  uuid,
  filePath: "$LIBRARY/zulu.wav",
  filename: "zulu.wav",
  title,
  artist: "Preview Artist",
  album: null,
  bpm: 124.0,
  durationSeconds: 40.0,
  artworkPath: art ? "$LIBRARY/padding-art.png" : null,
});
const ROWS = [row("alpha", "Alpha", true), row("beta", "Beta", false)];

fixture({
  seconds: 20,
  equal_timestamp_track: true,
  second_track_seconds: 40,
  // Writes `padding-art.png`; with no extra tracks it is only the cover.
  album_art: 0,
  source_fixture: {
    library: { trackCount: 2 },
    playlists: [],
    tracks: ROWS,
    // A query with a space in it answers with a row of its own, so a space
    // that was typed is visible.
    searches: { a: [ROWS[0]], "a ": [row("spaced", "Spaced", false)] },
  },
});

const button = (s, label) => s.find({ role: "button", label });
// The node `find` names, once it exists. `until` answers with the snapshot.
const node = (what, find) => find(until(what, find));
const slider = (s) => s.findAll({ role: "slider" }).filter((n) => n.label.startsWith("Preview position:"));
// "Preview position: 0:14 of 0:40" -> 14.
const elapsed = (s) => {
  const [m, sec] = slider(s)[0].label.match(/: (\d+):(\d+) of/).slice(1).map(Number);
  return m * 60 + sec;
};

function openSource() {
  nav.venue("Test Venue");
  nav.step("the add-track affordance", "button", "Add track");
  app.click(node("the browser", (s) => button(s, "Import tracks")));
  app.click(node("the source menu", (s) => s.find({ role: "row", label: "Rekordbox" })));
  return until("the source rows, search focused", (s) =>
    s.find({ role: "row", label: "Beta" }) &&
    s.find({ role: "input", label: "Search source…" })?.focused ? s : undefined);
}

test("a row's cover comes from its library, and a row without one keeps the plate", () => {
  openSource();
  until("Alpha's thumbnail", (s) => s.find({ role: "text", label: "Cover of Alpha" }));
  // Beta's file has no tags and its library no thumbnail: it is looked for
  // once, found missing, and stays the placeholder.
  app.frames(10, { waitMs: 20 });
  expect(app.snapshot().find({ role: "text", label: "Cover of Beta" })).toBe(undefined);
});

test("the cover plays and pauses, and one row plays at a time", () => {
  openSource();
  app.click(node("Alpha's play button", (s) => button(s, "Play preview of Alpha")));
  const playing = until("Alpha previewing", (s) =>
    button(s, "Pause preview of Alpha") && slider(s).length === 1 ? s : undefined);
  // Into the body of the song, not the intro: about a third of 40 s.
  expect(elapsed(playing)).toBeGreaterThan(10);
  expect(elapsed(playing)).toBeLessThan(20);

  app.click(button(playing, "Play preview of Beta"));
  const switched = until("Beta previewing instead", (s) =>
    button(s, "Pause preview of Beta") && button(s, "Play preview of Alpha") ? s : undefined);
  expect(slider(switched).length).toBe(1);

  app.click(button(switched, "Pause preview of Beta"));
  const paused = until("Beta paused", (s) => button(s, "Play preview of Beta") && slider(s).length === 1 ? s : undefined);
  const at = elapsed(paused);
  app.frames(8, { waitMs: 150 });
  expect(elapsed(app.snapshot())).toBe(at);
});

test("the scrub line and shift-arrows seek", () => {
  openSource();
  app.click(node("Alpha's play button", (s) => button(s, "Play preview of Alpha")));
  const start = elapsed(until("Alpha previewing", (s) => slider(s).length === 1 ? s : undefined));

  app.key("shift-right");
  const ahead = until("ten seconds on", (s) => slider(s).length && elapsed(s) >= start + 9 ? s : undefined);
  expect(elapsed(ahead)).toBeLessThan(start + 13);
  app.key("shift-left");
  const back = until("ten seconds back", (s) => slider(s).length && elapsed(s) < start + 5 ? s : undefined);
  expect(elapsed(back)).toBeGreaterThan(start - 2);

  // The middle of the bar is the middle of the song.
  app.click(slider(back)[0]);
  const middle = until("seeked to the middle", (s) => {
    const t = slider(s).length && elapsed(s);
    return t >= 19 && t <= 23 ? s : undefined;
  });
  // A press on the bar seeks; it does not also select the row.
  expect(button(middle, "Import selected").enabled).toBe(false);

  // Dragging scrubs: from the middle to the start of the bar.
  const bar = slider(middle)[0];
  app.drag(bar, { dx: -bar.bounds.width / 2 + 1, dy: 0 }, { steps: 6 });
  until("scrubbed back to the start", (s) => slider(s).length && elapsed(s) <= 3);
});

test("space previews the cursor row", () => {
  openSource();
  app.key("space");
  until("the first row previewing", (s) => button(s, "Pause preview of Alpha"));
  app.key("down");
  app.key("space");
  until("the cursor row previewing", (s) =>
    button(s, "Pause preview of Beta") && button(s, "Play preview of Alpha"));
  // The query stayed empty: space was a command, not text.
  expect(app.snapshot().find({ role: "row", label: "Alpha" }) !== undefined).toBe(true);
});

test("space types a space while a query is being typed", () => {
  openSource();
  app.key("a");
  until("the query's rows", (s) => s.find({ role: "row", label: "Alpha" }) && !s.find({ role: "row", label: "Beta" }));
  app.key("space");
  until("the query with a space in it", (s) => s.find({ role: "row", label: "Spaced" }));
  expect(app.snapshot().findAll({ role: "slider" }).length).toBe(0);
});

test("closing the dialog stops the preview and gives the song its player back", () => {
  nav.venue("Test Venue");
  nav.track("Aurora");
  const time = () => app.snapshot().findAll({ role: "text" })
    .find((n) => /^\d+:\d+ \/ \d+:\d+$/.test(n.label))?.label;
  nav.step("a ready player", "button", "Play");
  until("Aurora playing", (s) => button(s, "Pause"));

  nav.step("the add-track affordance", "button", "Add track");
  app.click(node("the browser", (s) => button(s, "Import tracks")));
  app.click(node("the source menu", (s) => s.find({ role: "row", label: "Rekordbox" })));
  app.click(node("Alpha's play button", (s) => button(s, "Play preview of Alpha")));
  until("Alpha previewing", (s) => button(s, "Pause preview of Alpha"));

  app.click(app.snapshot().find({ role: "button", label: "Close" }));
  until("the dialog gone", (s) => !s.find({ role: "button", label: "Close" }));
  // The song let go of the player for the preview, and has it back: it plays
  // its own 20 s audio, not the 40 s preview's.
  nav.step("the song's player", "button", "Play");
  until("Aurora playing again", (s) => button(s, "Pause"));
  app.frames(4, { waitMs: 60 });
  expect(time()).toMatch(/ \/ 0:20$/);
});

test("leaving the source stops its preview", () => {
  openSource();
  app.click(node("Alpha's play button", (s) => button(s, "Play preview of Alpha")));
  until("Alpha previewing", (s) => button(s, "Pause preview of Alpha"));
  // ← walks the dialog back a route (the shell has a Back button of its own).
  app.key("left");
  until("the browser", (s) => button(s, "Import tracks") && !s.find({ role: "input", label: "Search source…" }));
  app.click(button(app.snapshot(), "Import tracks"));
  app.click(node("the source menu", (s) => s.find({ role: "row", label: "Rekordbox" })));
  const back = until("the source again", (s) => button(s, "Play preview of Alpha"));
  expect(slider(back).length).toBe(0);
});
