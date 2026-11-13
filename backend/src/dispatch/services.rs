//! [`AppServices`] — the single injection point every command body reaches its
//! services through, and the two host capabilities it carries.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::agent_execution::workspace::PythonWorkspaceService;
use crate::database::local::auth;
use crate::database::local::state::StateDb;
use crate::database::Db;
use crate::error::CommandError;
use crate::events::Events;
use crate::host_audio::HostAudioState;
use crate::output::OutputHub;
use crate::preprocessing::{AnalysisTaskGroup, WorkerEnvironment};
use crate::render_engine::RenderEngine;
use crate::services::agent_execution::LiveHandles;
use crate::services::fixtures::FixtureState;
use crate::storage::StorageRoot;

/// External DJ catalog reads needed by import handlers.
///
/// Production uses the installed Engine DJ/Rekordbox databases; a host test
/// can substitute disposable rows without putting fixture logic in handlers.
pub trait TrackSources: Send + Sync + 'static {
    /// Load one Engine DJ catalog identity and its tracks.
    fn engine_tracks<'a>(
        &'a self,
        library_path: &'a str,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<(String, Vec<super::EngineDjTrack>), String>>
                + Send
                + 'a,
        >,
    >;

    /// Load every Rekordbox track from the auto-discovered catalog.
    fn rekordbox_tracks(
        &self,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Vec<super::RekordboxTrack>, String>>
                + Send
                + '_,
        >,
    >;
}

struct SystemTrackSources;

/// The production Engine DJ/Rekordbox adapter.
#[must_use]
pub fn system_track_sources() -> Arc<dyn TrackSources> {
    Arc::new(SystemTrackSources)
}

impl TrackSources for SystemTrackSources {
    fn engine_tracks<'a>(
        &'a self,
        library_path: &'a str,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<(String, Vec<super::EngineDjTrack>), String>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            super::handlers::engine_dj::with_library(library_path, |pool| async move {
                let info = crate::engine_dj::db::get_library_info(&pool, library_path).await?;
                let tracks = crate::engine_dj::db::list_tracks(&pool).await?;
                Ok((info.database_uuid, tracks))
            })
            .await
            .map_err(|error| error.to_string())
        })
    }

    fn rekordbox_tracks(
        &self,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Vec<super::RekordboxTrack>, String>>
                + Send
                + '_,
        >,
    > {
        Box::pin(async {
            super::handlers::rekordbox::blocking(crate::rekordbox::subprocess::list_tracks)
                .await
                .map_err(|error| error.to_string())
        })
    }
}

// -----------------------------------------------------------------------------
// Services
// -----------------------------------------------------------------------------

