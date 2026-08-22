//! The command dispatcher seam: Luma's command surface, decoupled from any
//! host runtime.
//!
//! # Interface
//!
//! The whole seam is three ideas:
//!
//! 1. Build an [`AppServices`] — [`AppServices::headless`] for a host that is
//!    not the Tauri app.
//! 2. Call [`dispatch`] with a command name and its JSON arguments.
//! 3. Handle a [`CommandError`].
//!
//! Everything else — the wire decoding, the handler bodies, the generated
//! `#[tauri::command]` wrappers — is implementation. A host that wants events
//! or process control implements [`EventSink`] and [`Host`]; both have
//! do-nothing defaults ([`Events::discard`], [`HostControl::process_exit`]) so
//! a minimal host implements neither.
//!
//! # Implementation
//!
//! The command table below generates two entry points from one declaration:
//! `adapter::<name>`, a `#[tauri::command]` that injects `AppServices` and
//! lowers `CommandError` to the `String` the wire expects; and an arm of
//! [`dispatch`], which decodes arguments from JSON instead. Declaring the wire
//! name, argument names, argument types and return type exactly once is what
//! keeps two hosts from drifting apart.
//!
//! Wire decoding lives in this file rather than its own module because it and
//! [`dispatch`] know the same thing — the wire schema. Splitting them would be
//! decomposition by chronology, not by knowledge.
//!
//! `docs/specs/dispatcher-port-guide.md` has the recipe for putting a command
//! on the seam, the special cases, and the designs that lost.

#![warn(missing_docs)]

mod error;
pub(crate) mod handlers;
#[cfg(test)]
mod manifest;
mod services;
mod tauri_host;

pub use crate::engine_dj::types::EngineDjTrack as ImportedEngineDjTrack;
pub use crate::rekordbox::types::RekordboxTrack as ImportedRekordboxTrack;
pub use error::CommandError;
pub use services::system_track_sources;
pub use services::{AppServices, EventSink, Events, Host, HostControl, TrackSources};
pub(crate) use tauri_host::{tauri_events, tauri_host};

use serde::de::DeserializeOwned;
use serde_json::Value;

/// Declare a command once; get the Tauri adapter, the JSON dispatch arm, and
/// the name registry from it.
///
/// Each row reads as `<handler module>::<wire name>(<args>) -> <return type>`.
/// The wire name *is* the handler function name, and the argument names are the
/// handler's parameter names, which Tauri renames `snake_case` → `camelCase` on
/// the wire.
///
/// Every handler is `async`, including the ones whose bodies never await —
/// awaiting a synchronous body costs nothing and keeps the table free of
/// special cases.
macro_rules! commands {
    ($( $domain:ident :: $name:ident ( $($arg:ident : $ty:ty),* $(,)? ) -> $ret:ty );* $(;)?) => {
        /// The Tauri adapter. Each wrapper does the two things a handler
        /// cannot: it receives the host's shared `AppServices`, and it lowers
        /// [`CommandError`] to the `String` the wire carries. Generated, so a
        /// wrapper cannot drift from its handler.
        ///
        /// Results are returned in their concrete type rather than through
        /// `serde_json::Value`, so the desktop path serializes exactly once.
        pub(crate) mod adapter {
            #![allow(clippy::too_many_arguments, missing_docs)]
            use super::*;
            $(
                #[tauri::command]
                pub async fn $name(
                    services: tauri::State<'_, std::sync::Arc<AppServices>>,
                    $($arg: $ty,)*
                ) -> Result<$ret, String> {
                    handlers::$domain::$name(&services, $($arg),*)
                        .await
                        .map_err(String::from)
                }
            )*
        }

        /// Run a command by its wire name against `services`.
        ///
        /// `args` is the same JSON object the frontend passes to `invoke`;
        /// arguments are accepted in either their `camelCase` wire spelling or
        /// their `snake_case` Rust spelling.
        ///
        /// # Errors
        ///
        /// [`CommandError::NotFound`] if no command has that name,
        /// [`CommandError::Invalid`] if an argument is missing or undecodable,
        /// otherwise whatever the command itself returned.
        pub async fn dispatch(
            services: &AppServices,
            name: &str,
            args: &Value,
        ) -> Result<Value, CommandError> {
            match name {
                $(
                    stringify!($name) => {
                        $( let $arg: $ty = decode(args, stringify!($arg))?; )*
                        let value = handlers::$domain::$name(services, $($arg),*).await?;
                        serde_json::to_value(value).map_err(|error| {
                            CommandError::Internal(format!(
                                "failed to serialize `{name}` result: {error}"
                            ))
                        })
                    }
                )*
                other => Err(CommandError::NotFound(format!("unknown command `{other}`"))),
            }
        }

        /// The table itself, structurally, for the tests that assert over the
        /// registry as a whole and for the manifest generator. Not an
        /// interface: a host learns a name is not ours from
        /// [`CommandError::NotFound`], and a second way to ask would be a
        /// second thing to keep true.
        #[cfg(test)]
        const TABLE: &[manifest::Command] = &[$(
            manifest::Command {
                name: stringify!($name),
                domain: stringify!($domain),
                args: &[$(
                    manifest::Arg {
                        name: stringify!($arg),
                        rust_type: stringify!($ty),
                    }
                ),*],
                returns: stringify!($ret),
            }
        ),*];
    };
}

