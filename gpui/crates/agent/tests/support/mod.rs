//! One seeded Luma library, and a harness pointed at it.
//!
//! Two tests want this fixture with different numbers in it: the exit gate
//! asserts exact geometry over two clips on twenty seconds of audio, and the
//! frame budget needs a track long enough — and a timeline crowded enough —
//! that a full zoom-out has something to be slow about. What they need is
//! identical apart from those sizes, so the seeding is one thing and the sizes
//! are arguments.
//!
//! # Why the fixture has real audio in it
//!
//! Unlike the browser's, this fixture cannot be rows alone. `host_load_track`
//! decodes the file at `tracks.file_path` and `get_track_waveform` renders its
//! envelope from the same file, so a row pointing at a path that does not
//! exist gives a screen with no waveform, a transport that cannot start, and —
//! because a failed load sets `Editor::error` — no canvas at all, only an
//! error plate. Synthesized WAV is the smallest thing both commands accept.
//!
//! Audio *output* is turned off in the fixture's settings. The transport's
//! position is driven by a wall clock rather than by the sound card (see
//! `host_audio::refresh_progress`), so the playhead advances either way — but
//! opening a cpal stream needs an output device, and a test that failed on a
//! machine without one would be testing the machine.
//!
//! # Why the score goes through the seam
//!
//! Like the graph editor's fixture, and for the same reason: a score is an
//! authored document with a content-addressed revision, and a `track_scores`
//! row inserted behind that is a clip the editor can read and never write to.

// Each test binary uses the parts of this it needs, and cargo compiles the
// whole module into each — the standard shape of a shared test fixture.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui::{AnyView, App, AppContext as _, Window};
use gpui_agent::{Config, Harness, Mode};
use serde_json::{json, Value};
use sqlx::SqlitePool;

/// `until(what, pred)` in a script — see `support/until.js`.
pub const UNTIL: &str = include_str!("until.js");

/// `nav.*` in a script — the suite's one description of how to reach a view.
///
/// Carries [`UNTIL`] with it, because every step polls: a test that spliced
/// this alone would get a `nav` whose first call died on an undefined `until`,
/// and pairing them here is what makes "which helpers do I need" a question
/// with one answer.
pub const NAV: &str = concat!(include_str!("until.js"), include_str!("nav.js"));

/// `body`, with the navigation helpers in front of it.
///
/// A function rather than a `concat!` at each `const SCRIPT` because `concat!`
/// only takes literals, and a suite where half the tests spliced the helpers
/// and half re-declared them would be the duplication this module removes.
#[must_use]
pub fn script(body: &str) -> String {
    format!("{NAV}\n{body}")
}

pub const VENUE: &str = "venue-main";
pub const VENUE_NAME: &str = "Test Venue";
pub const TRACK: &str = "track-aurora";
pub const TRACK_NAME: &str = "Aurora";

/// One clip to put on the timeline, before the editor resolves it into a lane.
pub struct Clip {
    /// Also the pattern's id. A clip is named in the automation tree by its
    /// pattern, so two clips of one pattern would be two nodes under one label
    /// and `find` would silently take the first — every clip here gets its own.
    pub pattern: String,
    pub name: String,
    pub start: f64,
    pub end: f64,
    pub z_index: i64,
    /// Whether the pattern gets a graph that actually emits light — see
    /// [`Clip::lit`]. Off by default: a clip on a timeline is a rectangle, and
    /// every test but the 3D view's is testing the rectangle.
    pub lit: bool,
}

impl Clip {
    pub fn new(pattern: impl Into<String>, name: impl Into<String>, start: f64, end: f64) -> Self {
        Self {
            pattern: pattern.into(),
            name: name.into(),
            start,
            end,
            z_index: 0,
            lit: false,
        }
    }

    /// Give the pattern a graph that lights every fixture, pulsing with the
    /// beat grid.
    ///
    /// Without this a clip's pattern is a `patterns` row with no graph
    /// document behind it, which composites to a scene that evaluates to
    /// nothing — fine for a timeline, useless for a view whose whole subject
    /// is the light. See [`Fixture::light`] for the graph.
    pub fn lit(mut self) -> Self {
        self.lit = true;
        self
    }