/// Every service a command body can reach.
///
/// Each one is a process-global singleton built once at startup and
/// live for the process, so one struct passed by reference covers the whole
/// command surface and no body carries an injection lifetime.
///
/// Command bodies reach the host through explicit capabilities: [`Events`],
/// resolved storage/fixture paths,
/// and [`WorkerEnvironment`] cache/resource paths. Nothing else about a host
/// is abstracted here.
///
/// Fields are `pub(crate)`: handlers read them directly, and an external host
/// neither builds nor inspects them beyond the accessors below. That keeps
/// `RenderEngine` and friends out of the crate's public API.
pub struct AppServices {
    pub(crate) db: Db,
    pub(crate) state_db: StateDb,
    pub(crate) workspaces: Arc<PythonWorkspaceService>,
    pub(crate) analysis_tasks: AnalysisTaskGroup,
    pub(crate) workers: WorkerEnvironment,
    pub(crate) track_sources: Arc<dyn TrackSources>,
    /// Where a venue's share code is redeemed. The server, unless a host with
    /// none installs a stand-in — see [`AppServices::with_share_codes`].
    pub(crate) share_codes: Arc<dyn crate::services::venue_sharing::ShareCodeServer>,
    pub(crate) render_engine: RenderEngine,
    /// Every venue's live controls, which the engine, the inputs and the
    /// Perform session all play. `Arc` because they share it.
    pub(crate) controls: Arc<crate::controls::ControlBanks>,
    /// MIDI and OSC inputs bound to live controls. `Arc` because it owns
    /// the open ports — a clone would fork them.
    pub(crate) inputs: Arc<crate::inputs::InputHub>,
    /// Live follow of a DJ source. `Arc` because it owns the live source
    /// connection — a clone would fork it.
    pub(crate) perform: Arc<crate::perform::PerformSession>,
    /// Physical DMX output is installed only by the desktop host.
    pub(crate) output: Option<Arc<OutputHub>>,
    pub(crate) host_audio: HostAudioState,
    /// Whether the persisted audio setting may open the machine's audio
    /// device. Off on a windowless host, which must never probe it.
    pub(crate) audio_device: bool,
    pub(crate) track_audio: crate::sync::audio::TrackAudio,
    pub(crate) storage: StorageRoot,
    pub(crate) fixtures_root: PathBuf,
    /// In-memory index of the bundled fixture definitions. Held as an `Arc`
    /// because its interior is a bare `Mutex` — a clone would fork the index
    /// rather than share it.
    pub(crate) fixtures: Arc<FixtureState>,
    /// Delegated turns in flight, per parent thread. Empty on a host that
    /// never runs a `subagent` call; it is only the concurrency cap's counter.
    pub(crate) subagents: Arc<crate::agent::subagent::SubagentRegistry>,
    pub(crate) events: Events,
    /// Explicit trusted principal for a disposable headless fixture. Unset on
    /// the desktop app, where identity resolves from the verified state
    /// database and the app-database admission gate instead.
    pub(crate) fixture_principal: Option<String>,
    /// Record replication, when this host started it. `None` is a local-only
    /// process — a test, or a build with no PowerSync instance configured —
    /// and every command still works; only `sync_status` has less to say.
    pub(crate) sync: Option<crate::sync::service::Service>,
    /// Media transfer, when this host runs it: only its progress is read here,
    /// for the status bar's sync item.
    pub(crate) media: Option<crate::sync::media::Media>,
}

/// An owning, cloneable handle to services, for work that outlives the
/// command that starts it, such as an agent turn.
///
/// [`AppServices::into_shared`] is the only constructor. Derefs to
/// [`AppServices`], so a `&SharedServices` is a `&AppServices` everywhere a
/// handler wants one.
#[derive(Clone)]
pub struct SharedServices(Arc<AppServices>);

impl std::ops::Deref for SharedServices {
    type Target = AppServices;

    fn deref(&self) -> &AppServices {
        &self.0
    }
}

impl AppServices {
    /// What a Python cell may play or bind in this process.
    pub(crate) fn live_handles(&self) -> LiveHandles {
        LiveHandles {
            perform: Arc::clone(&self.perform),
            inputs: Arc::clone(&self.inputs),
            controls: Arc::clone(&self.controls),
        }
    }

    /// Assemble services without physical output or background loops.
    ///
    /// Everything constructible without a window is constructed here rather
    /// than by a host, so hosts cannot drift on how a singleton is
    /// configured; a host gets services from [`crate::boot::open`]. Events
    /// are discarded unless overridden with [`AppServices::with_events`].
    ///
    /// The deliberate absences are DMX output
    /// and the loops: nothing spawns a render loop, a sync loop, or an audio
    /// broadcaster, so a `RenderEngine` and a `HostAudioState` exist here but
    /// drive nothing.
    #[must_use]
    pub(crate) fn headless(
        db: Db,
        state_db: StateDb,
        storage: StorageRoot,
        fixtures_root: PathBuf,
        workspaces: Arc<PythonWorkspaceService>,
    ) -> Self {
        let render_engine = RenderEngine::default();
        let host_audio = HostAudioState::default();
        // Until a host opts in with `with_audio_device`, the machine's audio
        // device is never probed or opened. Transport still advances from
        // wall time.
        host_audio.set_audio_output_enabled(false);
        let track_audio =
            crate::sync::audio::TrackAudio::new(db.0.clone(), state_db.0.clone(), storage.clone());
        let controls = Arc::new(crate::controls::ControlBanks::default());
        let inputs = Arc::new(crate::inputs::InputHub::new(
            db.0.clone(),
            controls.clone(),
            false,
        ));
        let perform = Arc::new(crate::perform::PerformSession::new(
            render_engine.clone(),
            controls.clone(),
            db.0.clone(),
            storage.clone(),
            fixtures_root.clone(),
            host_audio.clone(),
        ));
        let share_codes = Arc::new(crate::services::venue_sharing::SupabaseShareCodes::new(
            state_db.0.clone(),
        ));
        Self {
            track_audio,
            db,
            state_db,
            workspaces,
            analysis_tasks: AnalysisTaskGroup::new(),
            workers: WorkerEnvironment::from_env_default()
                .expect("headless worker environment paths must resolve"),
            track_sources: system_track_sources(),
            share_codes,
            render_engine: render_engine.clone(),
            controls,
            inputs,
            perform,
            output: None,
            host_audio,
            audio_device: false,
            storage,
            fixtures_root,
            fixtures: Arc::new(FixtureState::empty()),
            subagents: Arc::default(),
            events: Events::discard(),
            fixture_principal: None,
            sync: None,
            media: None,
        }
    }