use std::collections::HashMap;

use crate::annotation_preview::LivePreviewInput;
use crate::artnet::ArtNetNode;
use crate::compositor::LiveAnnotation;
use crate::database::remote::queries::SearchPatternRow;
use crate::engine_dj::types::{EngineDjLibraryInfo, EngineDjPlaylist, EngineDjTrack};
use crate::host_audio::HostAudioSnapshot;
use crate::models::agent_execution::{PythonCellResult, PythonScopeInput};
use crate::models::agent_threads::{
    AgentThread, AgentThreadDetail, AgentThreadMessage, AppendAgentThreadMessagesInput,
    CreateAgentThreadInput,
};
use crate::models::authored_state::{
    AuthoredCurrentRevision, AuthoredHistoryPage, AuthoredRestoreResult, AuthoredTurnCommit,
    AuthoredWorkspaceCheck, AuthoredWorkspaceCommit, AuthoredWorkspaceHandle,
    AuthoredWorkspaceInput, AuthoredWorkspaceMerge, CommitAuthoredWorkspaceInput,
    CreateAuthoredWorkspaceInput, FinalizeAuthoredTurnInput, ForkAuthoredWorkspaceInput,
    MergeAuthoredWorkspaceInput, MergeAuthoredWorkspaceIntoWorkspaceInput,
    PrepareAuthoredTurnInput, PreparedAuthoredTurn, RestoreAuthoredStateInput,
    WriteAuthoredWorkspaceGraphInput,
};
use crate::models::fixtures::{FixtureDefinition, FixtureEntry, PatchedFixture};
use crate::models::groups::{FixtureGroup, FixtureGroupNode, MovementConfig};
use crate::models::midi::{
    ControllerState, ControllerStatus, CreateBindingInput, CreateCueInput, CreateModifierInput,
    Cue, MidiBinding, ModifierDef, Target, UpdateBindingInput, UpdateCueInput,
};
use crate::models::mixer::{MixerMapping, MixerStatus};
use crate::models::node_graph::{
    BeatGrid, Graph, GraphContext, NodeTypeDef, PatternArgDef, RunResult,
};
use crate::models::patterns::{
    AnnotationPreview, ForkPatternInput, ForkPatternResult, PatternCategory, PatternSummary,
};
use crate::models::perform::PerformTrackMatch;
use crate::models::scores::{
    CreateTrackScoreInput, DeleteTrackScoreInput, Score, ScoreSummary, TrackScore,
    UpdateTrackScoreInput,
};
use crate::models::stage::StagePiece;
use crate::models::tracks::{TrackBrowserRow, TrackImportResult, TrackSummary};
use crate::models::universe::UniverseState;
use crate::models::venues::Venue;
use crate::models::waveforms::{TrackWaveform, WaveformWindow};
use crate::rekordbox::types::{RekordboxLibraryInfo, RekordboxPlaylist, RekordboxTrack};
use crate::render_engine::PerformDeckInput;
use crate::services::graph_documents::{GraphDocument, GraphEditResult};
use crate::services::track_edits::TrackEditResult;
use crate::services::tracks::TrackBarClassifications;
use crate::settings::AppSettings;
use crate::sync::orchestrator::SyncReport;
use handlers::score_dsl::{
    ScoreDslExportResponse, ScoreDslImportResponse, ScoreDslValidationResponse,
};
use handlers::tracks::TrackAudioBase64;
use prodjlink::DiscoveredDevice;

