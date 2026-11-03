//! Live follow: decks playing in DJ software drive the matching scores.
//!
//! One [`PerformSession`] follows one source at a time. Each source reports
//! its decks as [`Reading`]s; the session matches each deck's track against
//! the library, compiles that track's scores for the venue into the deck's
//! render layer, and hands the render engine a [`clock::DeckClock`] per deck so the
//! lights keep moving between reports. Nothing here needs a UI: the Perform
//! screen only reads [`PerformSession::state`] and sets the few controls.

pub(crate) mod clock;
mod matching;
mod rkbx;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sqlx::SqlitePool;
use tokio::task::JoinHandle;

use crate::database::local::venue_access::{Read, VenueAccess, VenueResource};
use crate::models::perform::{DeckMatch, PerformDeck, PerformSource, PerformState};
use crate::render_engine::{PerformDeckInput, RenderEngine, SceneTarget};
use crate::storage::StorageRoot;
use clock::DeckFollower;

/// rkbx_link sends a new track's title and artist as separate messages, and
/// its BPM just after: wait for the burst to finish before matching.
const MATCH_AFTER: Duration = Duration::from_millis(150);
const TICK: Duration = Duration::from_millis(25);

/// What a source says a deck has loaded.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LoadedTrack {
    pub title: String,
    pub artist: String,
    /// The file's path on the player, when the source names it (StageLinQ).
    pub network_path: Option<String>,
    pub duration: Option<f64>,
    /// The track's unpitched BPM, when known.
    pub bpm: Option<f64>,
}

impl LoadedTrack {
    /// Whether `other` is the same track. Tempo and duration can arrive after
    /// the track itself, so they are not part of its identity.
    fn same(&self, other: &Self) -> bool {
        self.title == other.title
            && self.artist == other.artist
            && self.network_path == other.network_path
    }
}

/// One deck as a source reported it.
#[derive(Clone, Debug)]
pub(crate) struct Reading {
    /// Numbered from 1.
    pub deck: u8,
    pub track: Option<LoadedTrack>,
    /// A new observation of the playhead, in track seconds, or `None` when
    /// this reading says nothing about it.
    pub position: Option<f64>,
    /// `None` for a source that never says; play is then inferred.
    pub playing: Option<bool>,
    pub bpm: f64,
    /// Playback speed relative to the track's own tempo.
    pub rate: f64,
    pub volume: f64,
}

struct Deck {
    track: Option<LoadedTrack>,
    follower: DeckFollower,
    bpm: f64,
    volume: f64,
    matched: DeckMatch,
    /// When to start matching, once the source has settled on a track.
    match_due: Option<Instant>,
    /// Bumped on every track change, so a late match for the last one is
    /// dropped.
    generation: u64,
}

impl Default for Deck {
    fn default() -> Self {
        Self {
            track: None,
            follower: DeckFollower::default(),
            bpm: 0.0,
            volume: 1.0,
            matched: DeckMatch::Unknown,
            match_due: None,
            generation: 0,
        }
    }
}

struct State {
    /// Bumped on every start and stop, so a stopped source's late callbacks
    /// and matches change nothing.
    run: u64,
    source: Option<PerformSource>,
    venue_id: Option<String>,
    connected: bool,
    detail: String,
    problem: Option<String>,
    last_heard: Option<Instant>,
    saw_position: bool,
    latency_ms: f32,
    decks: BTreeMap<u8, Deck>,
}

struct Shared {
    state: Mutex<State>,
    render: RenderEngine,
    pool: SqlitePool,
    storage: StorageRoot,
    fixtures_root: PathBuf,
}

enum Source {
    Rkbx(rkbx::Listener),
    ProDjLink(prodjlink::ProDJLinkClient),
    StageLinq(stagelinq::StageLinqClient),
}

struct Running {
    source: Source,
    ticker: JoinHandle<()>,
}

/// Follows one DJ source at a time. Cheap to share: clone the `Arc` it lives
/// in.
pub struct PerformSession {
    shared: Arc<Shared>,
    running: tokio::sync::Mutex<Option<Running>>,
}

