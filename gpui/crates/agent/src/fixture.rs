//! One seeded Luma library, and a harness pointed at it.
//!
//! Two forms, one seeding path: the Rust builder ([`Fixture::new`] and its
//! `with_*` methods) that the cargo suites use, and a JSON object with the same
//! options (`{ track: false, seconds: 8, clips: [...] }`) that a `.test.js`
//! file hands to `fixture(...)`. Both are this struct.
//!
//! # Why the fixture has real audio in it
//!
//! `host_load_track` decodes the file at `tracks.file_path` and
//! `get_track_waveform` renders its envelope from the same file, so a row
//! pointing at a missing path gives a screen with no waveform and — because a
//! failed load sets `Editor::error` — no canvas at all. Synthesized WAV is the
//! smallest thing both commands accept.
//!
//! Audio *output* is turned off in the fixture's settings: the transport runs
//! on a wall clock either way, and a test that needed a sound card would be
//! testing the machine.
//!
//! # Why the score goes through the seam
//!
//! The editor only ever writes a score as one whole `luma_patterns::Score`, so
//! the fixture seeds it the same way, through `apply_score_document`.

mod agent;
pub mod session;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui::{AnyView, App, AppContext as _, Window};
use luma_lib::storage::StorageRoot;
use luma_ui::runtime::Runtime;
use serde::{Deserialize, Deserializer};
use serde_json::{json, Value};
use sqlx::SqlitePool;

use crate::{Config, Harness, Mode};

pub const VENUE: &str = "venue-main";
pub const VENUE_NAME: &str = "Test Venue";
pub const TRACK: &str = "track-aurora";
pub const TRACK_NAME: &str = "Aurora";

/// How far off the `y = 0` plane [`Fixture::with_skewed_rig`] patches its
/// movers, in metres. Public because a test that projects one has to know.
pub const SKEW_DEPTH_M: f64 = 2.0;

/// Where [`Fixture::with_rig`]'s fixtures are patched from, relative to the
/// bundle root it writes.
pub const MOVER_PATH: &str = "Luma/Mover.qxf";

/// The grid [`Fixture::seed_beats`] lays down: 120 bpm, so two beats a second.
/// A clip is written in seconds and stored in beats.
const BEATS_PER_SECOND: f64 = 2.0;

/// What the `index`th seeded conversation asked. Distinct per score, so "which
/// conversation is on screen" is a question a snapshot can answer.
#[must_use]
pub fn seeded_prompt(index: usize) -> String {
    format!("Seeded question about score {index}")
}

/// One clip to put on the timeline, before the editor resolves it into a lane.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Clip {
    /// The clip's id in the score. A clip plays the Wash preset unless it
    /// names another.
    pub pattern: String,
    /// What the test calls the clip. The clip takes its preset's name.
    pub name: String,
    /// Seconds.
    pub start: f64,
    pub end: f64,
    #[serde(default, rename = "lane")]
    pub z_index: i64,
    /// A shipped clip preset to play instead of Wash, e.g. `"Chase"`.
    #[serde(default)]
    pub preset: Option<String>,
    #[serde(default)]
    pub seed: u64,
    /// The clip's selection expression, when not the whole venue.
    #[serde(default)]
    pub selection: Option<String>,
    /// A clip graph (`{"version": 3, "nodes": {...}}`) to play in place of
    /// the preset's.
    #[serde(default)]
    pub graph: Option<Value>,
}

impl Clip {
    pub fn new(pattern: impl Into<String>, name: impl Into<String>, start: f64, end: f64) -> Self {
        Self {
            pattern: pattern.into(),
            name: name.into(),
            start,
            end,
            z_index: 0,
            preset: None,
            seed: 0,
            selection: None,
            graph: None,
        }
    }

    pub fn lane(mut self, z_index: i64) -> Self {
        self.z_index = z_index;
        self
    }
}