    pub fn lane(mut self, z_index: i64) -> Self {
        self.z_index = z_index;
        self
    }
}

/// A library with one venue, one track, one score, and clips on it.
pub struct Fixture {
    /// Distinguishes one test binary's config directory from another's, which
    /// matters because `LUMA_CONFIG_DIR` is process-global: two tests seeding
    /// the same directory would be one library with both their contents.
    name: &'static str,
    seconds: u32,
    clips: Vec<Clip>,
    /// Whether the venue gets a rig — patched fixtures, a stage piece, and a
    /// fixture bundle for them to resolve against. Off by default: every other
    /// fixture in this file is testing rows and geometry, and a rig would only
    /// be a slower seed.
    rig: bool,
    window: Option<gpui::Size<gpui::Pixels>>,
}

impl Fixture {
    pub fn new(name: &'static str, seconds: u32, clips: Vec<Clip>) -> Self {
        Self {
            name,
            seconds,
            clips,
            rig: false,
            window: None,
        }
    }

    /// Open the window at `width` × `height` instead of the default 1200×800.
    ///
    /// For tests whose geometry premises were authored against a full-window
    /// canvas: the shell's sidebar takes `shell::SIDEBAR_WIDTH` of the row, so
    /// growing the window by exactly that much puts the tab body back at the
    /// width every pixel-arithmetic comment in those files was written for.
    #[allow(dead_code)]
    pub fn window(mut self, width: f32, height: f32) -> Self {
        self.window = Some(gpui::size(gpui::px(width), gpui::px(height)));
        self
    }

    /// Patch a small rig into the venue and bundle the definition it needs.
    ///
    /// Also sets `LUMA_FIXTURES_ROOT`, so the bundled definition is the one the
    /// app resolves rather than the developer's — the same escape hatch
    /// `LUMA_CONFIG_DIR` is, and process-global for the same reason.
    pub fn with_rig(mut self) -> Self {
        self.rig = true;
        self
    }

    /// Seed the library and open the app on it.
    ///
    /// Sets `LUMA_CONFIG_DIR`, so exactly one fixture may be open per process.
    pub fn open(self, mode: Mode) -> Harness {
        std::env::set_var("LUMA_CONFIG_DIR", self.seed());
        // A walk acts and then looks. With the panel slides running, "looks"
        // would land mid-transition and every geometry assertion would be a
        // race against a 200ms tween; snapped, the frame after an action is
        // the finished one. Process-global for the same reason the config
        // directory is — and left alone when the run already asked for motion
        // (`shell_motion` shoots the slides themselves).
        if std::env::var_os("LUMA_MOTION").is_none() {
            std::env::set_var("LUMA_MOTION", "off");
        }
        let root: gpui_agent::RootFactory = Arc::new(|_: &mut Window, cx: &mut App| -> AnyView {
            luma_app::init(cx);
            let library = luma_app::Library::open().expect("failed to open the fixture library");
            cx.new(|cx| luma_app::Luma::new(library, cx)).into()
        });
        let mut config = Config {
            mode,
            call_timeout: Duration::from_secs(120),
            ..Config::default()
        };
        if let Some(window) = self.window {
            config.window_size = window;
        }
        Harness::headless(config, root).expect("failed to start the harness")
    }

    /// A library of its own, so the run cannot see — or corrupt — the
    /// developer's. Named after the process so two runs never share one.
    fn seed(&self) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("luma-gpui-{}-{}", self.name, std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("failed to create the temporary config directory");
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("failed to start the fixture runtime")
            .block_on(self.write(&dir));
        dir
    }