impl PerformSession {
    pub(crate) fn new(
        render: RenderEngine,
        pool: SqlitePool,
        storage: StorageRoot,
        fixtures_root: PathBuf,
    ) -> Self {
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State {
                    run: 0,
                    source: None,
                    venue_id: None,
                    connected: false,
                    detail: String::new(),
                    problem: None,
                    last_heard: None,
                    saw_position: false,
                    latency_ms: 0.0,
                    decks: BTreeMap::new(),
                }),
                render,
                pool,
                storage,
                fixtures_root,
            }),
            running: tokio::sync::Mutex::new(None),
        }
    }

    /// Follow `source` for `venue_id`, replacing whatever was followed before.
    pub async fn start(&self, venue_id: String, source: PerformSource) -> Result<(), String> {
        VenueAccess::<Read>::read(&self.shared.pool, VenueResource::Venue(&venue_id))
            .await
            .map_err(|error| error.to_string())?;
        let mut running = self.running.lock().await;
        self.shutdown(running.take()).await;

        let run = {
            let mut state = self.shared.lock();
            state.run += 1;
            state.source = Some(source.clone());
            state.venue_id = Some(venue_id);
            state.detail = "Starting".into();
            state.run
        };
        let started = match &source {
            PerformSource::RkbxLink { address } => {
                let address = address.as_deref().unwrap_or(rkbx::DEFAULT_ADDRESS);
                let shared = self.shared.clone();
                rkbx::listen(address, move |reading, at| shared.ingest(run, reading, at))
                    .await
                    .map(|listener| {
                        let mut state = self.shared.lock();
                        state.connected = true;
                        state.detail = format!("Listening on {}", listener.address);
                        Source::Rkbx(listener)
                    })
            }
            PerformSource::ProDjLink { device_num } => {
                let shared = self.shared.clone();
                prodjlink::ProDJLinkClient::start(*device_num, move |event| {
                    shared.prodjlink(run, event)
                })
                .await
                .map(Source::ProDjLink)
            }
            PerformSource::StageLinq => {
                let shared = self.shared.clone();
                stagelinq::StageLinqClient::start(move |event| shared.stagelinq(run, event))
                    .await
                    .map(Source::StageLinq)
            }
        };
        let source = match started {
            Ok(source) => source,
            Err(error) => {
                self.shared.reset();
                return Err(error);
            }
        };
        let shared = self.shared.clone();
        let ticker = tokio::spawn(async move {
            let mut ticks = tokio::time::interval(TICK);
            ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticks.tick().await;
                Shared::tick(&shared, run);
            }
        });
        *running = Some(Running { source, ticker });
        Ok(())
    }

    /// Stop following and clear every deck's lights. Idempotent.
    pub async fn stop(&self) {
        let mut running = self.running.lock().await;
        self.shutdown(running.take()).await;
    }

    async fn shutdown(&self, running: Option<Running>) {
        self.shared.reset();
        let Some(running) = running else {
            return;
        };
        running.ticker.abort();
        match running.source {
            Source::Rkbx(listener) => drop(listener),
            Source::ProDjLink(client) => client.stop().await,
            Source::StageLinq(client) => client.stop().await,
        }
    }

    pub fn set_latency(&self, latency_ms: f32) {
        let latency_ms = if latency_ms.is_finite() {
            latency_ms.clamp(-1000.0, 1000.0)
        } else {
            0.0
        };
        self.shared.lock().latency_ms = latency_ms;
        self.shared.render.set_perform_latency(latency_ms / 1000.0);
    }

    /// Pin `deck` to `track_id`, or with `None`, match it automatically again.
    /// A pinned track holds until the deck loads another.
    pub fn assign(&self, deck: u8, track_id: Option<String>) -> Result<(), String> {
        let mut state = self.shared.lock();
        let run = state.run;
        let venue_id = state.venue_id.clone().ok_or("Live follow is off")?;
        let entry = state.decks.get_mut(&deck).ok_or("No such deck")?;
        entry.generation += 1;
        entry.matched = DeckMatch::Matching;
        match track_id {
            Some(track_id) => {
                entry.match_due = None;
                let job = Job {
                    run,
                    deck,
                    generation: entry.generation,
                    venue_id,
                    track: entry.track.clone(),
                    pinned: Some(track_id),
                };
                tokio::spawn(resolve(self.shared.clone(), job));
            }
            None => entry.match_due = Some(Instant::now()),
        }
        Ok(())
    }

    /// Everything live follow knows, extrapolated to now.
    pub fn state(&self) -> PerformState {
        let state = self.shared.lock();
        let Some(source) = state.source.clone() else {
            return PerformState::off(state.latency_ms);
        };
        let now = Instant::now();
        let problem = state.problem.clone().or_else(|| {
            match source {
            PerformSource::RkbxLink { .. } if state.last_heard.is_none() => Some(
                "Nothing heard from rkbx_link yet. Check it is running and sending OSC here."
                    .into(),
            ),
            PerformSource::RkbxLink { .. } if !state.saw_position => Some(
                "rkbx_link is not sending deck positions. Set osc.msg.n/time to true in its config."
                    .into(),
            ),
            _ => None,
        }
        });
        PerformState {
            source: Some(source),
            venue_id: state.venue_id.clone(),
            connected: state.connected,
            detail: state.detail.clone(),
            last_heard_ms: state
                .last_heard
                .map(|at| now.saturating_duration_since(at).as_millis() as u64),
            problem,
            latency_ms: state.latency_ms,
            decks: state
                .decks
                .iter()
                .map(|(id, deck)| {
                    let clock = deck.follower.clock();
                    PerformDeck {
                        id: *id,
                        title: deck
                            .track
                            .as_ref()
                            .map(|t| t.title.clone())
                            .unwrap_or_default(),
                        artist: deck
                            .track
                            .as_ref()
                            .map(|t| t.artist.clone())
                            .unwrap_or_default(),
                        bpm: deck.bpm,
                        position: clock.map_or(0.0, |clock| clock.position_at(now)),
                        playing: clock.is_some_and(|clock| clock.playing()),
                        track: deck.matched.clone(),
                    }
                })
                .collect(),
        }
    }
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().expect("perform state poisoned")
    }

    /// Forget the source and its decks, and clear their lights.
    fn reset(&self) {
        let mut state = self.lock();
        state.run += 1;
        state.source = None;
        state.venue_id = None;
        state.connected = false;
        state.detail.clear();
        state.problem = None;
        state.last_heard = None;
        state.saw_position = false;
        state.decks.clear();
        self.render.clear_perform();
    }

    /// Take one reading. `None` says only that the source was heard.
    fn ingest(&self, run: u64, reading: Option<Reading>, at: Instant) {
        let mut state = self.lock();
        if state.run != run {
            return;
        }
        state.last_heard = Some(at);
        let Some(reading) = reading else {
            return;
        };
        state.saw_position |= reading.position.is_some();
        let deck = state.decks.entry(reading.deck).or_default();
        let changed = match (&deck.track, &reading.track) {
            (Some(old), Some(new)) => !old.same(new),
            (None, None) => false,
            _ => true,
        };
        if changed {
            deck.generation += 1;
            deck.follower.reset();
            deck.matched = if reading.track.is_some() {
                DeckMatch::Matching
            } else {
                DeckMatch::Unknown
            };
            deck.match_due = reading.track.is_some().then_some(at + MATCH_AFTER);
            self.render.clear_deck(reading.deck);
        } else if deck.match_due.is_some() {
            // Still settling: wait for the rest of the burst.
            deck.match_due = Some(at + MATCH_AFTER);
        }
        deck.track = reading.track;
        deck.bpm = reading.bpm;
        deck.volume = reading.volume;
        if let Some(position) = reading.position {
            deck.follower
                .observe(position, reading.rate, reading.playing, at);
        }
        self.push(&state);
    }

    fn tick(shared: &Arc<Self>, run: u64) {
        let now = Instant::now();
        let mut state = shared.lock();
        if state.run != run {
            return;
        }
        let venue_id = state.venue_id.clone().unwrap_or_default();
        let mut stopped = false;
        for (id, deck) in &mut state.decks {
            stopped |= deck.follower.tick(now);
            if deck.match_due.is_some_and(|due| due <= now) {
                deck.match_due = None;
                let job = Job {
                    run,
                    deck: *id,
                    generation: deck.generation,
                    venue_id: venue_id.clone(),
                    track: deck.track.clone(),
                    pinned: None,
                };
                tokio::spawn(resolve(shared.clone(), job));
            }
        }
        if stopped {
            shared.push(&state);
        }
    }

    /// Hand the engine every deck's clock. Only playing decks blend in: no
    /// source here reports its mixer channel reliably, so a paused deck is
    /// the only one known to be silent.
    fn push(&self, state: &State) {
        let inputs = state
            .decks
            .iter()
            .filter_map(|(id, deck)| {
                let clock = deck.follower.clock()?;
                Some(PerformDeckInput {
                    deck_id: *id,
                    clock,
                    volume: if clock.playing() {
                        deck.volume as f32
                    } else {
                        0.0
                    },
                })
            })
            .collect();
        self.render.set_perform_deck_states(inputs);
    }

    fn prodjlink(&self, run: u64, event: prodjlink::ProDJLinkEvent) {
        use prodjlink::ProDJLinkEvent as Event;
        let now = Instant::now();
        match event {
            Event::Discovered { name, .. } => self.status(run, None, Some(format!("Found {name}"))),
            Event::Connected => {
                self.status(run, Some(true), Some("Connected to Pro DJ Link".into()))
            }
            Event::Disconnected => self.status(
                run,
                Some(false),
                Some("Disconnected from Pro DJ Link".into()),
            ),
            Event::Error { message } => self.problem(run, message),
            Event::StateChanged(snapshot) => {
                for deck in snapshot.decks {
                    // Title and artist arrive in a metadata fetch after the
                    // track loads; until then the deck has nothing to match.
                    let known = deck.rekordbox_id != 0
                        && !(deck.title.is_empty() && deck.artist.is_empty());
                    let reading = Reading {
                        deck: deck.player,
                        track: known.then(|| LoadedTrack {
                            title: deck.title,
                            artist: deck.artist,
                            network_path: None,
                            duration: (deck.duration_secs > 0)
                                .then_some(f64::from(deck.duration_secs)),
                            bpm: (deck.bpm > 0.0).then_some(deck.bpm),
                        }),
                        position: Some(f64::from(deck.position_ms) / 1000.0),
                        playing: Some(deck.playing),
                        bpm: deck.effective_bpm,
                        rate: if deck.bpm > 0.0 {
                            deck.effective_bpm / deck.bpm
                        } else {
                            1.0
                        },
                        volume: 1.0,
                    };
                    self.ingest(run, Some(reading), now);
                }
            }
        }
    }

    fn stagelinq(&self, run: u64, event: stagelinq::DeckEvent) {
        use stagelinq::DeckEvent as Event;
        let now = Instant::now();
        match event {
            Event::DeviceDiscovered { name, .. } => {
                self.status(run, None, Some(format!("Found {name}")))
            }
            Event::Connected { address } => {
                self.status(run, Some(true), Some(format!("Connected to {address}")))
            }
            Event::Disconnected { address } => self.status(
                run,
                Some(false),
                Some(format!("Disconnected from {address}")),
            ),
            Event::Error { message } => self.problem(run, message),
            Event::StateChanged(snapshot) => {
                for deck in snapshot.decks {
                    let reading = Reading {
                        deck: deck.id,
                        track: deck.song_loaded.then(|| LoadedTrack {
                            title: deck.title,
                            artist: deck.artist,
                            network_path: (!deck.track_network_path.is_empty())
                                .then_some(deck.track_network_path),
                            duration: None,
                            bpm: None,
                        }),
                        position: (deck.sample_rate > 0.0).then(|| deck.samples / deck.sample_rate),
                        playing: Some(deck.playing),
                        bpm: deck.bpm,
                        // StageLinQ reports no unpitched tempo; its positions
                        // arrive often enough for the follower to correct.
                        rate: 1.0,
                        volume: 1.0,
                    };
                    self.ingest(run, Some(reading), now);
                }
            }
        }
    }

    fn status(&self, run: u64, connected: Option<bool>, detail: Option<String>) {
        let mut state = self.lock();
        if state.run != run {
            return;
        }
        if let Some(connected) = connected {
            state.connected = connected;
        }
        if let Some(detail) = detail {
            state.detail = detail;
        }
        state.problem = None;
    }

    fn problem(&self, run: u64, message: String) {
        let mut state = self.lock();
        if state.run == run {
            state.problem = Some(message);
        }
    }
}