/// A library with one venue, one track, one score, and clips on it.
///
/// The JSON form names each option after its builder method; durations are
/// `*_ms` integers and `window` is `[width, height]`.
#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Fixture {
    /// Keeps one test's config directory apart from another's: two tests
    /// seeding one directory would be one library with both their contents.
    /// Unique per test, not per file — many run side by side in one process.
    #[serde(skip)]
    name: String,
    seconds: u32,
    clips: Vec<Clip>,
    /// Patched movers. 0 is no rig: most fixtures test rows and geometry, and
    /// a rig would only be a slower seed.
    rig: usize,
    /// See [`Fixture::with_skewed_rig`].
    skewed_rig: bool,
    #[serde(rename = "track")]
    seed_track: bool,
    track_created_at: Option<String>,
    #[serde(deserialize_with = "window_size")]
    window: Option<gpui::Size<gpui::Pixels>>,
    /// Shared with the test that set it, so it has no JSON form.
    #[serde(skip)]
    sync_status: Option<Arc<std::sync::Mutex<luma_lib::models::sync::SyncStatus>>>,
    #[serde(deserialize_with = "source_fixture")]
    source_fixture: Option<luma_app::SourceAdapterFixture>,
    #[serde(rename = "source_fixture_delay_ms", deserialize_with = "millis")]
    source_fixture_delay: Option<Duration>,
    #[serde(deserialize_with = "search_responses")]
    source_search_responses: Vec<luma_app::SourceSearchFixtureResponse>,
    #[serde(rename = "source_import_fixture_delay_ms", deserialize_with = "millis")]
    source_import_fixture_delay: Option<Duration>,
    equal_timestamp_track: bool,
    second_track_seconds: Option<u32>,
    #[serde(rename = "motion")]
    force_motion: bool,
    motion_scale: Option<f32>,
    extra_tracks: usize,
    album_art: Option<usize>,
    extra_scores: usize,
    seeded_threads: bool,
    graph_score: Option<Value>,
    /// Statements run on the seeded library after its rows and before the
    /// score: what no option models (a second venue, a clashing patch, an
    /// output binding). Owned rows need `uid` = [`session::PRINCIPAL`] or
    /// admission refuses them; `$PRINCIPAL` in a statement is replaced by it.
    sql: Vec<String>,
    /// Files written into the library directory after the rows, by path
    /// relative to it: a fixture definition with a second mode over the rig's
    /// (`fixtures/Luma/Mover.qxf`), say.
    files: HashMap<String, String>,
    /// The agent's model, replayed: one array of events per step. See
    /// [`agent`] for the event shapes. Unset, the app's own model runs.
    #[serde(deserialize_with = "agent::steps")]
    model: Option<Vec<Vec<luma_lib::agent::model::ModelEvent>>>,
    /// The gap between scripted events, so a turn is observably mid-text.
    #[serde(rename = "model_cadence_ms", deserialize_with = "millis")]
    model_cadence: Option<Duration>,
    /// The agent's tools instead of its kind's own set: shipped ones by name,
    /// scripted ones as objects.
    tools: Option<Vec<agent::ToolEntry>>,
}

impl Default for Fixture {
    fn default() -> Self {
        Self {
            name: String::new(),
            seconds: 8,
            clips: Vec::new(),
            rig: 0,
            skewed_rig: false,
            seed_track: true,
            track_created_at: None,
            window: None,
            sync_status: None,
            source_fixture: None,
            source_fixture_delay: None,
            source_search_responses: Vec::new(),
            source_import_fixture_delay: None,
            equal_timestamp_track: false,
            second_track_seconds: None,
            force_motion: false,
            motion_scale: None,
            extra_tracks: 0,
            album_art: None,
            extra_scores: 0,
            seeded_threads: false,
            graph_score: None,
            sql: Vec::new(),
            files: HashMap::new(),
            model: None,
            model_cadence: None,
            tools: None,
        }
    }
}

impl Fixture {
    pub fn new(name: impl Into<String>, seconds: u32, clips: Vec<Clip>) -> Self {
        Self {
            name: name.into(),
            seconds,
            clips,
            ..Self::default()
        }
    }

    /// The JSON form, named `name`. Unknown keys are an error, so a misspelt
    /// option cannot quietly seed the default.
    pub fn from_json(name: impl Into<String>, spec: Value) -> Result<Self, String> {
        let mut fixture: Self = serde_json::from_value(spec).map_err(|error| error.to_string())?;
        if fixture.graph_score.is_some() && !fixture.clips.is_empty() {
            return Err("graph_score and clips are exclusive".into());
        }
        fixture.name = name.into();
        if let Some(tools) = &fixture.tools {
            agent::registry(tools, &StorageRoot::from_path(config_dir(&fixture.name)))?;
        }
        Ok(fixture)
    }