    /// Redeem share codes somewhere other than the server: a harness with no
    /// server stands in a [`crate::services::venue_sharing::LocalShareCodes`].
    #[must_use]
    pub fn with_share_codes(
        mut self,
        share_codes: Arc<dyn crate::services::venue_sharing::ShareCodeServer>,
    ) -> Self {
        self.share_codes = share_codes;
        self
    }

    /// Attach the running record replication, started on the booting host's
    /// reactor, which the SDK's tasks live on.
    #[must_use]
    pub(crate) fn with_sync(mut self, sync: crate::sync::service::Service) -> Self {
        self.sync = Some(sync);
        self
    }

    /// Report `media`'s transfers in the sync status. The host runs its loop;
    /// this keeps a handle on its progress.
    #[must_use]
    pub fn with_media(mut self, media: crate::sync::media::Media) -> Self {
        self.media = Some(media);
        self
    }

    /// Set this host's Python environment up in the background, and make
    /// analysis and the agent's cells wait for it instead of failing while it
    /// runs. Only the desktop sets Python up; harnesses and the CLI fail fast
    /// without one.
    #[must_use]
    pub fn start_python_setup(mut self) -> Self {
        let setup = self.workers.start_setup();
        self.workspaces.attach_setup(setup);
        self
    }

    /// Stop record replication, for a host that is quitting.
    pub fn stop_sync(&self) {
        if let Some(sync) = &self.sync {
            sync.stop();
        }
    }

    /// Resume stale or interrupted analysis once this host's Python setup has
    /// settled. Nobody is waiting on it, so its workers run below the UI: a
    /// version bump or a broken environment can queue the whole library here.
    /// A setup that left nothing to run on skips it, rather than failing every
    /// pending track.
    pub async fn reconcile_analysis(&self) -> Result<(), String> {
        // Settle before leasing, so a sign-in during setup finds no lease to
        // wait for, and the catch-up then runs for the identity it brought.
        self.workers.setup_settled().await;
        let epoch = self.analysis_tasks.current_epoch()?;
        let lease = self.analysis_tasks.lease(epoch)?;
        self.workers.ready(&lease.guard()).await?;
        crate::preprocessing::scheduler::reconcile_on_startup(
            self.db.0.clone(),
            self.storage.clone(),
            self.workers.clone().in_background(),
            self.events.clone(),
            lease.guard(),
        )
        .await
    }

    /// Enable physical output, and MIDI and OSC inputs, for the native
    /// desktop host. Harnesses retain the headless service set and never
    /// bind sockets or open devices.
    pub(crate) async fn with_output(mut self) -> Result<Self, String> {
        let output = Arc::new(OutputHub::new(&self.db.0, self.fixtures_root.clone()).await?);
        self.inputs = Arc::new(crate::inputs::InputHub::new(
            self.db.0.clone(),
            self.controls.clone(),
            true,
        ));
        self.output = Some(output);
        Ok(self)
    }