/// One deck to resolve: match its track (or take the pinned one) and compile
/// that track's scores into the deck's layer.
struct Job {
    run: u64,
    deck: u8,
    generation: u64,
    venue_id: String,
    track: Option<LoadedTrack>,
    pinned: Option<String>,
}

async fn resolve(shared: Arc<Shared>, job: Job) {
    // Begun before any read, so a newer build for this deck supersedes this
    // one; see `RenderEngine::begin_scene_update`.
    let update = shared
        .render
        .begin_scene_update(SceneTarget::Deck(job.deck));
    let (scene, matched) = match resolve_scene(&shared, &job).await {
        Ok(resolved) => resolved,
        Err(message) => (None, DeckMatch::Failed { message }),
    };
    let mut state = shared.lock();
    let current = state.run == job.run
        && state
            .decks
            .get(&job.deck)
            .is_some_and(|deck| deck.generation == job.generation);
    if !current {
        return;
    }
    // Installed under the state lock, so a track change cannot land between
    // the check and the install.
    shared.render.finish_scene_update(
        update,
        scene.unwrap_or_else(|| crate::eval::Scene::new(Vec::new())),
    );
    if let Some(deck) = state.decks.get_mut(&job.deck) {
        deck.matched = matched;
    }
}

async fn resolve_scene(
    shared: &Shared,
    job: &Job,
) -> Result<(Option<crate::eval::Scene>, DeckMatch), String> {
    let manual = job.pinned.is_some();
    let track_id = match &job.pinned {
        Some(track_id) => Some(track_id.clone()),
        None => match &job.track {
            Some(track) => matching::find_track(&shared.pool, track).await?,
            None => None,
        },
    };
    let Some(track_id) = track_id else {
        return Ok((None, DeckMatch::Unmatched));
    };
    let track = crate::database::local::tracks::get_track_by_id(&shared.pool, &track_id)
        .await?
        .ok_or("That track is not in the library")?;
    let scene = deck_scene(shared, &track_id, &job.venue_id).await?;
    let has_score = !scene.is_empty();
    Ok((
        Some(scene),
        DeckMatch::Matched {
            track_id,
            title: track.title.unwrap_or_default(),
            artist: track.artist.unwrap_or_default(),
            has_score,
            manual,
        },
    ))
}

/// Every score for the track in the venue, combined into one scene.
async fn deck_scene(
    shared: &Shared,
    track_id: &str,
    venue_id: &str,
) -> Result<crate::eval::Scene, String> {
    let mut access = VenueAccess::<Read>::read(&shared.pool, VenueResource::Venue(venue_id))
        .await
        .map_err(|error| error.to_string())?;
    let scores =
        crate::database::local::scores::list_scores_for_track(&mut access, track_id).await?;
    drop(access);
    let mut annotations = Vec::new();
    let mut rig = crate::eval::aim::Rig::default();
    for score in scores {
        let scene = crate::compositor::build_score_scene(
            &shared.pool,
            &shared.storage,
            &shared.fixtures_root,
            &score.id,
            None,
        )
        .await?;
        if let Some(aimed) = scene.rig() {
            rig.extend(aimed);
        }
        annotations.extend(scene.annotations);
    }
    crate::eval::Scene::new(annotations)
        .with_rig(rig)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests;