    /// Mint `count` further scores on the seeded `(track, venue)`, after the
    /// one the clips are authored on, through the same `create_score` seam.
    pub fn with_extra_scores(mut self, count: usize) -> Self {
        self.extra_scores = count;
        self
    }

    /// Give every score on the seeded pair a track-agent conversation with a
    /// message already in it, so a thread's *arrival* is observable.
    pub fn with_seeded_threads(mut self) -> Self {
        self.seeded_threads = true;
        self
    }

    /// Open the window at `width` × `height` instead of the default 1200×800.
    pub fn window(mut self, width: f32, height: f32) -> Self {
        self.window = Some(gpui::size(gpui::px(width), gpui::px(height)));
        self
    }

    pub fn with_graph_score(mut self, score: Value) -> Self {
        assert!(
            self.clips.is_empty(),
            "graph fixture uses its canonical document"
        );
        self.graph_score = Some(score);
        self
    }

    /// Patch a small rig into the venue and bundle the definition it needs,
    /// and point the harness's fixtures root at that bundle.
    pub fn with_rig(self) -> Self {
        self.with_rig_of(4)
    }

    /// `count` movers placed off the `y = 0` plane, and no deck.
    ///
    /// The default rig lies on the one plane the data→world mirror leaves
    /// fixed, so a screen that drew in the wrong space could look correct.
    pub fn with_skewed_rig(mut self, count: usize) -> Self {
        self.skewed_rig = true;
        self.with_rig_of(count)
    }

    /// Patch `count` movers instead of the default four. Rig size is the axis
    /// most renderer costs scale on.
    pub fn with_rig_of(mut self, count: usize) -> Self {
        self.rig = count;
        self
    }

    pub fn with_sync_status(
        mut self,
        status: Arc<std::sync::Mutex<luma_lib::models::sync::SyncStatus>>,
    ) -> Self {
        self.sync_status = Some(status);
        self
    }

    /// Raw DJ-adapter answers. The UI still crosses the production Library
    /// normalization and import seams; only the external database read is
    /// substituted.
    pub fn with_source_fixture(mut self, fixture: luma_app::SourceAdapterFixture) -> Self {
        self.source_fixture = Some(fixture);
        self
    }

    /// Hold the first normalized source read, for a loading-state snapshot.
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

    /// A second playable song with an observably different audio duration.
    pub fn with_second_track(mut self, seconds: u32) -> Self {
        self.equal_timestamp_track = true;
        self.second_track_seconds = Some(seconds);
        self
    }

    /// Force authored motion on for a test that measures a transition.
    pub fn with_motion(mut self) -> Self {
        self.force_motion = true;
        self
    }

    /// Stretch every timeline by `scale`. Only meaningful with
    /// [`Self::with_motion`].
    pub fn with_motion_scale(mut self, scale: f32) -> Self {
        self.motion_scale = Some(scale);
        self
    }

    /// Keep the venue but omit the library track and its score.
    pub fn without_track(mut self) -> Self {
        self.seed_track = false;
        self
    }

    /// Pad the library with `count` extra rows with no audio and no beats.
    pub fn with_extra_tracks(mut self, count: usize) -> Self {
        self.extra_tracks = count;
        self
    }

    /// Give the padding rows album art: one real PNG, and a path that resolves
    /// to nothing for every `broken_every`-th row (`0` disables those). A
    /// missing file is the case worth pinning: an uncached failed decode is a
    /// per-frame cost.
    pub fn with_album_art(mut self, broken_every: usize) -> Self {
        self.album_art = Some(broken_every);
        self
    }

    /// Pin the seeded track's insertion time, to prove ordering against a
    /// later import without wall-clock sleeps.
    pub fn with_track_created_at(mut self, created_at: impl Into<String>) -> Self {
        self.track_created_at = Some(created_at.into());
        self
    }

    /// Seed the library and open the app on it.
    ///
    /// Every knob travels in the harness's [`Runtime`], so any number of
    /// fixtures may be open at once in one process. The library goes with the
    /// harness when the test passes and stays, path printed, when it panics.
    pub fn open(self, mode: Mode) -> Harness {
        let dir = config_dir(&self.name);
        self.open_with(mode, Duration::from_secs(120))
            .expect("failed to start the harness")
            .remove_on_drop(dir)
    }