    /// Let the persisted audio setting open the machine's audio device, for
    /// a host with a window to play to.
    #[must_use]
    pub(crate) fn with_audio_device(mut self) -> Self {
        self.audio_device = true;
        self
    }

    /// Render the desktop's DMX at wire rate until its owning library shuts
    /// down. Each tick is published to the drivers, which send it at their
    /// own pace.
    pub async fn run_output(&self, mut shutdown: tokio::sync::watch::Receiver<bool>) {
        let Some(output) = &self.output else {
            return;
        };
        // One tick per output frame, mid-frame on the strobe clock, so every
        // tick samples one frame of the strobe gate (`luma_scene::strobe`).
        let frame = std::time::Duration::from_secs_f64(luma_scene::strobe::FRAME_S);
        let first = luma_scene::strobe::epoch()
            + std::time::Duration::from_secs_f64(luma_scene::strobe::next_frame_middle(
                luma_scene::strobe::clock(),
            ));
        let mut ticks = tokio::time::interval_at(tokio::time::Instant::from_std(first), frame);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut had_output = false;
        loop {
            tokio::select! {
                _ = ticks.tick() => {
                    // A laser test pattern shows though nothing plays.
                    let frame = self
                        .render_engine
                        .sample(self.host_audio.render_time())
                        .or_else(|| output.testing_lasers().then(Default::default));
                    if let Some(frame) = frame {
                        output.publish(&frame);
                        had_output = true;
                    } else if had_output {
                        output.publish(&crate::models::universe::UniverseState::default());
                        had_output = false;
                    }
                }
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() { break; }
                }
            }
        }
        if had_output {
            output.publish(&crate::models::universe::UniverseState::default());
        }
    }

    /// Put the services behind a shared handle.
    ///
    /// The agent's turn loop outlives the command that starts it, so it cannot
    /// borrow a command body's `&AppServices`.
    #[must_use]
    pub fn into_shared(self) -> SharedServices {
        SharedServices(Arc::new(self))
    }

    /// Observe the events commands emit.
    #[must_use]
    pub fn with_events(mut self, events: Events) -> Self {
        self.events = events;
        self
    }

    #[must_use]
    /// Replace external DJ catalog reads, primarily for a disposable host.
    pub fn with_track_sources(mut self, sources: Arc<dyn TrackSources>) -> Self {
        self.track_sources = sources;
        self
    }

    /// Trust `principal` as the caller's identity instead of resolving it from
    /// the verified session. For a disposable headless fixture only.
    #[must_use]
    pub(crate) fn with_fixture_principal(mut self, principal: Option<String>) -> Self {
        self.fixture_principal = principal;
        self
    }

    /// Push the persisted settings into the subsystems that read them.
    ///
    /// Settings live in a table, but two subsystems hold their own copy of the
    /// ones they care about: the DMX output hub and the audio host. A write goes
    /// through `set_setting`, which calls this — but a freshly built
    /// `AppServices` has never been told, so [`crate::boot::open`] calls it
    /// once or the session runs on the defaults. Output is skipped
    /// on a host with no hub, and audio on a host without
    /// [`AppServices::with_audio_device`].
    ///
    /// # Errors
    ///
    /// If the settings cannot be read, or a subsystem refuses what they say.
    pub(crate) async fn apply_persisted_settings(&self) -> Result<(), CommandError> {
        let pool = &self.db.0;
        if let Some(output) = &self.output {
            crate::output::reload_settings(output, pool).await?;
        }
        if self.audio_device {
            crate::host_audio::reload_settings(&self.host_audio, pool).await?;
        }
        Ok(())
    }

    /// Write the agent rows a quit left in progress to their synced rows,
    /// leaving those a live turn in another process is still writing.
    /// [`crate::boot::open`] calls this, before any turn here can run.
    ///
    /// # Errors
    ///
    /// If the admission or the open rows cannot be read.
    pub(crate) async fn recover_open_agent_messages(&self) -> Result<(), CommandError> {
        // Signed out, no turn is anybody's to recover: the rows wait for the
        // boot that follows their owner's sign-in.
        let Some(principal) = self.admitted_principal().await? else {
            return Ok(());
        };
        crate::agent::recover_open_rows(self, &principal)
            .await
            .map_err(CommandError::Internal)
    }

    /// Drop everything the previous identity left live in this process: its
    /// Python workspaces, loaded audio, live follow, input ports, control
    /// values, cached venue rigs and installed scenes. Every identity change
    /// — a switch to another account and a sign-out — calls this, after
    /// closing write admission, so no effect of the old identity lands after.
    ///
    /// The returned barrier keeps Python workspaces closed; hold it until the
    /// next identity is admitted.
    pub(crate) async fn reset_identity(
        &self,
    ) -> crate::agent_execution::workspace::WorkspaceIdentityBarrier<'_> {
        let workspaces = self.workspaces.suspend_for_identity_switch().await;
        self.host_audio.unload();
        self.perform.stop().await;
        self.inputs.close().await;
        self.controls.clear();
        crate::database::local::venue_graph::rig_changed();
        self.render_engine.reset_for_identity_switch();
        workspaces
    }

    /// The principal the app database admits, or `None` while nobody is
    /// signed in. A command wants [`Self::require_session`]; this is for the
    /// host's own work that has nothing to do until someone is.
    ///
    /// # Errors
    ///
    /// [`CommandError::Internal`] if the admission gate cannot be read.
    async fn admitted_principal(&self) -> Result<Option<String>, CommandError> {
        match &self.fixture_principal {
            Some(principal) => Ok(Some(principal.clone())),
            None => Ok(auth::admitted_principal(&self.db.0).await?),
        }
    }

    /// The signed-in account: whose library a command reads, and the owner
    /// every row it writes is written under.
    ///
    /// Sync is not optional and neither is identity: a row's `uid` is who it
    /// belongs to, and there is no such person while nobody is signed in. A
    /// command that reads or writes the library asks here — or opens a
    /// `VenueAccess` or `VisibleTrackAccess`, which ask the same question in
    /// their own transaction — and the answer is either a principal or a
    /// refusal the user can act on: the shell's sign-in screen.
    ///
    /// # Errors
    ///
    /// [`CommandError::Unauthorized`] when no session exists, and
    /// [`CommandError::Internal`] if the gate cannot be read.
    pub async fn require_session(&self) -> Result<String, CommandError> {
        self.admitted_principal()
            .await?
            .ok_or_else(|| CommandError::Unauthorized(auth::SIGN_IN_REQUIRED.to_string()))
    }
}

