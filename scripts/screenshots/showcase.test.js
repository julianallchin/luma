// The website's and README's screenshots: the real app, on a staged library
// that looks like a working one. Not a test; a capture script run by
// `scripts/screenshots/capture`, which keeps each shot under $LUMA_SHOTS.
//
// The library: a venue, "Halcyon Hall", with two rows of moving heads and
// four laser projectors over a stage of decks; eight songs with cover art;
// one of them, "Night Bloom", with real-sounding audio and a busy score. The
// fixture definitions, the art and the audio are in harness/showcase/, made
// by scripts/screenshots/make_assets.py. Every name is made up.

const SHOWCASE = "harness/showcase";
const VENUE = "Halcyon Hall";
const SONG = "Night Bloom";

// -- the rig -----------------------------------------------------------------

const BEAM = "Northlight/Northlight-Arc-Beam.qxf";
const LASER = "Northlight/Northlight-Prism-Laser.qxf";
const ROOT = "venue-main:venue";

const sql = [];
const node = (id, kind, ref, label, mine, theirs, [u, v, trim], extra = {}) => {
  sql.push(`INSERT INTO venue_nodes (id, uid, venue_id, kind, catalog_ref, label, parent_id, my_socket, their_socket, roll)
    VALUES ('${id}', '$PRINCIPAL', 'venue-main', '${kind}', '${ref}', '${label}', '${ROOT}', '${mine}', '${theirs}', 0.0)`);
  for (const [key, value] of [["u", u], ["v", v], ["trim", trim], ...Object.entries(extra)]) {
    sql.push(`INSERT INTO venue_node_params (node_id, uid, venue_id, key, value)
      VALUES ('${id}', '$PRINCIPAL', 'venue-main', '${key}', ${value})`);
  }
};
const groups = {};
const member = (group, id) => (groups[group] ??= []).push(id);
let address = 1;
const patch = (id, path, model, mode, channels, label, at, surface = "rig", extra = {}) => {
  sql.push(`INSERT INTO fixtures (id, uid, venue_id, universe, address, num_channels,
      manufacturer, model, mode_name, fixture_path, label)
    VALUES ('${id}', '$PRINCIPAL', 'venue-main', 1, ${address}, ${channels},
      'Northlight', '${model}', '${mode}', '${path}', '${label}')`);
  address += channels;
  node(id, "fixture", path, label, "clamp", surface, at, extra);
};

const BACK = 10;
for (let i = 0; i < BACK; i++) {
  const id = `beam-back-${i}`;
  const u = (i / (BACK - 1) - 0.5) * 9;
  patch(id, BEAM, "Arc Beam", "8 Channel", 8, `Back beam ${i + 1}`, [u, 3.4, 0], "floor");
  member("back_beams", id);
  member(u < 0 ? "left_side" : "right_side", id);
}
const FRONT = 6;
for (let i = 0; i < FRONT; i++) {
  const id = `beam-front-${i}`;
  const u = (i / (FRONT - 1) - 0.5) * 7;
  patch(id, BEAM, "Arc Beam", "8 Channel", 8, `Front beam ${i + 1}`, [u, -0.5, 4.6]);
  member("front_beams", id);
  member(u < 0 ? "left_side" : "right_side", id);
}
for (let i = 0; i < 4; i++) {
  const id = `laser-${i}`;
  const u = (i / 3 - 0.5) * 7.5;
  patch(id, LASER, "Prism Laser", "ILDA", 1, `Laser ${i + 1}`, [u, 0.9, 0], "floor", { tilt: 0.95 });
  member("lasers", id);
}
// A stage of decks, three wide and two deep.
for (let row = 0; row < 2; row++) {
  for (let col = 0; col < 3; col++) {
    node(`deck-${row}-${col}`, "stage", "stage_lab/stage_praticavel_2x1x1.glb", "Deck", "bottom", "floor",
      [(col - 1) * 2, 1.6 + row, 0]);
  }
}
let order = 0;
for (const [name, members] of Object.entries(groups)) {
  sql.push(`INSERT INTO fixture_groups (id, uid, venue_id, name, display_order)
    VALUES ('group-${name}', '$PRINCIPAL', 'venue-main', '${name}', ${order++})`);
  members.forEach((id, index) => sql.push(`INSERT INTO fixture_group_members
      (id, fixture_id, group_id, head_index, display_order)
    VALUES ('group-${name}-${id}', '${id}', 'group-${name}', -1, ${index})`));
}