    /// [`Self::open`] with the pump's per-command deadline stated.
    pub fn open_with(
        self,
        mode: Mode,
        call_timeout: Duration,
    ) -> Result<Harness, crate::HarnessError> {
        let config_dir = self.seed();
        // `seed_rig` writes the bundle under the config directory, so the app
        // resolves the definition this fixture wrote rather than the
        // developer's.
        let fixtures_root = (self.rig > 0).then(|| config_dir.join("fixtures"));
        let sync_status = self.sync_status.clone();
        let source_fixture = self.source_fixture.clone();
        let source_fixture_delay = self.source_fixture_delay;
        let source_search_responses = self.source_search_responses.clone();
        let source_import_fixture_delay = self.source_import_fixture_delay;
        let model = self.model.clone();
        let model_cadence = self.model_cadence;
        let tools = self.tools.clone();
        let storage = StorageRoot::from_path(config_dir.clone());
        let root: crate::RootFactory =
            Arc::new(move |window: &mut Window, cx: &mut App| -> AnyView {
                luma_app::init(cx);
                let mut library =
                    luma_app::Library::open().expect("failed to open the fixture library");
                if let Some(status) = sync_status.clone() {
                    library.set_sync_status_fixture(status);
                }
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
                if let Some(steps) = &model {
                    library.set_agent_model(Arc::new(agent::model(steps, model_cadence)));
                }
                if let Some(tools) = &tools {
                    // Checked in `from_json`, so this cannot fail here.
                    library.set_agent_tools(
                        agent::registry(tools, &storage)
                            .expect("fixture tools"),
                    );
                }
                let luma = cx.new(|cx| luma_app::Luma::new(library, cx));
                cx.new(|cx| gpui_component::Root::new(luma, window, cx).bordered(false))
                    .into()
            });
        let mut config = Config {
            mode,
            call_timeout,
            runtime: Runtime {
                config_dir: Some(config_dir),
                fixtures_root,
                // Snapped, so the frame after an action is the finished one
                // and geometry is not a race against a 200ms tween. Tests that
                // shoot the slides themselves opt out with `with_motion`.
                reduced_motion: !self.force_motion,
                motion_scale: self.motion_scale.unwrap_or(1.0),
                cloud: false,
                // Left unset so `Harness::headless` answers from the mode.
                stage_gpu: None,
            },
            ..Config::default()
        };
        if let Some(window) = self.window {
            config.window_size = window;
        }
        Harness::headless(config, root)
    }

    /// A seeded track's content hash, which has to vary with the content.
    ///
    /// Two process-global decode caches key on it, so two fixtures of
    /// different lengths claiming one hash would serve each other's audio.
    /// [`wav`] is a pure function of `seconds`, so `seconds` names the bytes.
    /// `stem` keeps two seeded tracks apart: they share one file, and the
    /// import matcher treats an equal hash as the same track.
    fn track_hash(&self, stem: &str) -> String {
        format!("{stem}-{}s", self.seconds)
    }

    /// A library of its own, so the run cannot see — or corrupt — the
    /// developer's.
    fn seed(&self) -> PathBuf {
        let dir = config_dir(&self.name);
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("failed to create the temporary config directory");
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("failed to start the fixture runtime");
        // The `-shm` index stays behind: SQLite rebuilds it on open.
        let migrated = migrated(&runtime);
        for entry in std::fs::read_dir(migrated).expect("the migrated library is gone") {
            let entry = entry.expect("a readable migrated library");
            let name = entry.file_name();
            if !name.to_string_lossy().ends_with("-shm") {
                std::fs::copy(entry.path(), dir.join(&name))
                    .expect("failed to copy the migrated library");
            }
        }
        runtime.block_on(self.write(&dir));
        dir
    }