commands! {
    node_graph::get_node_types() -> Vec<NodeTypeDef>;
    node_graph::run_graph(
        graph: Graph,
        context: GraphContext,
        include_mel_specs: Option<bool>,
        agent_thread_id: Option<String>,
        agent_execution_id: Option<String>,
        drive_live_preview: Option<bool>,
    ) -> RunResult;
    node_graph::preview_pattern(
        pattern_id: String,
        track_id: String,
        venue_id: String,
        start_time: f32,
        end_time: f32,
        beat_grid: Option<BeatGrid>,
        fps: f32,
    ) -> Vec<UniverseState>;

    patterns::list_patterns() -> Vec<PatternSummary>;
    patterns::get_pattern(id: String) -> PatternSummary;
    patterns::create_pattern(
        request_id: String,
        name: String,
        description: Option<String>,
    ) -> PatternSummary;
    patterns::update_pattern(
        id: String,
        name: String,
        description: Option<String>,
    ) -> PatternSummary;
    patterns::fork_pattern(input: ForkPatternInput) -> ForkPatternResult;
    patterns::delete_pattern(id: String) -> ();
    patterns::set_pattern_category(pattern_id: String, category_name: Option<String>) -> ();
    patterns::verify_pattern(id: String, verify: bool) -> PatternSummary;
    patterns::get_pattern_graph_document(
        id: String,
        implementation_id: Option<String>,
    ) -> GraphDocument;
    patterns::get_pattern_args(
        id: String,
        venue_id: Option<String>,
        implementation_id: Option<String>,
    ) -> Vec<PatternArgDef>;
    patterns::save_pattern_graph_document(
        id: String,
        implementation_id: String,
        operation_id: String,
        base_revision: String,
        graph: Graph,
    ) -> GraphEditResult;

    agent_threads::agent_thread_get(thread_id: String) -> AgentThreadDetail;
    agent_threads::agent_thread_list(
        agent_kind: Option<String>,
        subject_kind: Option<String>,
        subject_id: Option<String>,
    ) -> Vec<AgentThread>;
    agent_threads::agent_thread_create(input: CreateAgentThreadInput) -> AgentThread;
    agent_threads::agent_thread_append_messages(
        thread_id: String,
        input: AppendAgentThreadMessagesInput,
    ) -> Vec<AgentThreadMessage>;
    agent_threads::agent_thread_rename(thread_id: String, title: Option<String>) -> AgentThread;
    agent_threads::agent_thread_delete(thread_id: String) -> ();

    // Only what a webview needs and a typed caller does not: it cannot hold a
    // `TurnStream`, so a turn is addressed by thread id and its deltas arrive
    // as `"agent-turn"` events.
    agent::agent_turn_start(thread_id: String, prompt: String) -> String;
    agent::agent_turn_cancel(thread_id: String) -> bool;
    agent::agent_steer(thread_id: String, message: String) -> ();

    authored_state::authored_state_prepare_turn(
        input: PrepareAuthoredTurnInput,
    ) -> PreparedAuthoredTurn;
    authored_state::authored_state_finalize_turn(
        input: FinalizeAuthoredTurnInput,
    ) -> AuthoredTurnCommit;
    authored_state::authored_state_recover_turns(thread_id: String) -> Vec<AuthoredTurnCommit>;
    authored_state::authored_state_list_history(
        thread_id: String,
        cursor: Option<String>,
        limit: Option<usize>,
    ) -> AuthoredHistoryPage;
    authored_state::authored_state_restore(
        input: RestoreAuthoredStateInput,
    ) -> AuthoredRestoreResult;
    authored_state::authored_state_current_revision(
        thread_id: String,
    ) -> AuthoredCurrentRevision;
    authored_state::authored_state_create_workspace(
        input: CreateAuthoredWorkspaceInput,
    ) -> AuthoredWorkspaceHandle;
    authored_state::authored_state_fork_workspace(
        input: ForkAuthoredWorkspaceInput,
    ) -> AuthoredWorkspaceHandle;
    authored_state::authored_state_check_workspace(
        input: AuthoredWorkspaceInput,
    ) -> AuthoredWorkspaceCheck;
    authored_state::authored_state_write_workspace_graph(
        input: WriteAuthoredWorkspaceGraphInput,
    ) -> Graph;
    authored_state::authored_state_commit_workspace(
        input: CommitAuthoredWorkspaceInput,
    ) -> AuthoredWorkspaceCommit;
    authored_state::authored_state_merge_workspace(
        input: MergeAuthoredWorkspaceInput,
    ) -> AuthoredWorkspaceMerge;
    authored_state::authored_state_merge_workspace_into_workspace(
        input: MergeAuthoredWorkspaceIntoWorkspaceInput,
    ) -> AuthoredWorkspaceMerge;
    authored_state::authored_state_remove_workspace(input: AuthoredWorkspaceInput) -> ();

    agent_execution::run_python_cell(
        thread_id: String,
        execution_id: Option<String>,
        authored_workspace_id: Option<String>,
        turn_message_id: String,
        code: String,
        scope: PythonScopeInput,
    ) -> PythonCellResult;
    agent_execution::cancel_python_cell(
        thread_id: String,
        execution_id: Option<String>,
        authored_workspace_id: Option<String>,
    ) -> bool;

    fixtures::get_patched_fixtures(venue_id: String) -> Vec<PatchedFixture>;
    fixtures::initialize_fixtures() -> usize;
    fixtures::search_fixtures(query: String, offset: usize, limit: usize) -> Vec<FixtureEntry>;
    fixtures::get_fixture_definition(path: String) -> FixtureDefinition;
    fixtures::patch_fixture(
        venue_id: String,
        universe: i64,
        address: i64,
        num_channels: i64,
        manufacturer: String,
        model: String,
        mode_name: String,
        fixture_path: String,
        label: Option<String>,
    ) -> PatchedFixture;
    fixtures::move_patched_fixture(venue_id: String, id: String, address: i64) -> ();
    fixtures::move_patched_fixture_spatial(
        venue_id: String,
        id: String,
        pos_x: f64,
        pos_y: f64,
        pos_z: f64,
        rot_x: f64,
        rot_y: f64,
        rot_z: f64,
    ) -> ();
    fixtures::remove_patched_fixture(venue_id: String, id: String) -> ();
    fixtures::rename_patched_fixture(venue_id: String, id: String, label: String) -> ();

    groups::list_groups(venue_id: String) -> Vec<FixtureGroup>;
    groups::create_group(
        venue_id: String,
        name: Option<String>,
        axis_lr: Option<f64>,
        axis_fb: Option<f64>,
        axis_ab: Option<f64>,
    ) -> FixtureGroup;
    groups::update_group(
        id: String,
        name: Option<String>,
        axis_lr: Option<f64>,
        axis_fb: Option<f64>,
        axis_ab: Option<f64>,
    ) -> FixtureGroup;
    groups::delete_group(id: String) -> ();
    groups::add_fixture_to_group(
        fixture_id: String,
        group_id: String,
        head_index: Option<i64>,
    ) -> ();
    groups::remove_fixture_from_group(
        fixture_id: String,
        group_id: String,
        head_index: Option<i64>,
    ) -> ();
    groups::get_grouped_hierarchy(venue_id: String) -> Vec<FixtureGroupNode>;
    groups::get_ungrouped_fixtures(venue_id: String) -> Vec<PatchedFixture>;
    groups::update_movement_config(
        group_id: String,
        config: Option<MovementConfig>,
    ) -> FixtureGroup;
    groups::preview_selection_query(
        venue_id: String,
        query: String,
        seed: Option<u64>,
    ) -> Vec<PatchedFixture>;

    artnet::start_discovery() -> ();
    artnet::stop_discovery() -> ();
    artnet::get_discovered_nodes() -> Vec<ArtNetNode>;

    waveforms::get_track_waveform(track_id: String) -> TrackWaveform;
    waveforms::get_track_waveform_window(
        track_id: String,
        start_seconds: f64,
        end_seconds: f64,
        buckets: u32,
    ) -> WaveformWindow;
    waveforms::reprocess_waveform(track_id: String) -> TrackWaveform;

    tracks::list_tracks() -> Vec<TrackSummary>;
    tracks::list_tracks_enriched(venue_id: Option<String>) -> Vec<TrackBrowserRow>;
    tracks::update_track_metadata(
        track_id: String,
        title: Option<String>,
        artist: Option<String>,
        album: Option<String>,
    ) -> ();
    tracks::delete_track(track_id: String) -> ();
    tracks::get_track_beats(track_id: String) -> Option<BeatGrid>;
    tracks::get_track_bar_classifications(
        track_id: String,
    ) -> Option<TrackBarClassifications>;
    tracks::get_track_drum_onsets(track_id: String) -> Option<HashMap<String, Vec<f32>>>;
    tracks::get_classifier_thresholds() -> HashMap<String, f64>;
    tracks::get_track_audio_base64(track_id: String) -> TrackAudioBase64;
    tracks::get_venue_annotation_counts(venue_id: String) -> HashMap<String, i64>;

    compositor::composite_track(
        track_id: String,
        venue_id: String,
        annotations: Option<Vec<LiveAnnotation>>,
        skip_cache: Option<bool>,
    ) -> ();
    compositor::leave_track(score_id: String) -> ();

    annotation_preview::preview_annotation(
        track_id: String,
        venue_id: String,
        annotation: LivePreviewInput,
    ) -> AnnotationPreview;
    annotation_preview::generate_annotation_previews(
        track_id: String,
        venue_id: String,
    ) -> Vec<AnnotationPreview>;
    annotation_preview::preview_pattern_image(
        pattern_id: String,
        track_id: String,
        venue_id: String,
        start_time: f32,
        end_time: f32,
        beat_grid: Option<BeatGrid>,
    ) -> AnnotationPreview;
    annotation_preview::preview_graph_image(
        graph: Graph,
        track_id: String,
        venue_id: String,
        start_time: f32,
        end_time: f32,
        beat_grid: Option<BeatGrid>,
    ) -> AnnotationPreview;
    annotation_preview::view_composite_image(
        track_id: String,
        start_time: f32,
        end_time: f32,
    ) -> AnnotationPreview;

    categories::list_pattern_categories() -> Vec<PatternCategory>;

    scores::list_scores_for_track(track_id: String, venue_id: String) -> Vec<ScoreSummary>;
    scores::create_score(
        request_id: String,
        track_id: String,
        venue_id: String,
        name: Option<String>,
    ) -> Score;
    scores::ensure_venue_score(
        request_id: String,
        track_id: String,
        venue_id: String,
        name: Option<String>,
    ) -> Score;
    scores::delete_score(id: String) -> ();
    scores::list_track_scores(score_id: String) -> Vec<TrackScore>;
    scores::create_track_score(payload: CreateTrackScoreInput) -> TrackEditResult;
    scores::update_track_score(payload: UpdateTrackScoreInput) -> TrackEditResult;
    scores::delete_track_score(payload: DeleteTrackScoreInput) -> TrackEditResult;
    scores::replace_track_scores(
        score_id: String,
        track_id: String,
        base_scores: Vec<TrackScore>,
        scores: Vec<TrackScore>,
        operation_id: String,
    ) -> TrackEditResult;

    score_dsl::score_dsl_export(
        score_id: String,
        track_id: String,
        venue_id: String,
        include_clip_ids: bool,
    ) -> ScoreDslExportResponse;
    score_dsl::score_dsl_validate(
        score_id: String,
        track_id: String,
        venue_id: String,
        source: String,
    ) -> ScoreDslValidationResponse;
    score_dsl::score_dsl_import(
        score_id: String,
        track_id: String,
        venue_id: String,
        operation_id: String,
        source: String,
        base_revision: String,
    ) -> ScoreDslImportResponse;

    cloud_sync::search_patterns_remote(
        query: String,
        category_name: Option<String>,
        limit: Option<i32>,
        offset: Option<i32>,
    ) -> Vec<SearchPatternRow>;
    cloud_sync::get_display_names(uids: Vec<String>) -> HashMap<String, String>;

    stage::list_stage_pieces(venue_id: String) -> Vec<StagePiece>;
    stage::place_stage_piece(
        venue_id: String,
        mesh_path: String,
        kind: String,
        parent_piece_id: Option<String>,
        pos_x: f64,
        pos_y: f64,
        pos_z: f64,
        rot_x: f64,
        rot_y: f64,
        rot_z: f64,
        scale: Option<f64>,
        label: Option<String>,
    ) -> StagePiece;
    stage::move_stage_piece(
        id: String,
        parent_piece_id: Option<String>,
        pos_x: f64,
        pos_y: f64,
        pos_z: f64,
        rot_x: f64,
        rot_y: f64,
        rot_z: f64,
    ) -> ();
    stage::rename_stage_piece(id: String, label: String) -> ();
    stage::delete_stage_piece(id: String) -> ();

    settings::get_settings() -> AppSettings;
    settings::set_setting(key: String, value: String) -> ();

    telemetry::append_render_telemetry(entry: Value) -> ();

    venues::list_venues() -> Vec<Venue>;
    venues::get_venue(id: String) -> Venue;
    venues::create_venue(name: String, description: Option<String>) -> Venue;
    venues::update_venue(id: String, name: String, description: Option<String>) -> Venue;
    venues::delete_venue(id: String) -> ();
    venues::get_or_create_share_code(venue_id: String) -> String;
    venues::join_venue(code: String) -> Venue;
    venues::leave_venue(venue_id: String) -> ();

    midi::midi_list_cues(venue_id: String) -> Vec<Cue>;
    midi::midi_create_cue(input: CreateCueInput) -> Cue;
    midi::midi_update_cue(input: UpdateCueInput) -> Cue;
    midi::midi_delete_cue(id: String) -> ();
    midi::midi_list_modifiers(venue_id: String) -> Vec<ModifierDef>;
    midi::midi_create_modifier(input: CreateModifierInput) -> ModifierDef;
    midi::midi_delete_modifier(id: String) -> ();
    midi::midi_list_bindings(venue_id: String) -> Vec<MidiBinding>;
    midi::midi_create_binding(input: CreateBindingInput) -> MidiBinding;
    midi::midi_update_binding(input: UpdateBindingInput) -> MidiBinding;
    midi::midi_delete_binding(id: String) -> ();
    midi::midi_reload_mapping(venue_id: String) -> ();
    midi::midi_compile_cues_for_deck(
        deck_id: u8,
        track_id: String,
        venue_id: String,
    ) -> ();
    midi::midi_fire_cue(cue_id: String, target_override: Option<Target>) -> ();
    midi::midi_release_cue(cue_id: String) -> ();

    controller::controller_connect(port_name: String, venue_id: String) -> ();
    controller::controller_disconnect(venue_id: String) -> ();
    controller::controller_init_for_venue(venue_id: String) -> ();
    controller::controller_get_status(venue_id: String) -> ControllerStatus;
    controller::controller_get_state(venue_id: String) -> ControllerState;
    controller::controller_set_active(venue_id: String, active: bool) -> ();
    controller::controller_start_learn(venue_id: String) -> ();
    controller::controller_cancel_learn(venue_id: String) -> ();

    mixer::mixer_list_ports() -> Vec<String>;
    mixer::mixer_connect(
        venue_id: String,
        port_name: String,
        mapping: MixerMapping,
    ) -> ();
    mixer::mixer_disconnect(venue_id: String) -> ();
    mixer::mixer_init_for_venue(venue_id: String) -> ();
    mixer::mixer_get_status(venue_id: String) -> MixerStatus;
    mixer::mixer_open_port(venue_id: String, port_name: String) -> ();
    mixer::mixer_start_learn(venue_id: String) -> ();
    mixer::mixer_cancel_learn(venue_id: String) -> ();

    render_engine::render_set_deck_states(
        venue_id: String,
        states: Vec<PerformDeckInput>,
    ) -> ();
    render_engine::render_clear_perform(venue_id: String) -> ();
    render_engine::render_clear_active_layer(venue_id: String) -> ();
    render_engine::render_identify(targets: Vec<String>) -> ();

    perform::stagelinq_connect() -> ();
    perform::stagelinq_disconnect() -> ();
    perform::prodjlink_discover() -> Vec<DiscoveredDevice>;
    perform::prodjlink_connect(device_num: u8) -> ();
    perform::prodjlink_disconnect() -> ();
    perform::perform_match_track(
        track_network_path: String,
        venue_id: String,
    ) -> PerformTrackMatch;
    perform::perform_match_track_by_metadata(
        title: String,
        artist: String,
        bpm: f64,
        duration_secs: f64,
        venue_id: String,
    ) -> PerformTrackMatch;
    perform::render_composite_deck(
        deck_id: u8,
        track_id: String,
        venue_id: String,
    ) -> ();
    perform::render_composite_deck_unmatched(
        deck_id: u8,
        bpm: f64,
        beat_number: u8,
        position_secs: f64,
        duration_secs: f64,
        venue_id: String,
    ) -> ();

    host_audio::host_load_track(track_id: String) -> ();
    host_audio::host_load_segment(
        track_id: String,
        start_time: f32,
        end_time: f32,
        beat_grid: Option<BeatGrid>,
    ) -> ();
    host_audio::host_play() -> ();
    host_audio::host_pause() -> ();
    host_audio::host_seek(seconds: f32) -> ();
    host_audio::host_set_loop(enabled: bool) -> ();
    host_audio::host_set_loop_region(
        start_seconds: Option<f32>,
        end_seconds: Option<f32>,
    ) -> ();
    host_audio::host_set_playback_rate(rate: f32) -> ();
    host_audio::host_snapshot() -> HostAudioSnapshot;

    sync::force_quit() -> ();
    sync::sync_full() -> SyncReport;

    rekordbox::rekordbox_open_library() -> RekordboxLibraryInfo;
    rekordbox::rekordbox_list_tracks() -> Vec<RekordboxTrack>;
    rekordbox::rekordbox_list_playlists() -> Vec<RekordboxPlaylist>;
    rekordbox::rekordbox_get_playlist_tracks(playlist_id: String) -> Vec<RekordboxTrack>;
    rekordbox::rekordbox_search_tracks(query: String) -> Vec<RekordboxTrack>;
    rekordbox::rekordbox_import_tracks(track_uuids: Vec<String>) -> TrackImportResult;

    engine_dj::engine_dj_open_library(library_path: String) -> EngineDjLibraryInfo;
    engine_dj::engine_dj_list_playlists(library_path: String) -> Vec<EngineDjPlaylist>;
    engine_dj::engine_dj_list_tracks(library_path: String) -> Vec<EngineDjTrack>;
    engine_dj::engine_dj_get_playlist_tracks(
        library_path: String,
        playlist_id: i64,
    ) -> Vec<EngineDjTrack>;
    engine_dj::engine_dj_search_tracks(
        library_path: String,
        query: String,
    ) -> Vec<EngineDjTrack>;
    engine_dj::engine_dj_default_library_path() -> String;
    engine_dj::engine_dj_import_tracks(
        library_path: String,
        track_ids: Vec<i64>,
    ) -> TrackImportResult;

    tracks::import_tracks(file_paths: Vec<String>) -> TrackImportResult;
    tracks::reprocess_track(track_id: String) -> ();

    auth::get_session_item(key: String) -> Option<String>;
    auth::set_session_item(key: String, value: String) -> ();
    auth::remove_session_item(key: String) -> ();
    auth::wipe_database() -> ();
}