// -- the library ---------------------------------------------------------------

const HAZE = { enabled: true, density: 0.38, appearance: { cloudiness: 0.55, cloudSize: 5, turbulence: 0.25, windSpeed: 0.2, windDirection: 30 } };
sql.push(`UPDATE venues SET name = '${VENUE}', haze = '${JSON.stringify(HAZE)}' WHERE id = 'venue-main'`);
sql.push(`UPDATE tracks SET title = '${SONG}', artist = 'Halden', album = 'Night Bloom',
    file_path = '$LIBRARY/night-bloom.mp3', album_art_path = '$LIBRARY/art/night-bloom.jpg',
    album_art_mime = 'image/jpeg' WHERE id = 'track-aurora'`);
const SONGS = [
  ["t-glass", "Glass Harbour", "Mira Vance", "glass-harbour", 122, 236],
  ["t-undertow", "Undertow", "Kesh & Lorne", "undertow", 174, 268],
  ["t-orbit", "Low Orbit", "Tamsin Okoye", "low-orbit", 128, 301],
  ["t-paper", "Paper Satellites", "Odessa Rain", "paper-satellites", 118, 214],
  ["t-static", "Saltwater Static", "Halden", "saltwater-static", 126, 342],
  ["t-afterglow", "Afterglow Theory", "Mira Vance", "afterglow-theory", 124, 257],
  ["t-pilgrim", "Neon Pilgrim", "The Vantage Hours", "neon-pilgrim", 132, 229],
];
for (const [id, title, artist, art, bpm, seconds] of SONGS) {
  sql.push(`INSERT INTO tracks (id, uid, track_hash, title, artist, album, duration_seconds, file_path,
      album_art_path, album_art_mime)
    VALUES ('${id}', '$PRINCIPAL', 'hash-${id}', '${title}', '${artist}', '${title}', ${seconds},
      '/showcase/${id}.mp3', '$LIBRARY/art/${art}.jpg', 'image/jpeg')`);
  sql.push(`INSERT INTO track_beats (track_id, uid, beats_json, downbeats_json, bpm, downbeat_offset, beats_per_bar)
    VALUES ('${id}', '$PRINCIPAL', '[]', '[]', ${bpm}, 0, 4)`);
}
const score = (id, track, name) => sql.push(`INSERT INTO scores (id, uid, track_id, venue_id, name)
  VALUES ('${id}', '$PRINCIPAL', '${track}', 'venue-main', '${name}')`);
score("s-glass", "t-glass", "Opener");
score("s-orbit", "t-orbit", "Peak");
score("s-orbit-2", "t-orbit", "Peak, lasers only");
score("s-static", "t-static", "Long blend");
score("s-afterglow", "t-afterglow", "Sunrise");
score("s-pilgrim", "t-pilgrim", "Encore");
const folder = (id, name, tracks) => {
  sql.push(`INSERT INTO folders (id, uid, venue_id, name) VALUES ('${id}', '$PRINCIPAL', 'venue-main', '${name}')`);
  for (const track of tracks) {
    sql.push(`INSERT INTO folder_tracks (folder_id, track_id, uid, venue_id)
      VALUES ('${id}', '${track}', '$PRINCIPAL', 'venue-main')`);
  }
};
folder("f-warmup", "Warm-up", ["t-glass", "t-paper", "t-afterglow"]);
folder("f-peak", "Peak time", ["track-aurora", "t-orbit", "t-undertow", "t-static", "t-pilgrim"]);
folder("f-closing", "Closing", ["t-afterglow", "t-glass"]);
for (const [id, name] of [["venue-roof", "Roof Garden"], ["venue-studio", "Studio B"]]) {
  sql.push(`INSERT INTO venues (id, uid, name) VALUES ('${id}', '$PRINCIPAL', '${name}')`);
}

// -- the score -----------------------------------------------------------------
// Seconds on a 120 bpm grid: a bar is two seconds. Intro 0–16, build 16–32,
// drop 32–64, break 64–80, outro 80–96.

const clip = (pattern, preset, start, end, lane, selection) =>
  ({ pattern, name: preset, preset, start, end, lane, selection });