    async fn write(&self, config_dir: &Path) {
        let audio = config_dir.join("aurora.wav");
        std::fs::write(&audio, wav(self.seconds)).expect("failed to write the fixture audio");

        // Rows first, while admission is still unarmed.
        let db = luma_lib::database::local::database::init_app_db_at(config_dir)
            .await
            .expect("failed to open the fixture database");
        let pool = &db.0;
        sqlx::query("INSERT INTO venues (id, uid, name) VALUES (?, ?, ?)")
            .bind(VENUE)
            .bind(session::PRINCIPAL)
            .bind(VENUE_NAME)
            .execute(pool)
            .await
            .expect("failed to seed the venue");
        if self.seed_track {
            sqlx::query(
                "INSERT INTO tracks
                    (id, uid, track_hash, title, artist, duration_seconds, file_path, created_at)
                 VALUES (?, ?, ?, ?, 'Nightliner', ?, ?,
                         COALESCE(?, CURRENT_TIMESTAMP))",
            )
            .bind(TRACK)
            .bind(session::PRINCIPAL)
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
                let seconds = self.second_track_seconds.unwrap_or(self.seconds);
                let second_audio = config_dir.join("zulu.wav");
                std::fs::write(&second_audio, wav(seconds)).unwrap();
                sqlx::query(
                    "INSERT INTO tracks
                        (id, uid, track_hash, title, artist, duration_seconds, file_path, created_at)
                     VALUES ('track-zulu', ?, ?, 'Zulu', 'Nightliner', ?, ?,
                             COALESCE(?, CURRENT_TIMESTAMP))",
                )
                .bind(session::PRINCIPAL)
                .bind(format!("zulu-{seconds}s"))
                .bind(f64::from(seconds))
                .bind(second_audio.to_string_lossy().to_string())
                .bind(self.track_created_at.as_deref())
                .execute(pool)
                .await
                .expect("failed to seed the equal-timestamp track");
            }
        }
        // One PNG shared by every row with art: one file measures the cache,
        // which is the question.
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
                 VALUES (?, ?, ?, ?, ?, 'Padding', ?, ?, ?, ?)",
            )
            .bind(format!("track-pad-{index:05}"))
            .bind(session::PRINCIPAL)
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
        for (path, contents) in &self.files {
            let path = config_dir.join(path);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .expect("failed to create a fixture file's directory");
            }
            std::fs::write(&path, contents).expect("failed to write a fixture file");
        }
        for statement in &self.sql {
            sqlx::query(sqlx::AssertSqlSafe(
                statement.replace("$PRINCIPAL", session::PRINCIPAL),
            ))
            .execute(pool)
            .await
            .unwrap_or_else(|error| panic!("fixture sql failed: {error}\n{statement}"));
        }

        // Then the score, through the seam.
        session::signed_in(config_dir).await;
        let state_db = luma_lib::database::local::state::init_state_db_at(config_dir)
            .await
            .expect("failed to open the fixture state database");
        luma_lib::database::local::auth::bootstrap_headless_admission(&db.0, &state_db.0)
            .await
            .expect("failed to arm admission");
        // A patched venue is not yet a room: its graph is built the first
        // time a venue is opened, and previews and playback evaluate against
        // it. Here because the conversion needs admission armed, and for
        // every fixture because an empty venue still has a root. `migrate`,
        // not `ensure_migrated`: the latter also snapshots derived groups,
        // which would hand every picker groups no test asked for.
        {
            use luma_lib::database::local::venue_access::{VenueAccess, VenueResource, Write};
            let mut access = VenueAccess::<Write>::write(&db.0, VenueResource::Venue(VENUE))
                .await
                .expect("failed to open the fixture venue");
            luma_lib::venue_graph::migrate(&mut access, &config_dir.join("fixtures"))
                .await
                .expect("failed to build the venue graph");
            luma_lib::venue_graph::commit_graph(access)
                .await
                .expect("failed to commit the venue graph");
        }
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
                    "requestId": request_id(0), "trackId": TRACK,
                    "venueId": VENUE, "name": "Fixture Score",
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

        let document = match &self.graph_score {
            Some(document) => Some(document.clone()),
            None if self.clips.is_empty() => None,
            None => Some(self.timeline()),
        };
        if let Some(document) = document {
            let score_id = score_id.as_ref().expect("a score fixture has a score");
            call(
                &services,
                "apply_score_document",
                json!({ "scoreId": score_id, "score": document }),
            )
            .await;
        }
    }

    /// One clip per entry, keyed by its pattern. Times are beats: the
    /// seeded grid is 120 bpm, so a beat is half a second.
    fn timeline(&self) -> Value {
        let mut clips = serde_json::Map::new();
        for clip in &self.clips {
            let preset = clip.preset.as_deref().unwrap_or("Wash");
            let mut placed = luma_patterns::presets()
                .clip(preset)
                .unwrap_or_else(|| panic!("no shipped preset {preset}"))
                .clip(
                    clip.start * BEATS_PER_SECOND,
                    (clip.end - clip.start) * BEATS_PER_SECOND,
                );
            placed.seed = clip.seed;
            if let Some(expression) = &clip.selection {
                placed.selection = serde_json::from_value(json!({ "expression": expression }))
                    .expect("a selection expression");
            }
            placed.z_index = clip.z_index;
            let mut placed = serde_json::to_value(placed).expect("a serializable clip");
            if let Some(graph) = &clip.graph {
                placed["graph"] = graph.clone();
            }
            clips.insert(clip.pattern.clone(), placed);
        }
        json!({ "clips": clips })
    }

    /// One track-agent conversation about `score`, written through the seam
    /// the app resolves it by, so the editor finds this thread rather than
    /// minting an empty one beside it.
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
                    // authored turn beside it (trigger 1811).
                    "messages": [
                        { "id": null, "role": "user", "parts": part(seeded_prompt(index)) },
                    ],
                },
            }),
        )
        .await;
    }

    /// Movers in a row and a deck under them, plus the QLC+ definition they
    /// name — written here so a renamed shipped definition cannot break a
    /// test of the view.
    async fn seed_rig(&self, pool: &SqlitePool, config_dir: &Path) {
        let bundle = config_dir.join("fixtures");
        std::fs::create_dir_all(bundle.join("Luma")).expect("failed to create the fixture bundle");
        std::fs::write(bundle.join("Luma/Mover.qxf"), MOVER_QXF)
            .expect("failed to write the fixture definition");

        let count = self.rig;
        let span = (count as f64).max(1.0) * 0.6;
        let depth = if self.skewed_rig { SKEW_DEPTH_M } else { 0.0 };
        for i in 0..count {
            // Sixty-four eight-channel fixtures fill a universe, and the
            // addressing migration refuses a footprint that leaves one, so a
            // big rig rolls to the next universe the way the allocator does.
            let universe = i as i64 / FIXTURES_PER_UNIVERSE + 1;
            let address = (i as i64 % FIXTURES_PER_UNIVERSE) * 8 + 1;
            sqlx::query(
                "INSERT INTO fixtures (id, uid, venue_id, universe, address, num_channels,
                                       manufacturer, model, mode_name, fixture_path, label,
                                       pos_x, pos_y, pos_z, rot_x, rot_y, rot_z)
                 VALUES (?, ?, ?, ?, ?, 8, 'Luma', 'Mover', 'Default', ?, ?, ?, ?, 3.0, 0.0, 0.0, 0.0)",
            )
            .bind(format!("fixture-{i}"))
            .bind(session::PRINCIPAL)
            .bind(VENUE)
            .bind(universe)
            .bind(address)
            .bind(MOVER_PATH)
            .bind(format!("Mover {i}"))
            .bind((i as f64 / (count.max(2) - 1) as f64 - 0.5) * span)
            .bind(depth)
            .execute(pool)
            .await
            .expect("failed to patch a fixture");
        }

        // Two groups, left half and right half: the smallest rig where a
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
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(group)
            .bind(session::PRINCIPAL)
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
             VALUES ('piece-deck', ?, ?, 'stage_lab/stage_praticavel_2x1x1.glb', 'floor', 'Deck',
                     0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0)",
        )
        .bind(session::PRINCIPAL)
        .bind(VENUE)
        .execute(pool)
        .await
        .expect("failed to place the stage piece");
    }

    /// A steady 120 bpm grid in 4/4 over the whole track, so the editor draws
    /// bars rather than falling back to the clock ruler.
    async fn seed_beats(&self, pool: &SqlitePool) {
        let beats: Vec<f64> = (0..self.seconds * 2)
            .map(|index| f64::from(index) * 0.5)
            .collect();
        let downbeats: Vec<f64> = beats.iter().copied().step_by(4).collect();
        sqlx::query(
            "INSERT INTO track_beats (track_id, uid, beats_json, downbeats_json, bpm, downbeat_offset, beats_per_bar)
             VALUES (?, ?, ?, ?, 120.0, 0.0, 4)",
        )
        .bind(TRACK)
        .bind(session::PRINCIPAL)
        .bind(serde_json::to_string(&beats).unwrap())
        .bind(serde_json::to_string(&downbeats).unwrap())
        .execute(pool)
        .await
        .expect("failed to seed the beat grid");
    }
}

