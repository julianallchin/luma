//! Live follow: decks playing in DJ software drive the matching scores.
//!
//! One [`PerformSession`] follows one source at a time. Each source reports
//! its decks as [`Reading`]s; the session matches each deck's track against
//! the library, compiles that track's scores for the venue into the deck's
//! render layer, and hands the render engine a [`clock::DeckClock`] per deck so the
//! lights keep moving between reports. Nothing here needs a UI: the Perform
//! screen only reads [`PerformSession::state`] and sets the few controls.

pub(crate) mod clock;
pub(crate) mod live;
mod matching;
mod rkbx;
mod virtual_deck;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sqlx::SqlitePool;
use tokio::task::JoinHandle;

use crate::database::local::venue_access::{Read, VenueAccess, VenueResource};
use crate::host_audio::HostAudioState;
use crate::models::perform::{DeckMatch, PerformClock, PerformDeck, PerformSource, PerformState};
use crate::render_engine::{PerformDeckInput, RenderEngine, SceneTarget};
use live::LiveClock;
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
    /// The venue whose Perform set is open. It outlives any source:
    /// following nothing, the set runs on the tapped tempo.
    set_venue: Option<String>,
    /// The deck and song the set was compiled on, or `None` for the tapped
    /// tempo.
    set_track: Option<(u8, String)>,
    /// The clock the open set runs on, `None` until it has compiled.
    set_clock: Option<LiveClock>,
}

struct Shared {
    state: Mutex<State>,
    render: RenderEngine,
    pool: SqlitePool,
    storage: StorageRoot,
    fixtures_root: PathBuf,
    /// The app's audio player, which a virtual source follows.
    host: HostAudioState,
}