/// Transitional accessors.
///
/// A host that implements no commands of its own needs none of these — it calls
/// [`super::dispatch`] and nothing else. They exist so a host that does can
/// reach these singletons rather than keep a second, forkable copy.
impl AppServices {
    /// The app database.
    pub fn db(&self) -> &Db {
        &self.db
    }

    /// The state database's pool — the session lives there, not in the app
    /// database, which is why sync and media both have to be handed it.
    pub fn state_pool(&self) -> &sqlx::SqlitePool {
        &self.state_db.0
    }

    /// The installed scene, for a host that draws its own frames.
    ///
    /// Not a command: a command is a request whose answer crosses a wire, and
    /// a viewport sampling the scene sixty times a second is neither. The
    /// firehose this replaces — a full `UniverseState` JSON-serialised at
    /// 240 Hz — is precisely what re-routing it through dispatch would rebuild.
    pub fn render_engine(&self) -> &RenderEngine {
        &self.render_engine
    }

    /// The transport clock, for the same reason: a frame's `t` is read at
    /// paint time, synchronously, and an awaited snapshot is a frame late.
    pub fn host_audio(&self) -> &HostAudioState {
        &self.host_audio
    }

    /// Root of Luma's durable on-disk data.
    pub fn storage(&self) -> &StorageRoot {
        &self.storage
    }

    /// Root of the bundled fixture definitions.
    pub fn fixtures_root(&self) -> &Path {
        &self.fixtures_root
    }
}