    async fn write(&self, config_dir: &Path) {
        let audio = config_dir.join("aurora.wav");
        std::fs::write(&audio, wav(self.seconds)).expect("failed to write the fixture audio");

        // Rows first, while admission is still unarmed — the same window the
        // browser's fixture writes through.
        let db = luma_lib::database::local::database::init_app_db_at(config_dir)
            .await
            .expect("failed to open the fixture database");
        let pool = &db.0;
        sqlx::query("INSERT INTO venues (id, uid, name) VALUES (?, NULL, ?)")
            .bind(VENUE)
            .bind(VENUE_NAME)
            .execute(pool)
            .await
            .expect("failed to seed the venue");
        // A lit clip's pattern is created through the seam instead, further
        // down: `create_pattern` also makes the graph *implementation* a graph
        // document hangs off, and a row inserted here has none.
        for clip in self.clips.iter().filter(|clip| !clip.lit) {
            sqlx::query("INSERT INTO patterns (id, uid, name) VALUES (?, NULL, ?)")
                .bind(&clip.pattern)
                .bind(&clip.name)
                .execute(pool)
                .await
                .expect("failed to seed a pattern");
        }
        sqlx::query(
            "INSERT INTO tracks (id, uid, track_hash, title, artist, duration_seconds, file_path)
             VALUES (?, NULL, 'aurora-hash', ?, 'Nightliner', ?, ?)",
        )
        .bind(TRACK)
        .bind(TRACK_NAME)
        .bind(f64::from(self.seconds))
        .bind(audio.to_string_lossy().to_string())
        .execute(pool)
        .await
        .expect("failed to seed the track");
        self.seed_beats(pool).await;
        if self.rig {
            self.seed_rig(pool, config_dir).await;
        }

        // Then the score, through the seam — see the module docs.
        let state_db = luma_lib::database::local::state::init_state_db_at(config_dir)
            .await
            .expect("failed to open the fixture state database");
        luma_lib::database::local::auth::bootstrap_host_admission(&db.0, &state_db.0)
            .await
            .expect("failed to arm admission");
        let storage = luma_lib::storage::StorageRoot::from_path(config_dir.to_path_buf());
        let workspaces = Arc::new(
            luma_lib::agent_execution::workspace::PythonWorkspaceService::new(
                storage.agent_workspaces_dir(),
                Arc::new(|| Err("the fixture does not run Python workspaces".to_string())),
            ),
        );
        let services = luma_lib::dispatch::AppServices::headless(
            db,
            state_db,
            storage,
            config_dir.to_path_buf(),
            workspaces,
        );

        call(
            &services,
            "set_setting",
            json!({ "key": "audio_output_enabled", "value": "false" }),
        )
        .await;

        let score = call(
            &services,
            "create_score",
            json!({
                "requestId": request_id(0),
                "trackId": TRACK,
                "venueId": VENUE,
                "name": "Fixture Score",
            }),
        )
        .await;
        let score_id = score["id"].as_str().expect("a created score has an id");

        for (index, clip) in self.clips.iter().enumerate() {
            // `create_pattern` mints its own id, so a lit clip's score row
            // names that one rather than `clip.pattern` — which for a lit clip
            // is only the request key that keeps re-seeding idempotent.
            let pattern = if clip.lit {
                let created = call(
                    &services,
                    "create_pattern",
                    json!({ "requestId": request_id(800 + index),
                            "name": clip.name, "description": null }),
                )
                .await;
                let id = created["id"]
                    .as_str()
                    .expect("a created pattern has an id")
                    .to_string();
                self.light(&services, &id, index).await;
                id
            } else {
                clip.pattern.clone()
            };
            call(
                &services,
                "create_track_score",
                json!({ "payload": {
                    "requestId": request_id(index + 1),
                    "scoreId": score_id,
                    "trackId": TRACK,
                    "patternId": pattern,
                    "startTime": clip.start,
                    "endTime": clip.end,
                    "zIndex": clip.z_index,
                }}),
            )
            .await;
        }
    }