// -----------------------------------------------------------------------------
// Wire decoding
// -----------------------------------------------------------------------------
//
// Tauri decodes command arguments from the JS object itself, so the desktop
// adapter never comes through here. A JSON host does, and it has to reproduce
// Tauri's decoding exactly or the frontend breaks silently: handler parameters
// are `snake_case` in Rust and `camelCase` on the wire. Both spellings are
// accepted — the camel one because that is what the frontend sends, the snake
// one so a Rust-side caller writing raw frames doesn't have to guess.
//
// An explicit `null` is treated as an absent key, matching Tauri's handling of
// an omitted optional argument.

fn decode<T: DeserializeOwned>(args: &Value, snake_name: &str) -> Result<T, CommandError> {
    match lookup(args, snake_name) {
        Some(value) => serde_json::from_value(value.clone()).map_err(|error| {
            CommandError::Invalid(format!("bad argument `{snake_name}`: {error}"))
        }),
        // `Option<T>` decodes from null; anything else is genuinely missing.
        None => serde_json::from_value(Value::Null).map_err(|_| {
            CommandError::Invalid(format!("missing required argument `{snake_name}`"))
        }),
    }
}

fn lookup<'a>(args: &'a Value, snake_name: &str) -> Option<&'a Value> {
    let camel = to_camel_case(snake_name);
    args.get(&camel)
        .or_else(|| args.get(snake_name))
        .filter(|value| !value.is_null())
}

