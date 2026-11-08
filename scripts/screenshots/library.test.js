// The website's screenshots from a copy of a real library, named by
// $LUMA_SHOWCASE_LIBRARY: `scripts/screenshots/capture --library DIR`. Not a
// test; a capture script. The fixture copies only the database, signs it in
// with a session made for the run, strips stored API keys, turns DMX and
// audio output off and keeps the app off the network (see `library` in
// gpui-agent's fixture.rs). Anything a shot changes lands in that copy.
//
// The venues, songs and scores are the ones in the library this was written
// against; another library names its own below.

const VENUE = "Gasworks Park";
const SONG = "RXGER CLUB!";
const SONG_AT = 0.3;
const LIVE_VENUE = "laser test";
const LIVE_SONG = "..FEISTY";
const LIVE_PADS = ["Beam fan", "Bar chase"];

fixture({
  mode: "pixel",
  window: [1440, 900],
  library: "$LUMA_SHOWCASE_LIBRARY",
  sql: ["INSERT OR REPLACE INTO settings (key, value) VALUES ('stage_low_quality', 'false')"],
});

const settle = (n = 30) => app.frames(n, { waitMs: 50 });
const keep = (name) => {
  // The pointer off every control, so no hover fill or tooltip is in the shot.
  app.scroll({ x: 2, y: 450 }, { dy: 0 });
  settle(12);
  image.keep(app.screenshot(), `showcase/${name}`);
};

// The transport to `fraction` of the song, by a press on the ruler.
function seek(fraction) {
  const ruler = until("the ruler", (s) => s.find({ role: "card", label: "Ruler" }))
    .find({ role: "card", label: "Ruler" }).bounds;
  app.drag({ x: ruler.x + ruler.width * fraction, y: ruler.y + ruler.height / 2 }, { dx: 0, dy: 0 }, { steps: 1 });
}

// The whole song across the timeline, every lane in view.
function openSong(venue, song) {
  nav.venue(venue);
  nav.track(song);
  until("the waveform", (s) => s.find({ role: "card", label: "Waveform" }), { timeoutMs: 60000 });
  const waveform = () => app.snapshot().find({ role: "card", label: "Waveform" });
  app.scroll(waveform(), { dy: -4000, steps: 20, modifiers: ["control"] });
  app.scroll(waveform(), { dx: 80000, steps: 20 });
  app.key("h");
  settle(20);
}

test("arrange", { timeoutMs: 600000 }, () => {
  openSong(VENUE, SONG);
  seek(SONG_AT);
  settle(60);
  keep("arrange");
});

test("stage", { timeoutMs: 600000 }, () => {
  openSong(VENUE, SONG);
  nav.agentOff();
  seek(SONG_AT);
  settle(10);
  nav.step("fullscreen", "button", "Fullscreen visualizer");
  settle(80);
  keep("stage");
});

test("songs", { timeoutMs: 600000 }, () => {
  nav.venue(VENUE);
  nav.songs();
  nav.step("the song", "row", SONG);
  until("its scores", (s) => nav.scoreRows(s, SONG).length > 0);
  keep("songs");
});

test("live", { timeoutMs: 600000 }, () => {
  nav.venue(LIVE_VENUE);
  nav.live();
  nav.agentOff();
  nav.step("the offer", "button", "Add source");
  nav.step("the virtual deck", "toggle", "Virtual");
  nav.step("a song for the deck", "button", "Choose song");
  nav.step("the song", "row", LIVE_SONG);
  until("the song on the deck", (s) => s.findAll({ role: "card" }).some((n) => n.label.startsWith(`Deck 1: ${LIVE_SONG}`)));
  for (const pad of LIVE_PADS) nav.step(pad, "button", `Pad: ${pad}`);
  settle(80);
  keep("live");
});

// Rig on the indoor venue: the outdoor one is a night sky until it plays.
test("rig", { timeoutMs: 600000 }, () => {
  nav.patch(LIVE_VENUE);
  nav.agentOff();
  // The house lights up, so the room is there to see; the copy keeps it.
  const slider = until("the house lights", (s) => s.find({ role: "slider", label: "House lights" }))
    .find({ role: "slider", label: "House lights" });
  app.drag(slider, { dx: 0, dy: -140 }, { steps: 12, restale: "match" });
  settle(60);
  keep("rig");
});

test("settings", { timeoutMs: 600000 }, () => {
  nav.venue(VENUE);
  nav.settings();
  nav.step("Shortcuts", "toggle", "Shortcuts");
  settle(20);
  keep("settings");
});