/// Where a named fixture's library lives.
///
/// Public because a test that seeds something [`Fixture`] does not model
/// writes it here after [`Fixture::open`] and before its script navigates.
#[must_use]
pub fn config_dir(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("luma-gpui-{name}-{}", std::process::id()))
}

/// What a test script may read back from its library: `{"op": "query",
/// "sql": …}` gives rows as objects (a blob as its length), `{"op": "score"}`
/// the score that holds clips as the document the editor saved,
/// `{"op": "presets"}` the shipped clip presets, `{"op": "curves"}` the named
/// curve shapes and `{"op": "gradients"}` the named gradients. Read-only: a
/// test writes through the app.
pub fn read_library(dir: &Path, request: &str) -> Result<String, String> {
    let request: Value = serde_json::from_str(request).map_err(|error| error.to_string())?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    let url = format!("sqlite:{}?mode=ro", dir.join("luma.db").display());
    runtime.block_on(async {
        let pool = SqlitePool::connect(&url)
            .await
            .map_err(|error| error.to_string())?;
        let out = match request["op"].as_str() {
            Some("query") => {
                let sql = request["sql"].as_str().ok_or("query needs sql")?;
                query_rows(&pool, sql).await
            }
            Some("score") => {
                let mut connection = pool.acquire().await.map_err(|error| error.to_string())?;
                let id: String = sqlx::query_scalar("SELECT score_id FROM clips LIMIT 1")
                    .fetch_one(&mut *connection)
                    .await
                    .map_err(|error| format!("no score holds a clip: {error}"))?;
                let score =
                    luma_lib::database::local::scores::rows::load_score(&mut connection, &id)
                        .await
                        .map_err(|error| error.to_string())?;
                serde_json::to_value(score).map_err(|error| error.to_string())
            }
            // The shipped catalogue a placed clip copies from, in menu order.
            Some("presets") => serde_json::to_value(&luma_patterns::presets().clips)
                .map_err(|error| error.to_string()),
            // As shipped, values 0–1: a curve's low and high scale them.
            Some("curves") => serde_json::to_value(&luma_patterns::presets().curves)
                .map_err(|error| error.to_string()),
            Some("gradients") => serde_json::to_value(&luma_patterns::presets().gradients)
                .map_err(|error| error.to_string()),
            other => Err(format!("unknown library op {other:?}")),
        };
        pool.close().await;
        out.map(|value| value.to_string())
    })
}

