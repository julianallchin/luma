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
use luma_ui::runtime::Runtime;
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

/// A runtime pointed at `config_dir` with motion snapped — what every harness
/// in this suite wants that does not go through [`Fixture::open`].
///
/// Carried in [`Config`] rather than set in the environment, so that two of
/// these may be live at once in one test binary. Everything else still falls
/// back to the environment, which keeps the escape hatches working for a human
/// running a single test by hand.
#[must_use]
pub fn runtime(config_dir: impl Into<PathBuf>) -> Runtime {
    Runtime {
        config_dir: Some(config_dir.into()),
        reduced_motion: true,
        ..Runtime::default()
    }
}

/// What the `index`th seeded conversation asked. Distinct per score, so "which
/// conversation is on screen" is a question a snapshot can answer — see
/// [`Fixture::with_seeded_threads`].
#[must_use]
pub fn seeded_prompt(index: usize) -> String {
    format!("Seeded question about score {index}")
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
    /// Distinguishes one test's config directory from another's: two tests
    /// seeding the same directory would be one library with both their
    /// contents. Keep it unique per test, not per file — a merged suite runs
    /// many of these side by side in one process.
    name: &'static str,
    seconds: u32,
    clips: Vec<Clip>,
    /// Whether the venue gets a rig — patched fixtures, a stage piece, and a
    /// fixture bundle for them to resolve against. Off by default: every other
    /// fixture in this file is testing rows and geometry, and a rig would only
    /// be a slower seed.
    rig: usize,
    /// Whether the rig is placed where a coordinate-space bug would show. See
    /// [`Fixture::with_skewed_rig`].
    skewed_rig: bool,
    seed_track: bool,
    track_created_at: Option<String>,
    window: Option<gpui::Size<gpui::Pixels>>,
    source_fixture: Option<luma_app::SourceAdapterFixture>,
    source_fixture_delay: Option<Duration>,
    source_search_responses: Vec<luma_app::SourceSearchFixtureResponse>,
    source_import_fixture_delay: Option<Duration>,
    equal_timestamp_track: bool,
    force_motion: bool,
    motion_scale: Option<f32>,
    extra_tracks: usize,
    album_art: Option<usize>,
    extra_scores: usize,
    seeded_threads: bool,
}

impl Fixture {
    pub fn new(name: &'static str, seconds: u32, clips: Vec<Clip>) -> Self {
        Self {
            name,
            seconds,
            clips,
            rig: 0,
            skewed_rig: false,
            seed_track: true,
            track_created_at: None,
            window: None,
            source_fixture: None,
            source_fixture_delay: None,
            source_search_responses: Vec::new(),
            source_import_fixture_delay: None,
            equal_timestamp_track: false,
            force_motion: false,
            motion_scale: None,
            extra_tracks: 0,
            album_art: None,
            extra_scores: 0,
            seeded_threads: false,
        }
    }

    /// Mint `count` further scores on the seeded `(track, venue)`, after the
    /// one the clips are authored on.
    ///
    /// A track/venue pair holds one score per principal in production, and a
    /// headless fixture has exactly one principal — so the extra scores are
    /// minted through the same `create_score` seam with distinct request ids,
    /// which is the only thing that decides identity there. What they give a
    /// test is the *shape* the rail exists for: more than one score to choose
    /// between, in ordinal order.
    #[allow(dead_code)]
    pub fn with_extra_scores(mut self, count: usize) -> Self {
        self.extra_scores = count;
        self
    }