const clips = [
  clip("c-wash", "Breathe", 0, 16, 0, "back_beams"),
  clip("c-gradient", "Gradient", 16, 32, 0, "all & ~lasers"),
  clip("c-pills", "Colored pills", 32, 64, 0, "all & ~lasers"),
  clip("c-clouds", "Clouds", 64, 80, 0, "back_beams"),
  clip("c-fade", "Color fade", 80, 96, 0, "all & ~lasers"),
  clip("c-fan", "Fan", 0, 32, 1, "back_beams"),
  clip("c-circle", "Circle", 32, 64, 1, "back_beams"),
  clip("c-sweep", "Sweep", 64, 96, 1, "back_beams"),
  clip("c-nod", "Nod wave", 16, 48, 2, "front_beams"),
  clip("c-scissor", "Scissor", 48, 80, 2, "front_beams"),
  clip("c-chase", "Chase", 16, 32, 3, "front_beams"),
  clip("c-sparkle", "Sparkle", 48, 64, 3, "front_beams"),
  clip("c-beamfan", "Beam fan", 32, 48, 4, "lasers"),
  clip("c-liquid", "Liquid sky", 48, 64, 4, "lasers"),
  clip("c-sheet", "Soft sheet", 64, 80, 4, "lasers"),
  clip("c-ramp", "Strobe steps up", 28, 32, 5, "all & ~lasers"),
  clip("c-strobe", "Strobe", 62, 64, 5, "all & ~lasers"),
];

// -- the agent ---------------------------------------------------------------

const ASK = "The drop feels flat. Can you make it hit harder?";
const OPENING = "Let me look at what's under the drop first.";
const CLOSING =
  "The drop at **bar 17** was one colour wash and a slow circle, so nothing marked the downbeat. " +
  "I changed three things:\n\n" +
  "- **Lasers** open into a beam fan on the first beat, then melt into *Liquid sky* at bar 25\n" +
  "- The **front beams** chase on the kick, and sparkle through the second half\n" +
  "- A two-bar **strobe ramp** leads in, so the drop lands after a build\n\n" +
  "Play from bar 15 to hear it land. Want the break to go darker too?";
const chunks = (text) => text.match(/[\s\S]{1,24}/g).map((chunk) => ({ text: chunk }));
const PYTHON = {
  name: "python",
  latency_ms: 200,
  schema: {
    type: "object",
    required: ["verb", "verbPast", "purpose", "code"],
    properties: { verb: { type: "string" }, verbPast: { type: "string" }, purpose: { type: "string" }, code: { type: "string" } },
  },
  result: {
    status: "ok",
    stdout: "drop: bars 17-32, 128 kicks, energy 0.91\nclips under it: Colored pills, Circle\n",
    stderr: "",
    repr: null,
    traceback: null,
    notices: [],
    figures: [],
    durationMs: 184,
  },
};

fixture({
  mode: "pixel",
  seconds: 96,
  window: [1440, 900],
  motion: true,
  model_cadence_ms: 5,
  sql,
  assets: {
    "night-bloom.mp3": `${SHOWCASE}/night-bloom.mp3`,
    ...Object.fromEntries(["night-bloom", ...SONGS.map((s) => s[3])]
      .map((art) => [`art/${art}.jpg`, `${SHOWCASE}/art/${art}.jpg`])),
    [`fixtures/${BEAM}`]: `${SHOWCASE}/fixtures/${BEAM}`,
    [`fixtures/${LASER}`]: `${SHOWCASE}/fixtures/${LASER}`,
  },
  clips,
  model: [
    [
      ...chunks(OPENING),
      { call: "python", id: "call-1", args: { verb: "Reading", verbPast: "Read", purpose: "the drop", code: "track.window(at=32, bars=16)" } },
      { end: "tool_use" },
    ],
    [...chunks(CLOSING), { end: "end_turn" }],
  ],
  tools: [PYTHON],
});

// -- the shots ---------------------------------------------------------------

const settle = (n = 30) => app.frames(n, { waitMs: 50 });
const keep = (name) => {
  // The pointer off every control, so no hover fill or tooltip is in the shot.
  app.scroll({ x: 2, y: 450 }, { dy: 0 });
  settle(12);
  image.keep(app.screenshot(), `showcase/${name}`);
};

function houseLightsDown() {
  const slider = until("the house lights", (s) => s.find({ role: "slider", label: "House lights" }))
    .find({ role: "slider", label: "House lights" });
  app.drag(slider, { dx: 0, dy: 140 }, { steps: 12, restale: "match" });
  until("the house lights down", (s) => s.find({ role: "text", label: "House lights = 0%" }));
}