enum Source {
    Rkbx(rkbx::Listener),
    ProDjLink(prodjlink::ProDJLinkClient),
    StageLinq(stagelinq::StageLinqClient),
    Virtual(JoinHandle<()>),
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
        host: HostAudioState,
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
                    set_venue: None,
                    set_track: None,
                    set_clock: None,
                }),
                render,
                pool,
                storage,
                fixtures_root,
                host,
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
        // Name the new source before the old one stops, so a read taken while
        // it stops sees the switch, never a moment of not following. The bump
        // in `run` is what silences the old source's late callbacks.
        let run = self.shared.reset_to(Some((source.clone(), venue_id)));
        self.stop_source(running.take()).await;
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
            PerformSource::Virtual => {
                {
                    let mut state = self.shared.lock();
                    state.connected = true;
                    state.detail = "Following Luma's player".into();
                }
                let host = self.shared.host.clone();
                Ok(Source::Virtual(virtual_deck::follow(
                    self.shared.clone(),
                    host,
                    run,
                )))
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
        drop(running);
        self.relive().await;
        Ok(())
    }

    /// Stop following and clear every deck's lights. Idempotent.
    pub async fn stop(&self) {
        let mut running = self.running.lock().await;
        self.shutdown(running.take()).await;
        drop(running);
        self.relive().await;
    }

    /// The decks changed under the set: put it on whatever is master now,
    /// the tapped tempo until live follow finds one.
    async fn relive(&self) {
        let open = {
            let mut state = self.shared.lock();
            state.set_track = master(&state);
            state.set_venue.is_some()
        };
        if open {
            Shared::recompile_set(self.shared.clone()).await;
        }
    }

    async fn shutdown(&self, running: Option<Running>) {
        self.shared.reset();
        self.stop_source(running).await;
    }

    /// End a running source's task and connection, leaving the state alone.
    async fn stop_source(&self, running: Option<Running>) {
        let Some(running) = running else {
            return;
        };
        running.ticker.abort();
        match running.source {
            Source::Rkbx(listener) => drop(listener),
            Source::ProDjLink(client) => client.stop().await,
            Source::StageLinq(client) => client.stop().await,
            Source::Virtual(task) => {
                task.abort();
                let _ = task.await;
            }
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

    /// Open `venue_id`'s Perform set: compile every pattern of it, to play
    /// as its controls say. Replaces the set open before.
    pub async fn open_set(&self, venue_id: String) -> Result<(), String> {
        VenueAccess::<Read>::read(&self.shared.pool, VenueResource::Venue(&venue_id))
            .await
            .map_err(|error| error.to_string())?;
        {
            let mut state = self.shared.lock();
            state.set_venue = Some(venue_id);
            state.set_track = master(&state);
        }
        Shared::recompile_set(self.shared.clone()).await;
        Ok(())
    }

    /// Set control `name` of the open set's venue to `value`, 0–1: a pad or
    /// fader of the Perform tab. No database read: the venue was admitted when
    /// its set opened, and this runs on every fader move.
    pub fn set_control(&self, name: &str, value: f64) -> Result<(), String> {
        let venue_id = self
            .shared
            .lock()
            .set_venue
            .clone()
            .ok_or("No Perform set is open")?;
        crate::controls::venue(&venue_id).set(name, value);
        Ok(())
    }

    /// Press (`down`) or release button `name` of the open set's venue: a
    /// button node of a pattern, its pad or another. It plays as the button says: a toggle
    /// flips on the press, a flash is on while held.
    pub fn press(&self, name: &str, down: bool) -> Result<(), String> {
        let venue_id = self
            .shared
            .lock()
            .set_venue
            .clone()
            .ok_or("No Perform set is open")?;
        crate::controls::venue(&venue_id).press(name, down, Instant::now());
        Ok(())
    }

    /// The venue whose Perform set is open, if one is.
    pub fn open_venue(&self) -> Option<String> {
        self.shared.lock().set_venue.clone()
    }

    /// `venue_id`'s Perform set changed: recompile it if it is open.
    pub async fn set_changed(&self, venue_id: &str) {
        if self.shared.lock().set_venue.as_deref() == Some(venue_id) {
            Shared::recompile_set(self.shared.clone()).await;
        }
    }

    /// Everything live follow knows, extrapolated to now.
    pub fn state(&self) -> PerformState {
        let state = self.shared.lock();
        let Some(source) = state.source.clone() else {
            return PerformState::off(
                state.latency_ms,
                set_clock(&state),
                set_controls(&state),
                set_buttons(&state),
            );
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
            clock: set_clock(&state),
            controls: set_controls(&state),
            buttons: set_buttons(&state),
        }
    }
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().expect("perform state poisoned")
    }

    /// Pin `deck` to `track_id` for `run`, or with `None`, match it
    /// automatically again.
    fn pin(shared: &Arc<Self>, run: u64, deck: u8, track_id: Option<String>) -> Result<(), String> {
        let mut state = shared.lock();
        if state.run != run {
            return Err("Live follow moved on".into());
        }
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
                tokio::spawn(resolve(shared.clone(), job));
            }
            None => entry.match_due = Some(Instant::now()),
        }
        Ok(())
    }

    /// Compile the open set on the clock in `set_track` and install it. A
    /// newer build supersedes this one.
    async fn recompile_set(shared: Arc<Self>) {
        let update = shared.render.begin_scene_update(SceneTarget::Live);
        let (venue_id, track) = {
            let state = shared.lock();
            (state.set_venue.clone(), state.set_track.clone())
        };
        let Some(venue_id) = venue_id else {
            shared.render.finish_live_update(update, None);
            return;
        };
        match Self::compile_set(&shared, &venue_id, track.as_ref()).await {
            Ok((clock, set)) => {
                let mut state = shared.lock();
                state.set_clock = Some(clock);
                follow_tempo(&state);
                drop(state);
                shared.render.finish_live_update(update, set);
            }
            Err(message) => {
                log::warn!("the Perform set did not compile: {message}");
                shared.lock().problem = Some(format!("Perform set: {message}"));
                shared.render.finish_live_update(update, None);
            }
        }
    }

    async fn compile_set(
        shared: &Shared,
        venue_id: &str,
        track: Option<&(u8, String)>,
    ) -> Result<(LiveClock, Option<live::LiveSet>), String> {
        let mut access = VenueAccess::<Read>::read(&shared.pool, VenueResource::Venue(venue_id))
            .await
            .map_err(|error| error.to_string())?;
        let patterns = crate::database::local::perform_set::list(&mut access).await?;
        let layout = crate::database::local::perform_set::layout(&mut access)
            .await?
            .unwrap_or_default();
        live::compile(
            &mut access,
            &shared.fixtures_root,
            &shared.storage,
            &patterns,
            &layout,
            track.map(|(deck, track_id)| (*deck, track_id.as_str())),
        )
        .await
    }

    /// Forget `deck` and clear its lights: its source has nothing loaded.
    fn remove_deck(&self, run: u64, deck: u8) {
        let mut state = self.lock();
        if state.run != run || state.decks.remove(&deck).is_none() {
            return;
        }
        self.render.clear_deck(deck);
        self.push(&state);
    }

    /// Forget the source and its decks, and clear their lights.
    fn reset(&self) {
        self.reset_to(None);
    }

    /// Clear the session for `next` (a source and its venue, or nothing) in
    /// one step, so no read sees the gap between two sources. Returns the new
    /// run.
    fn reset_to(&self, next: Option<(PerformSource, String)>) -> u64 {
        let mut state = self.lock();
        state.run += 1;
        let starting = next.is_some();
        let (source, venue_id) = next.unzip();
        state.source = source;
        state.venue_id = venue_id;
        state.connected = false;
        state.detail = if starting { "Starting".into() } else { String::new() };
        state.problem = None;
        state.last_heard = None;
        state.saw_position = false;
        state.decks.clear();
        let run = state.run;
        drop(state);
        self.render.clear_perform();
        run
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
        follow_tempo(&state);
        // The set follows the master deck's song.
        let master = master(&state);
        if state.set_venue.is_some() && master != state.set_track {
            state.set_track = master;
            drop(state);
            tokio::spawn(Self::recompile_set(shared.clone()));
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
/// The deck the Perform set follows: the loudest playing deck whose song
/// matched the library, else any matched deck. `None` puts it on the tapped
/// tempo.
fn master(state: &State) -> Option<(u8, String)> {
    let matched = |deck: &Deck| match &deck.matched {
        DeckMatch::Matched { track_id, .. } => Some(track_id.clone()),
        _ => None,
    };
    let playing = state
        .decks
        .iter()
        .filter(|(_, deck)| deck.follower.clock().is_some_and(|clock| clock.playing()))
        .filter_map(|(id, deck)| matched(deck).map(|track| (*id, track, deck.volume)))
        .max_by(|a, b| a.2.total_cmp(&b.2))
        .map(|(id, track, _)| (id, track));
    playing.or_else(|| {
        state
            .decks
            .iter()
            .find_map(|(id, deck)| matched(deck).map(|track| (*id, track)))
    })
}

/// What the open set is timed by, `None` until it has compiled.
fn set_clock(state: &State) -> Option<PerformClock> {
    let venue_id = state.set_venue.as_deref()?;
    Some(match state.set_clock? {
        LiveClock::Deck(deck) => PerformClock::Deck { deck },
        LiveClock::Tap => PerformClock::Tap(crate::controls::venue(venue_id).tempo(Instant::now())),
    })
}

/// Count the open set's buttons' beats at the tempo its clock runs at: its
/// deck's, or the tapped one.
fn follow_tempo(state: &State) {
    let Some(venue_id) = state.set_venue.as_deref() else {
        return;
    };
    let bpm = match state.set_clock {
        Some(LiveClock::Deck(deck)) => state.decks.get(&deck).map(|deck| deck.bpm),
        _ => None,
    };
    crate::controls::venue(venue_id).follow(bpm);
}

/// The open set's buttons as they stand now.
fn set_buttons(state: &State) -> BTreeMap<String, luma_patterns::press::Button> {
    state
        .set_venue
        .as_deref()
        .map(|venue_id| crate::controls::venue(venue_id).buttons())
        .unwrap_or_default()
}

/// The open set's venue's controls, as set now.
fn set_controls(state: &State) -> BTreeMap<String, f64> {
    state
        .set_venue
        .as_deref()
        .map(|venue_id| crate::controls::venue(venue_id).values())
        .unwrap_or_default()
}

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