    /// Give every score on the seeded pair a track-agent conversation with a
    /// message already in it.
    ///
    /// What it buys a test is a thread whose *arrival* is observable: an empty
    /// conversation looks the same before and after its read lands, so nothing
    /// about loading can be asserted over one.
    #[allow(dead_code)]
    pub fn with_seeded_threads(mut self) -> Self {
        self.seeded_threads = true;
        self
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
    /// Also points the harness's fixtures root at that bundle, so the app
    /// resolves the definition this fixture wrote rather than the developer's.
    pub fn with_rig(self) -> Self {
        self.with_rig_of(4)
    }

    /// `count` movers placed where a space bug is *visible*, and no deck.
    ///
    /// Every coordinate in the default rig lies on `y = 0`, which is the one
    /// plane the data→world mirror (`coords::world_from_data`) leaves fixed —
    /// so a screen that drew the editor's affordances in the wrong space could
    /// and did look perfectly correct in it. This rig sits off that plane. The
    /// deck is left out so a marquee across the viewport selects fixtures and
    /// nothing else.
    pub fn with_skewed_rig(mut self, count: usize) -> Self {
        self.skewed_rig = true;
        self.with_rig_of(count)
    }

    /// Patch `count` movers instead of the default four.
    ///
    /// Rig size is the axis most renderer costs scale on — cluster occupancy,
    /// shadow passes, draw count — so a test about frame cost has to be able to
    /// ask for a venue-sized one. The geometry is the same line of movers,
    /// spread to keep the whole rig in frame.
    pub fn with_rig_of(mut self, count: usize) -> Self {
        self.rig = count;
        self
    }

    /// Install raw DJ-adapter answers before constructing the app. The UI
    /// still crosses the production Library normalization and import seams;
    /// only the external Engine/Rekordbox database read is substituted.
    pub fn with_source_fixture(mut self, fixture: luma_app::SourceAdapterFixture) -> Self {
        self.source_fixture = Some(fixture);
        self
    }

    /// Hold the first normalized source read for a deterministic loading-state
    /// snapshot; the fixture still resolves through the production facade.
    pub fn with_source_fixture_delay(mut self, delay: Duration) -> Self {
        self.source_fixture_delay = Some(delay);
        self
    }

    pub fn with_source_search_responses(
        mut self,
        responses: Vec<luma_app::SourceSearchFixtureResponse>,
    ) -> Self {
        self.source_search_responses = responses;
        self
    }

    pub fn with_source_import_fixture_delay(mut self, delay: Duration) -> Self {
        self.source_import_fixture_delay = Some(delay);
        self
    }

    pub fn with_equal_timestamp_track(mut self) -> Self {
        self.equal_timestamp_track = true;
        self
    }

    /// Force authored motion on for a pixel test that measures a transition.
    /// This is deliberately opt-in: normal automation continues to honor the
    /// snapped harness policy and production still honors OS reduced motion.
    pub fn with_motion(mut self) -> Self {
        self.force_motion = true;
        self
    }

    /// Stretch every timeline by `scale`, so a screenshot burst can sample a
    /// 200ms tween per frame. Only meaningful alongside [`Self::with_motion`].
    ///
    /// Per-fixture rather than per-process, which is what lets one suite hold
    /// a test that wants 10x next to one that wants 3x.
    #[allow(dead_code)]
    pub fn with_motion_scale(mut self, scale: f32) -> Self {
        self.motion_scale = Some(scale);
        self
    }

    /// Keep the venue but omit the library track and its score. Used by
    /// outside-in empty-library flows whose only entry point is the sidebar
    /// head rather than an existing track row.
    pub fn without_track(mut self) -> Self {
        self.seed_track = false;
        self
    }

    /// Pad the library with `count` extra rows, so a list is a *library*
    /// rather than a line.
    ///
    /// The seeded track alone is the right fixture for behaviour and the wrong
    /// one for cost: anything that walks the library per frame is invisible at
    /// one row. These carry no audio and no beats — they exist to be listed.
    pub fn with_extra_tracks(mut self, count: usize) -> Self {
        self.extra_tracks = count;
        self
    }

    /// Give the padding rows album art: a real PNG on disk for most of them,
    /// and a path that resolves to nothing for every `broken_every`-th row.
    ///
    /// Both halves matter. Art is what a real library has and the cost
    /// fixtures otherwise lack, and a *missing* file is the case worth pinning:
    /// if a failed decode is not cached, every frame re-reads a path that will
    /// never resolve, which is a per-frame cost that no amount of list
    /// virtualization removes. `0` disables the broken rows.
    pub fn with_album_art(mut self, broken_every: usize) -> Self {
        self.album_art = Some(broken_every);
        self
    }

    /// Pin the seeded library row's insertion time when a test needs to prove
    /// ordering against a later import without relying on wall-clock sleeps.
    pub fn with_track_created_at(mut self, created_at: impl Into<String>) -> Self {
        self.track_created_at = Some(created_at.into());
        self
    }

    /// Seed the library and open the app on it.
    ///
    /// Every knob this needs travels in the harness's [`Runtime`], so any
    /// number of fixtures may be open at once in one process — which is what
    /// lets a whole suite share a test binary.
    pub fn open(self, mode: Mode) -> Harness {
        let config_dir = self.seed();
        // The rig's bundle is written under the config directory by
        // `seed_rig`, so the app resolves the definition this fixture just
        // wrote rather than the developer's.
        let fixtures_root = (self.rig > 0).then(|| config_dir.join("fixtures"));
        let source_fixture = self.source_fixture.clone();
        let source_fixture_delay = self.source_fixture_delay;
        let source_search_responses = self.source_search_responses.clone();
        let source_import_fixture_delay = self.source_import_fixture_delay;
        let root: gpui_agent::RootFactory =
            Arc::new(move |window: &mut Window, cx: &mut App| -> AnyView {
                luma_app::init(cx);
                let mut library =
                    luma_app::Library::open().expect("failed to open the fixture library");
                if let Some(fixture) = source_fixture.clone() {
                    library.set_source_adapter_fixture(fixture);
                }
                if let Some(delay) = source_fixture_delay {
                    library.set_source_adapter_fixture_delay(delay);
                }
                if !source_search_responses.is_empty() {
                    library.set_source_search_fixture_responses(source_search_responses.clone());
                }
                if let Some(delay) = source_import_fixture_delay {
                    library.set_source_import_fixture_delay(delay);
                }
                let luma = cx.new(|cx| luma_app::Luma::new(library, cx));
                cx.new(|cx| gpui_component::Root::new(luma, window, cx).bordered(false))
                    .into()
            });
        let mut config = Config {
            mode,
            call_timeout: Duration::from_secs(120),
            runtime: Runtime {
                config_dir: Some(config_dir),
                fixtures_root,
                // A walk acts and then looks. With the panel slides running,
                // "looks" would land mid-transition and every geometry
                // assertion would be a race against a 200ms tween; snapped,
                // the frame after an action is the finished one. Tests that
                // shoot the slides themselves opt out with `with_motion`.
                reduced_motion: !self.force_motion,
                motion_scale: self.motion_scale.unwrap_or(1.0),
                // Left unset so `Harness::headless` answers from the mode.
                stage_gpu: None,
            },
            ..Config::default()
        };
        if let Some(window) = self.window {
            config.window_size = window;
        }
        Harness::headless(config, root).expect("failed to start the harness")
    }

    /// A seeded track's content hash — and it has to vary with the content.
    ///
    /// A track hash names the bytes, and both `luma::audio::cache` and
    /// `luma::eval::context` keep process-global decode caches keyed on it. So
    /// a fixed literal here is not a harmless stand-in: two fixtures of
    /// different lengths claiming one hash means whichever test decodes first
    /// serves its audio to every later test in the process, which surfaces as
    /// that test's own track being the wrong duration. [`wav`] is a pure
    /// function of `seconds`, so `seconds` is what distinguishes the bytes.
    ///
    /// `stem` keeps the two seeded tracks apart. They share one audio file, so
    /// a strict content hash would be equal for both — but the import matcher
    /// treats an equal hash as the same track, and these are seeded precisely
    /// to be two. Distinct stems cost one redundant decode and buy that.
    fn track_hash(&self, stem: &str) -> String {
        format!("{stem}-{}s", self.seconds)
    }

    /// A library of its own, so the run cannot see — or corrupt — the
    /// developer's. Named after the process so two runs never share one.
    fn seed(&self) -> PathBuf {
        let dir = config_dir(self.name);
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
        if self.seed_track {
            sqlx::query(
                "INSERT INTO tracks
                    (id, uid, track_hash, title, artist, duration_seconds, file_path, created_at)
                 VALUES (?, NULL, ?, ?, 'Nightliner', ?, ?,
                         COALESCE(?, CURRENT_TIMESTAMP))",
            )
            .bind(TRACK)
            .bind(self.track_hash("aurora"))
            .bind(TRACK_NAME)
            .bind(f64::from(self.seconds))
            .bind(audio.to_string_lossy().to_string())
            .bind(self.track_created_at.as_deref())
            .execute(pool)
            .await
            .expect("failed to seed the track");
            self.seed_beats(pool).await;
            if self.equal_timestamp_track {
                sqlx::query(
                    "INSERT INTO tracks
                        (id, uid, track_hash, title, artist, duration_seconds, file_path, created_at)
                     VALUES ('track-zulu', NULL, ?, 'Zulu', 'Nightliner', ?, ?,
                             COALESCE(?, CURRENT_TIMESTAMP))",
                )
                .bind(self.track_hash("zulu"))
                .bind(f64::from(self.seconds))
                .bind(audio.to_string_lossy().to_string())
                .bind(self.track_created_at.as_deref())
                .execute(pool)
                .await
                .expect("failed to seed the equal-timestamp track");
            }
        }
        // One PNG on disk, shared by every row that has art. Distinct files
        // would measure the decoder; one file measures the cache, which is the
        // question — a real library's covers are already decoded once each.
        let art_path = self.album_art.map(|_| {
            let path = config_dir.join("padding-art.png");
            std::fs::write(&path, padding_art()).expect("failed to write padding art");
            path.to_string_lossy().into_owned()
        });
        for index in 0..self.extra_tracks {
            let art = art_path.as_ref().map(|path| {
                let broken_every = self.album_art.unwrap_or(0);
                if broken_every > 0 && index % broken_every == 0 {
                    // Points nowhere on purpose. See `with_album_art`.
                    format!("{path}.missing-{index:05}")
                } else {
                    path.clone()
                }
            });
            sqlx::query(
                "INSERT INTO tracks
                    (id, uid, track_hash, title, artist, album, duration_seconds, file_path,
                     album_art_path, album_art_mime)
                 VALUES (?, NULL, ?, ?, ?, 'Padding', ?, ?, ?, ?)",
            )
            .bind(format!("track-pad-{index:05}"))
            .bind(self.track_hash(&format!("pad-{index}")))
            .bind(format!("Padding Track {index:05}"))
            .bind(format!("Padding Artist {:03}", index % 97))
            .bind(f64::from(self.seconds))
            .bind(audio.to_string_lossy().to_string())
            .bind(art.clone())
            .bind(art.map(|_| "image/png".to_string()))
            .execute(pool)
            .await
            .expect("failed to seed a padding track");
        }
        if self.rig > 0 {
            self.seed_rig(pool, config_dir).await;
        }

        // Then the score, through the seam — see the module docs.
        let state_db = luma_lib::database::local::state::init_state_db_at(config_dir)
            .await
            .expect("failed to open the fixture state database");
        luma_lib::database::local::auth::bootstrap_headless_admission(&db.0, &state_db.0)
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

        let score_id = if self.seed_track {
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
            Some(
                score["id"]
                    .as_str()
                    .expect("a created score has an id")
                    .to_string(),
            )
        } else {
            None
        };

        let mut scores: Vec<String> = score_id.iter().cloned().collect();
        for extra in 0..self.extra_scores {
            let score = call(
                &services,
                "create_score",
                json!({
                    "requestId": request_id(900 + extra),
                    "trackId": TRACK,
                    "venueId": VENUE,
                    "name": null,
                }),
            )
            .await;
            scores.push(
                score["id"]
                    .as_str()
                    .expect("a created score has an id")
                    .to_string(),
            );
        }

        if self.seeded_threads {
            for (index, score) in scores.iter().enumerate() {
                self.seed_thread(&services, score, index).await;
            }
        }

        // Lit patterns are minted once per `clip.pattern` key, so two clips
        // that name the same key share one pattern — which is what a test
        // about same-pattern multi-selection needs to exist at all.
        let mut lit_patterns: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        for (index, clip) in self.clips.iter().enumerate().filter(|_| self.seed_track) {
            // `create_pattern` mints its own id, so a lit clip's score row
            // names that one rather than `clip.pattern` — which for a lit clip
            // is only the request key that keeps re-seeding idempotent.
            let pattern = if clip.lit {
                match lit_patterns.get(&clip.pattern) {
                    Some(id) => id.clone(),
                    None => {
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
                        lit_patterns.insert(clip.pattern.clone(), id.clone());
                        id
                    }
                }
            } else {
                clip.pattern.clone()
            };
            call(
                &services,
                "create_track_score",
                json!({ "payload": {
                    "requestId": request_id(index + 1),
                    "scoreId": score_id.as_deref().expect("track fixture has a score"),
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

    /// One track-agent conversation about `score`, with a prompt and a reply
    /// in it.
    ///
    /// Written through the same seam the app resolves it by, so the scope the
    /// editor derives from the screen finds this thread rather than minting an
    /// empty one beside it.
    async fn seed_thread(
        &self,
        services: &luma_lib::dispatch::AppServices,
        score: &str,
        index: usize,
    ) {
        let thread = call(
            services,
            "agent_thread_create",
            json!({ "input": {
                "requestId": request_id(700 + index),
                "agentKind": "track_copilot",
                "subjectKind": "track",
                "subjectId": TRACK,
                "implementationId": null,
                "venueId": VENUE,
                "scoreId": score,
                "title": null,
                "parentThreadId": null,
                "parentCallId": null,
            }}),
        )
        .await;
        let thread_id = thread["id"].as_str().expect("a created thread has an id");
        let part = |text: String| json!([{ "type": "text", "text": text }]);
        call(
            services,
            "agent_thread_append_messages",
            json!({
                "threadId": thread_id,
                "input": {
                    "operationId": request_id(750 + index),
                    "expectedHeadMessageId": null,
                    // The prompt alone: an assistant row needs a prepared
                    // authored turn beside it (trigger 1811), and what a test
                    // wants from a seeded conversation is that it *has* words,
                    // not who said them.
                    "messages": [
                        { "id": null, "role": "user", "parts": part(seeded_prompt(index)) },
                    ],
                },
            }),
        )
        .await;
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
                    // One arg of each editable family the args sheet must
                    // host. Only `selection` is wired into the graph; the
                    // rest are schema for the strip to render and write.
                    "args": [{
                        "id": "selection",
                        "name": "selection",
                        "argType": "Selection",
                        "defaultValue": { "expression": "all" },
                    }, {
                        "id": "intensity",
                        "name": "intensity",
                        "argType": "Scalar",
                        "defaultValue": 1.0,
                    }, {
                        "id": "tint",
                        "name": "tint",
                        "argType": "Color",
                        "defaultValue": { "r": 255.0, "g": 0.0, "b": 0.0, "a": 1.0 },
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

        let count = self.rig;
        let span = (count as f64).max(1.0) * 0.6;
        let depth = if self.skewed_rig { SKEW_DEPTH_M } else { 0.0 };
        for i in 0..count {
            sqlx::query(
                "INSERT INTO fixtures (id, uid, venue_id, universe, address, num_channels,
                                       manufacturer, model, mode_name, fixture_path, label,
                                       pos_x, pos_y, pos_z, rot_x, rot_y, rot_z)
                 VALUES (?, NULL, ?, 1, ?, 8, 'Luma', 'Mover', 'Default', ?, ?, ?, ?, 3.0, 0.0, 0.0, 0.0)",
            )
            .bind(format!("fixture-{i}"))
            .bind(VENUE)
            .bind(i as i64 * 8 + 1)
            .bind(MOVER_PATH)
            .bind(format!("Mover {i}"))
            .bind((i as f64 / (count.max(2) - 1) as f64 - 0.5) * span)
            .bind(depth)
            .execute(pool)
            .await
            .expect("failed to patch a fixture");
        }

        // Two groups over that line, left half and right half. A picker needs
        // names to tick, and two disjoint halves are the smallest rig where a
        // union is observably not either arm of it.
        for (index, (group, name)) in [
            ("group-left", "left_movers"),
            ("group-right", "right_movers"),
        ]
        .into_iter()
        .enumerate()
        {
            sqlx::query(
                "INSERT INTO fixture_groups (id, uid, venue_id, name, display_order)
                 VALUES (?, NULL, ?, ?, ?)",
            )
            .bind(group)
            .bind(VENUE)
            .bind(name)
            .bind(index as i64)
            .execute(pool)
            .await
            .expect("failed to create a fixture group");
            let half = count.div_ceil(2);
            let members = if index == 0 { 0..half } else { half..count };
            for (order, fixture) in members.enumerate() {
                sqlx::query(
                    "INSERT INTO fixture_group_members
                         (id, fixture_id, group_id, head_index, display_order)
                     VALUES (?, ?, ?, -1, ?)",
                )
                .bind(format!("{group}-{fixture}"))
                .bind(format!("fixture-{fixture}"))
                .bind(group)
                .bind(order as i64)
                .execute(pool)
                .await
                .expect("failed to add a fixture to a group");
            }
        }

        if self.skewed_rig {
            return;
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

/// Where a named fixture's library lives.
///
/// Public because a test that has to seed something [`Fixture`] does not model
/// — a hand-made address collision, a second universe, an output binding —
/// writes it into this directory after [`Fixture::open`] and before its script
/// navigates. One spelling, so the writer and the reader cannot disagree about
/// which library they are talking about.
#[must_use]
pub fn config_dir(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("luma-gpui-{name}-{}", std::process::id()))
}

/// How far off the `y = 0` plane [`Fixture::with_skewed_rig`] patches its
/// movers, in metres. Public because a test that projects one of them has to
/// know where it put it.
pub const SKEW_DEPTH_M: f64 = 2.0;

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
pub fn wav(seconds: u32) -> Vec<u8> {
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

#[cfg(feature = "pixel")]
pub mod image;
pub mod session;

/// Reading `app.timings()` — shared so two suites cannot disagree about what a
/// percentile means.
///
/// These are the CPU half of a frame (scene build), never the GPU: see
/// `app.timings()` in `api.d.ts`.
pub mod cost {
    use serde_json::Value;

    /// Nearest-rank percentile. Sorts in place.
    pub fn percentile(sample: &mut Vec<f64>, fraction: f64) -> f64 {
        sample.sort_by(|a, b| a.partial_cmp(b).unwrap());
        if sample.is_empty() {
            return f64::NAN;
        }
        sample[(((sample.len() - 1) as f64) * fraction).round() as usize]
    }

    /// Print what the frames in `(from, to]` cost, and hand back the median.
    ///
    /// Half-open on purpose: callers mark a stretch with the frame number a
    /// command returned, and that frame belongs to the stretch before it.
    pub fn summarize(frames: &[Value], from: u64, to: u64, label: &str) -> f64 {
        let mut draw: Vec<f64> = Vec::new();
        let mut parked: Vec<f64> = Vec::new();
        for frame in frames {
            let number = frame["frame"].as_u64().unwrap();
            if number > from && number <= to {
                draw.push(frame["drawMs"].as_f64().unwrap());
                parked.push(frame["parkedMs"].as_f64().unwrap());
            }
        }
        let count = draw.len();
        let mean = draw.iter().sum::<f64>() / count.max(1) as f64;
        let p50 = percentile(&mut draw.clone(), 0.50);
        let p95 = percentile(&mut draw, 0.95);
        println!(
            "{label:<26} n={count:<4} drawMs mean={mean:6.2} p50={p50:6.2} p95={p95:6.2}  \
             parkedMs p50={:5.2}",
            percentile(&mut parked, 0.50)
        );
        p50
    }
}

/// A 64×64 PNG, built rather than embedded.
///
/// Fixtures that need album art need *valid* art — a file that fails to decode
/// would put every row on the failure path and quietly measure the wrong thing.
/// Hand-written bytes with a wrong CRC do exactly that, so the encoder is here:
/// stored-mode deflate needs no compressor, and the two checksums are short.
fn padding_art() -> Vec<u8> {
    const SIDE: u32 = 64;
    let mut raw = Vec::with_capacity((SIDE * (1 + SIDE * 3)) as usize);
    for y in 0..SIDE {
        raw.push(0); // filter: none
        for x in 0..SIDE {
            raw.extend_from_slice(&[(x * 4) as u8, (y * 4) as u8, 0x80]);
        }
    }

    let mut zlib = vec![0x78, 0x01];
    for (index, block) in raw.chunks(65_535).enumerate() {
        let last = (index + 1) * 65_535 >= raw.len();
        zlib.push(u8::from(last));
        zlib.extend_from_slice(&(block.len() as u16).to_le_bytes());
        zlib.extend_from_slice(&(!(block.len() as u16)).to_le_bytes());
        zlib.extend_from_slice(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for byte in &raw {
        a = (a + u32::from(*byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    zlib.extend_from_slice(&((b << 16) | a).to_be_bytes());

    let mut header = Vec::new();
    header.extend_from_slice(&SIDE.to_be_bytes());
    header.extend_from_slice(&SIDE.to_be_bytes());
    header.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit RGB, no interlace

    let mut png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    for (kind, body) in [(b"IHDR", header), (b"IDAT", zlib), (b"IEND", Vec::new())] {
        png.extend_from_slice(&(body.len() as u32).to_be_bytes());
        let mut chunk = kind.to_vec();
        chunk.extend_from_slice(&body);
        png.extend_from_slice(&chunk);
        png.extend_from_slice(&crc32(&chunk).to_be_bytes());
    }
    png
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}