async fn query_rows(pool: &SqlitePool, sql: &str) -> Result<Value, String> {
    use sqlx::{Column as _, Row as _, TypeInfo as _, ValueRef as _};
    // A test's own statement against its own disposable library.
    let rows = sqlx::query(sqlx::AssertSqlSafe(sql.to_string()))
        .fetch_all(pool)
        .await
        .map_err(|error| error.to_string())?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let mut object = serde_json::Map::new();
        for (index, column) in row.columns().iter().enumerate() {
            let raw = row.try_get_raw(index).map_err(|error| error.to_string())?;
            let value = if raw.is_null() {
                Value::Null
            } else {
                match raw.type_info().name() {
                    "INTEGER" => json!(row.try_get::<i64, _>(index).map_err(|e| e.to_string())?),
                    "REAL" => json!(row.try_get::<f64, _>(index).map_err(|e| e.to_string())?),
                    "BLOB" => json!(row
                        .try_get::<Vec<u8>, _>(index)
                        .map_err(|e| e.to_string())?
                        .len()),
                    _ => json!(row.try_get::<String, _>(index).map_err(|e| e.to_string())?),
                }
            };
            object.insert(column.name().to_string(), value);
        }
        out.push(Value::Object(object));
    }
    Ok(Value::Array(out))
}