// The transport to `fraction` of the song, by a press on the ruler.
function seek(fraction) {
  const ruler = until("the ruler", (s) => s.find({ role: "card", label: "Ruler" }))
    .find({ role: "card", label: "Ruler" }).bounds;
  app.drag({ x: ruler.x + ruler.width * fraction, y: ruler.y + ruler.height / 2 }, { dx: 0, dy: 0 }, { steps: 1 });
}

// The whole song across the timeline, every lane in view, the stage a little
// shorter so the lanes have room.
function openSong() {
  nav.venue(VENUE);
  nav.track(SONG);
  until("the clips", (s) => s.find({ role: "card", label: "Breathe" }));
  houseLightsDown();
  const seam = until("the stage seam", (s) => s.find({ role: "slider", label: "Stage height" }))
    .find({ role: "slider", label: "Stage height" });
  app.drag(seam, { dx: 0, dy: -150 }, { steps: 10 });
  const waveform = () => app.snapshot().find({ role: "card", label: "Waveform" });
  app.scroll(waveform(), { dy: -4000, steps: 20, modifiers: ["control"] });
  app.scroll(waveform(), { dx: 80000, steps: 20 });
  app.key("h");
  settle(10);
}

test("arrange", { timeoutMs: 300000 }, () => {
  openSong();
  seek(0.42);
  nav.step("new conversation", "button", "New chat");
  const field = until("the composer", (s) => composer(s)).find({ role: "input", label: "Do anything…" });
  app.type(field, ASK);
  app.click(app.snapshot().find({ role: "button", label: "Send" }));
  until("the turn to begin", busy);
  until("the turn to end", (s) => !busy(s) && texts(s).some((t) => t.includes("Want the break")), { timeoutMs: 60000 });
  settle(60);
  keep("arrange");
});

// The backdrop the composites stand on: the stage alone, mid-drop.
test("stage", { timeoutMs: 300000 }, () => {
  openSong();
  nav.agentOff();
  seek(0.36);
  settle(10);
  nav.step("fullscreen", "button", "Fullscreen visualizer");
  settle(20);
  const card = app.snapshot().find({ role: "card", label: "Stage" }).bounds;
  app.scroll({ x: card.x + card.width / 2, y: card.y + card.height * 0.4 }, { dy: 30, steps: 3 });
  settle(80);
  keep("stage");
});

test("songs", { timeoutMs: 300000 }, () => {
  nav.venue(VENUE);
  nav.songs();
  nav.step("the folder", "row", "Folder Peak time");
  nav.step("the song", "row", "Low Orbit");
  until("its scores", (s) => nav.scoreRows(s, "Low Orbit").length === 2);
  keep("songs");
});

test("live", { timeoutMs: 300000 }, () => {
  nav.venue(VENUE);
  nav.live();
  until("the set's first page", (s) => s.find({ role: "toggle", label: "Page Main" }));
  nav.step("Edit layout", "button", "Edit layout");
  until("the grid to arrange", (s) => s.find({ role: "button", label: "Select Master" }));
  nav.step("a busk page", "button", "Add busk page");
  until("the busk page", (s) => s.find({ role: "toggle", label: "Page Busk" }), { timeoutMs: 20000 });
  nav.step("Done", "button", "Done");
  nav.step("the busk page", "toggle", "Page Busk");
  nav.step("the offer", "button", "Add source");
  nav.step("the virtual deck", "toggle", "Virtual");
  nav.step("a song for the deck", "button", "Choose song");
  nav.step("Night Bloom", "row", SONG);
  until("the song on the deck", (s) => s.findAll({ role: "card" }).some((n) => n.label.startsWith(`Deck 1: ${SONG}`)));
  nav.agentOff();
  houseLightsDown();
  nav.step("every group", "button", "Group: All");
  for (const pad of ["Pad: Chase", "Pad: Circle"]) {
    nav.step(pad, "button", pad);
  }
  nav.step("an accent colour", "button", "Accent color Value 5, #8A2BE2");
  settle(80);
  keep("live");
});

test("rig", { timeoutMs: 300000 }, () => {
  nav.patch(VENUE);
  nav.agentOff();
  settle(20);
  // Back from the decks, so the whole rig is in view.
  const card = app.snapshot().find({ role: "card", label: "Stage" }).bounds;
  app.scroll({ x: card.x + card.width / 2, y: card.y + card.height / 2 }, { dy: 120, steps: 6 });
  settle(40);
  keep("rig");
});

test("settings", { timeoutMs: 300000 }, () => {
  nav.venue(VENUE);
  nav.settings();
  nav.step("Shortcuts", "toggle", "Shortcuts");
  settle(20);
  keep("settings");
});