fn to_camel_case(snake: &str) -> String {
    let mut out = String::with_capacity(snake.len());
    let mut upper_next = false;
    for c in snake.chars() {
        if c == '_' {
            upper_next = true;
        } else if upper_next {
            out.push(c.to_ascii_uppercase());
            upper_next = false;
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn dispatched() -> Vec<&'static str> {
        TABLE.iter().map(|command| command.name).collect()
    }

    #[test]
    fn registry_names_are_unique() {
        let mut seen = dispatched();
        seen.sort_unstable();
        let count = seen.len();
        seen.dedup();
        assert_eq!(
            seen.len(),
            count,
            "duplicate wire name in the command table"
        );
    }

    #[test]
    fn background_import_commands_are_dispatched() {
        assert!(dispatched().contains(&"get_node_types"));
        for command in [
            "import_tracks",
            "reprocess_track",
            "engine_dj_import_tracks",
            "rekordbox_import_tracks",
        ] {
            assert!(dispatched().contains(&command), "missing `{command}`");
        }
        for command in [
            "import_tracks",
            "engine_dj_import_tracks",
            "rekordbox_import_tracks",
        ] {
            let row = TABLE.iter().find(|row| row.name == command).unwrap();
            assert_eq!(
                row.returns, "TrackImportResult",
                "Tauri adapter `{command}` regressed to the pre-dispatch result shape"
            );
        }
    }

    /// `docs/specs/ipc-manifest.{json,md}` are the command surface written
    /// down. Regenerating them here is what keeps the two from drifting: the
    /// files are rewritten from the table on every run, and a run that had to
    /// change them fails.
    #[test]
    fn ipc_manifest_matches_the_command_table() {
        if let Err(paths) = manifest::check(TABLE) {
            panic!("regenerated from the command table: {paths} — commit the new files");
        }
    }

    #[test]
    fn accepts_both_argument_spellings() {
        let camel = json!({ "venueId": "v1" });
        let snake = json!({ "venue_id": "v1" });
        assert_eq!(decode::<String>(&camel, "venue_id").unwrap(), "v1");
        assert_eq!(decode::<String>(&snake, "venue_id").unwrap(), "v1");
    }

    #[test]
    fn missing_optional_is_none_missing_required_is_named() {
        let empty = json!({});
        assert_eq!(decode::<Option<String>>(&empty, "venue_id").unwrap(), None);
        let error = decode::<String>(&empty, "venue_id").unwrap_err();
        assert_eq!(error.to_string(), "missing required argument `venue_id`");
        assert_eq!(error.kind(), "invalid");
    }

    #[test]
    fn explicit_null_reads_as_absent() {
        let nulled = json!({ "venueId": Value::Null });
        assert_eq!(decode::<Option<String>>(&nulled, "venue_id").unwrap(), None);
    }

    #[test]
    fn single_word_names_are_unchanged() {
        assert_eq!(
            decode::<String>(&json!({ "id": "p1" }), "id").unwrap(),
            "p1"
        );
    }

    /// The wire contract: an error's text is exactly the message, never a
    /// variant-decorated version of it.
    #[test]
    fn error_display_is_verbatim() {
        let conflict = CommandError::Conflict {
            expected: Some("a".into()),
            found: Some("b".into()),
            message: "heads differ".into(),
        };
        assert_eq!(String::from(conflict), "heads differ");
        assert_eq!(
            String::from(CommandError::from("plain".to_string())),
            "plain"
        );
    }
}