/// A migrated, empty library to start every seed from.
///
/// Migrating is nearly all of a seed — about two seconds alone, and several
/// times that side by side, because parallel migrations contend inside
/// SQLite. Copying the migrated file takes milliseconds, and
/// `init_app_db_at` on the copy finds nothing left to apply.
///
/// Kept across processes, keyed by this executable: the migrations are
/// compiled in, so a rebuilt binary is the only thing that can change them.
fn migrated(runtime: &tokio::runtime::Runtime) -> &'static Path {
    use std::hash::{Hash as _, Hasher as _};
    static MIGRATED: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    MIGRATED.get_or_init(|| {
        let mut key = std::collections::hash_map::DefaultHasher::new();
        if let Ok(exe) = std::env::current_exe() {
            exe.hash(&mut key);
            if let Ok(meta) = std::fs::metadata(&exe) {
                meta.len().hash(&mut key);
                meta.modified().ok().hash(&mut key);
            }
        }
        let base = std::env::temp_dir().join(format!("luma-gpui-migrated-{:x}", key.finish()));
        if base.join("luma.db").exists() {
            return base;
        }
        // Built beside the final name and renamed into it, so a process that
        // finds the directory finds it whole.
        let building = config_dir("migrating");
        std::fs::remove_dir_all(&building).ok();
        std::fs::create_dir_all(&building).expect("failed to create the migrated library");
        runtime.block_on(async {
            let db = luma_lib::database::local::database::init_app_db_at(&building)
                .await
                .expect("failed to migrate the base library");
            db.0.close().await;
        });
        if std::fs::rename(&building, &base).is_err() {
            // Another process got there first; its copy is as good.
            std::fs::remove_dir_all(&building).ok();
        }
        base
    })
}

// -- the JSON form's value shapes ----------------------------------------------

fn millis<'de, D: Deserializer<'de>>(de: D) -> Result<Option<Duration>, D::Error> {
    Ok(Option::<u64>::deserialize(de)?.map(Duration::from_millis))
}

fn window_size<'de, D: Deserializer<'de>>(
    de: D,
) -> Result<Option<gpui::Size<gpui::Pixels>>, D::Error> {
    Ok(Option::<[f32; 2]>::deserialize(de)?
        .map(|[width, height]| gpui::size(gpui::px(width), gpui::px(height))))
}

/// `luma_app::SourceAdapterFixture` is not `Deserialize` — it is the app's
/// type, kept at the JSON boundary on purpose — so its fields are read here.
fn source_fixture<'de, D: Deserializer<'de>>(
    de: D,
) -> Result<Option<luma_app::SourceAdapterFixture>, D::Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Raw {
        #[serde(default)]
        library: Value,
        #[serde(default)]
        playlists: Value,
        #[serde(default)]
        tracks: Value,
        #[serde(default)]
        playlist_tracks: HashMap<String, Value>,
        #[serde(default)]
        searches: HashMap<String, Value>,
    }
    Ok(
        Option::<Raw>::deserialize(de)?.map(|raw| luma_app::SourceAdapterFixture {
            library: raw.library,
            playlists: raw.playlists,
            tracks: raw.tracks,
            playlist_tracks: raw.playlist_tracks,
            searches: raw.searches,
        }),
    )
}

fn search_responses<'de, D: Deserializer<'de>>(
    de: D,
) -> Result<Vec<luma_app::SourceSearchFixtureResponse>, D::Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Raw {
        query: String,
        #[serde(default)]
        delay_ms: u64,
        rows: Value,
    }
    Ok(Vec::<Raw>::deserialize(de)?
        .into_iter()
        .map(|raw| luma_app::SourceSearchFixtureResponse {
            query: raw.query,
            delay: Duration::from_millis(raw.delay_ms),
            rows: raw.rows,
        })
        .collect())
}

// -- seeding details ---------------------------------------------------------

/// How many eight-channel movers fit in one DMX universe.
const FIXTURES_PER_UNIVERSE: i64 = 512 / 8;

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
/// store validates these as UUIDs; fixed, so re-seeding replays.
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
/// A slow amplitude sweep over a 220 Hz tone rather than silence: a flat
/// signal gives the waveform renderer three bands of zero to draw.
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

/// A 64×64 PNG, built rather than embedded: hand-written bytes with a wrong
/// CRC would put every row on the decode-failure path. Stored-mode deflate
/// needs no compressor.
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