    /// Author `pattern`'s graph as "every selected fixture, red, pulsing once
    /// every four beats".
    ///
    /// `pattern_args.selection → apply_color.selection` and
    /// `color × sine_wave → apply_color.signal`, which is the smallest graph
    /// that is both *visible* (a saturated hue no part of the rig or the grid
    /// shares, so a red pixel can only have come from a beam) and *moving*
    /// (half a cycle per second at the fixture's 120 bpm, so two shots a second
    /// apart cannot agree by luck).
    ///
    /// Written through the seam rather than as a `patterns` row because a graph
    /// is an authored document with a content-addressed revision — the same
    /// reason the score is. See the module docs.
    async fn light(&self, services: &luma_lib::dispatch::AppServices, pattern: &str, index: usize) {
        let document = call(
            services,
            "get_pattern_graph_document",
            json!({ "id": pattern, "implementationId": null }),
        )
        .await;
        let node = |id: &str, type_id: &str, params: Value| {
            json!({ "id": id, "typeId": type_id, "params": params,
                    "positionX": 0.0, "positionY": 0.0 })
        };
        let edge = |from: &str, from_port: &str, to: &str, to_port: &str| {
            json!({ "id": format!("{from}{from_port}-{to}{to_port}"),
                    "fromNode": from, "fromPort": from_port,
                    "toNode": to, "toPort": to_port })
        };
        call(
            services,
            "save_pattern_graph_document",
            json!({
                "id": pattern,
                "implementationId": document["implementationId"],
                "operationId": request_id(900 + index),
                "baseRevision": document["revision"],
                "graph": {
                    "nodes": [
                        node("pattern_args", "pattern_args", json!({})),
                        node("red", "color", json!({ "color": r#"{"r":255,"g":0,"b":0,"a":1}"# })),
                        // A quarter cycle per beat — 2 s per pulse at 120 bpm
                        // — a quarter turn ahead, so t = 0 is the peak rather
                        // than the zero crossing a stopped transport sits on.
                        node("pulse", "sine_wave",
                             json!({ "subdivision": 0.25, "phase_deg": 90.0 })),
                        node("mix", "math", json!({ "operation": "multiply" })),
                        node("apply", "apply_color", json!({})),
                    ],
                    "edges": [
                        edge("red", "out", "mix", "a"),
                        edge("pulse", "out", "mix", "b"),
                        edge("mix", "out", "apply", "signal"),
                        edge("pattern_args", "selection", "apply", "selection"),
                    ],
                    "args": [{
                        "id": "selection",
                        "name": "selection",
                        "argType": "Selection",
                        "defaultValue": { "expression": "all", "spatialReference": "global" },
                    }],
                },
            }),
        )
        .await;
    }

    /// Four movers in a row and a deck under them, plus the QLC+ definition
    /// they name.
    ///
    /// Written here rather than pointed at `resources/fixtures` because a test
    /// that depended on the shipped bundle would fail the day a definition in
    /// it was renamed, and would be testing that bundle rather than the view.
    async fn seed_rig(&self, pool: &SqlitePool, config_dir: &Path) {
        let bundle = config_dir.join("fixtures");
        std::fs::create_dir_all(bundle.join("Luma")).expect("failed to create the fixture bundle");
        std::fs::write(bundle.join("Luma/Mover.qxf"), MOVER_QXF)
            .expect("failed to write the fixture definition");
        std::env::set_var("LUMA_FIXTURES_ROOT", &bundle);

        for i in 0..4 {
            sqlx::query(
                "INSERT INTO fixtures (id, uid, venue_id, universe, address, num_channels,
                                       manufacturer, model, mode_name, fixture_path, label,
                                       pos_x, pos_y, pos_z, rot_x, rot_y, rot_z)
                 VALUES (?, NULL, ?, 1, ?, 8, 'Luma', 'Mover', 'Default', ?, ?, ?, 0.0, 3.0, 0.0, 0.0, 0.0)",
            )
            .bind(format!("fixture-{i}"))
            .bind(VENUE)
            .bind(i * 8 + 1)
            .bind(MOVER_PATH)
            .bind(format!("Mover {i}"))
            .bind(f64::from(i) * 1.5 - 2.25)
            .execute(pool)
            .await
            .expect("failed to patch a fixture");
        }

        sqlx::query(
            "INSERT INTO stage_pieces (id, uid, venue_id, mesh_path, kind, label,
                                       pos_x, pos_y, pos_z, rot_x, rot_y, rot_z, scale)
             VALUES ('piece-deck', NULL, ?, 'stage_lab/stage_praticavel_2x1x1.glb', 'floor', 'Deck',
                     0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0)",
        )
        .bind(VENUE)
        .execute(pool)
        .await
        .expect("failed to place the stage piece");
    }

    /// A steady 120 bpm grid over the whole track: four beats to the bar, so
    /// the editor draws bar lines and numbers rather than falling back to the
    /// clock ruler.
    async fn seed_beats(&self, pool: &SqlitePool) {
        let beats: Vec<f64> = (0..self.seconds * 2)
            .map(|index| f64::from(index) * 0.5)
            .collect();
        let downbeats: Vec<f64> = beats.iter().copied().step_by(4).collect();
        sqlx::query(
            "INSERT INTO track_beats (track_id, uid, beats_json, downbeats_json, bpm, downbeat_offset, beats_per_bar)
             VALUES (?, NULL, ?, ?, 120.0, 0.0, 4)",
        )
        .bind(TRACK)
        .bind(serde_json::to_string(&beats).unwrap())
        .bind(serde_json::to_string(&downbeats).unwrap())
        .execute(pool)
        .await
        .expect("failed to seed the beat grid");
    }
}

/// Where [`Fixture::with_rig`]'s fixtures are patched from, relative to the
/// bundle root it writes.
pub const MOVER_PATH: &str = "Luma/Mover.qxf";

/// The smallest QLC+ definition the renderer reads anything out of: a `Type` it
/// maps to a mesh, one mode, and a lens whose angle drives the cone.
const MOVER_QXF: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<FixtureDefinition>
 <Manufacturer>Luma</Manufacturer>
 <Model>Mover</Model>
 <Type>Moving Head</Type>
 <Channel Name="Dimmer" Preset="IntensityMasterDimmer"/>
 <Mode Name="Default">
  <Channel Number="0">Dimmer</Channel>
 </Mode>
 <Physical>
  <Dimensions Weight="10" Width="300" Height="400" Depth="300"/>
  <Lens Name="Fixed" DegreesMin="14" DegreesMax="14"/>
 </Physical>
</FixtureDefinition>
"#;

/// The idempotency key for the fixture's `n`th authored write. The authored
/// store validates these as UUIDs, and they are fixed so that re-seeding one
/// directory replays rather than duplicates.
fn request_id(n: usize) -> String {
    format!("5b3f0a10-0000-4000-8000-{n:012}")
}

async fn call(services: &luma_lib::dispatch::AppServices, name: &str, args: Value) -> Value {
    luma_lib::dispatch::dispatch(services, name, &args)
        .await
        .unwrap_or_else(|error| panic!("fixture command {name} failed: {error}"))
}

/// `seconds` of 16-bit stereo PCM at 44.1 kHz, as a WAV file.
///
/// A slow amplitude sweep over a 220 Hz tone rather than silence: the waveform
/// renderer's band envelopes are the point of loading it, and a flat signal
/// would give three bands of zero and nothing to look at — or to draw slowly.
fn wav(seconds: u32) -> Vec<u8> {
    const RATE: u32 = 44_100;
    let frames = RATE * seconds;
    let mut samples = Vec::with_capacity(frames as usize * 4);
    for frame in 0..frames {
        let t = frame as f32 / RATE as f32;
        let envelope = 0.2 + 0.6 * (t * 0.5).sin().abs();
        let value = ((t * 220. * std::f32::consts::TAU).sin() * envelope * i16::MAX as f32) as i16;
        samples.extend_from_slice(&value.to_le_bytes());
        samples.extend_from_slice(&value.to_le_bytes());
    }

    let mut file = Vec::with_capacity(samples.len() + 44);
    file.extend_from_slice(b"RIFF");
    file.extend_from_slice(&(36 + samples.len() as u32).to_le_bytes());
    file.extend_from_slice(b"WAVEfmt ");
    file.extend_from_slice(&16u32.to_le_bytes()); // PCM header length
    file.extend_from_slice(&1u16.to_le_bytes()); // PCM
    file.extend_from_slice(&2u16.to_le_bytes()); // stereo
    file.extend_from_slice(&RATE.to_le_bytes());
    file.extend_from_slice(&(RATE * 4).to_le_bytes()); // bytes per second
    file.extend_from_slice(&4u16.to_le_bytes()); // bytes per frame
    file.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    file.extend_from_slice(b"data");
    file.extend_from_slice(&(samples.len() as u32).to_le_bytes());
    file.extend_from_slice(&samples);
    file
}
