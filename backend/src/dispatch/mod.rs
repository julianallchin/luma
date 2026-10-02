//! Shared command dispatcher for GPUI, the in-app agent, and backend tools.
//! Hosts construct AppServices, dispatch a command with JSON arguments, and
//! handle CommandError. The command table owns wire types and metadata.

mod error;
pub(crate) mod handlers;
#[cfg(test)]
mod manifest;
mod services;

pub use crate::engine_dj::types::EngineDjTrack as ImportedEngineDjTrack;
pub use crate::rekordbox::types::RekordboxTrack as ImportedRekordboxTrack;
pub use error::CommandError;
pub use services::system_track_sources;
pub use services::{AppServices, EventSink, Events, SharedServices, TrackSources};

use serde::de::DeserializeOwned;
use serde_json::Value;

/// Declare a command once; get the JSON dispatch arm and
/// the name registry from it.
///
/// Each row reads as `<handler module>::<wire name>(<args>) -> <return type>`.
/// The wire name *is* the handler function name, and the argument names are the
/// handler's parameter names, which the dispatcher maps `snake_case` → `camelCase` on
/// the wire.
///
/// Every handler is `async`, including the ones whose bodies never await —
/// awaiting a synchronous body costs nothing and keeps the table free of
/// special cases.
macro_rules! commands {
    ($( $domain:ident :: $name:ident ( $($arg:ident : $ty:ty),* $(,)? ) -> $ret:ty );* $(;)?) => {
        /// Run a command by its wire name against `services`.
        ///
        /// `args` is a JSON object of named arguments;
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
                        // Keep each command's poll temporaries in its own frame.
                        // In debug builds, one async match over every handler
                        // otherwise reserves their combined stack space (>1 MiB).
                        fn run<'a>(services: &'a AppServices, _args: &'a Value)
                            -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, CommandError>> + Send + 'a>>
                        {
                            Box::pin(async move {
                                $( let $arg: $ty = decode(_args, stringify!($arg))?; )*
                                let value: $ret = handlers::$domain::$name(services, $($arg),*).await?;
                                serde_json::to_value(value).map_err(|error| {
                                    CommandError::Internal(format!(
                                        "failed to serialize `{}` result: {error}", stringify!($name)
                                    ))
                                })
                            })
                        }
                        run(services, args).await
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

use std::collections::BTreeMap;

use crate::engine_dj::types::{EngineDjLibraryInfo, EngineDjPlaylist, EngineDjTrack};
use crate::host_audio::HostAudioSnapshot;
use crate::models::agent_execution::{PythonCellResult, PythonScopeInput};
use crate::models::agent_threads::{
    AgentThread, AgentThreadMessage, AgentThreadUsage, AppendAgentThreadMessagesInput,
    CreateAgentThreadInput,
};
use crate::models::distribute::{DistributeLayout, DistributeReport};
use crate::models::fixtures::{FixtureDefinition, FixtureEntry, FixtureFacing, PatchedFixture};
use crate::models::folders::Folder;
use crate::models::groups::{FixtureGroup, GroupTreeNode};
use crate::models::midi::{
    ControllerState, ControllerStatus, CreateBindingInput, CreateModifierInput, MidiBinding,
    ModifierDef, UpdateBindingInput,
};
use crate::models::mixer::{MixerMapping, MixerStatus};
use crate::models::node_graph::BeatGrid;
use crate::models::patch::ArtNetNode;
use crate::models::patch::{AutoPatchReport, UniverseCell, UniverseOutput};
use crate::models::patterns::AnnotationPreview;
use crate::models::perform::PerformTrackMatch;
use crate::models::scores::{Score, ScoreSummary};
use crate::models::selection::Selection;
use crate::models::sync::SyncStatus;
use crate::models::tracks::{
    BeatValidation, BeatValidationReason, BeatValidationVerdict, TrackBrowserRow,
    TrackImportResult, TrackSummary,
};
use crate::models::universe::UniverseState;
use crate::models::venue_graph::{PlacementReport, Reach, ResolvedVenue, VenueGraphRows};
use crate::models::venues::Venue;
use crate::models::waveforms::TrackWaveform;

use crate::rekordbox::types::{RekordboxLibraryInfo, RekordboxPlaylist, RekordboxTrack};
use crate::render_engine::PerformDeckInput;
use crate::settings::AppSettings;
pub use handlers::scores::prepare_score_clip_preview;
/// Large native audio payload; retains the dispatcher's visibility checks.
pub use handlers::waveforms::get_track_waveform_signal;
use luma_render::scene_desc::{VenueEnvironment, VenueHaze};
use prodjlink::DiscoveredDevice;

commands! {
    composable_patterns::preview_composable_pattern(request: Value) -> Value;
    composable_patterns::selection_cells(
        venue_id: String,
        selection: Selection,
        seed: u64,
    ) -> Vec<luma_patterns::Cell>;

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
    agent_threads::agent_thread_set_model(thread_id: String, selection: crate::agent::engine::catalog::Selection) -> AgentThread;
    agent_threads::agent_thread_set_actor(thread_id: String, actor: String) -> ();
    agent_threads::agent_thread_record_usage(usage: AgentThreadUsage) -> ();
    agent_threads::agent_thread_delete(thread_id: String) -> ();

    agent_execution::run_python_cell(
        thread_id: String,
        turn_message_id: String,
        code: String,
        scope: PythonScopeInput,
    ) -> PythonCellResult;
    agent_execution::cancel_python_cell(thread_id: String) -> bool;

    fixtures::get_patched_fixtures(venue_id: String) -> Vec<PatchedFixture>;
    fixtures::get_fixture_facings(venue_id: String) -> Vec<FixtureFacing>;
    fixtures::initialize_fixtures() -> usize;
    fixtures::search_fixtures(query: String, offset: usize, limit: usize) -> Vec<FixtureEntry>;
    fixtures::get_fixture_definition(path: String) -> FixtureDefinition;
    fixtures::set_fixture_address(
        venue_id: String,
        id: String,
        universe: i64,
        address: i64,
    ) -> ();
    fixtures::auto_patch(venue_id: String) -> AutoPatchReport;
    fixtures::universe_occupancy(venue_id: String, universe: i64) -> Vec<UniverseCell>;
    fixtures::universes_in_use(venue_id: String) -> Vec<u16>;
    fixtures::set_fixture_mode(
        venue_id: String,
        id: String,
        mode_name: String,
        allow_move: bool,
    ) -> PatchedFixture;
    fixtures::set_address_pinned(venue_id: String, id: String, pinned: bool) -> ();
    fixtures::remove_patched_fixture(venue_id: String, id: String) -> ();
    fixtures::rename_patched_fixture(venue_id: String, id: String, label: String) -> ();

    group_references::missing_venue_groups(venue_id: String) -> Vec<crate::models::groups::MissingGroup>;
    group_references::resolve_venue_group(venue_id: String, missing: String, replacement: Option<String>, fixtures: Vec<String>) -> ();
    folders::list_folders(venue_id: String) -> Vec<Folder>;
    folders::create_folder(venue_id: String, name: String) -> Folder;
    folders::rename_folder(folder_id: String, name: String) -> ();
    folders::delete_folder(folder_id: String) -> ();
    folders::set_folder_track(folder_id: String, track_id: String, linked: bool) -> ();

    groups::generate_venue_groups(venue_id: String) -> ();
    groups::save_venue_group(venue_id: String, group_id: Option<String>, label: String, added: Vec<String>, removed: Vec<String>) -> ();
    groups::list_groups(venue_id: String) -> Vec<FixtureGroup>;
    groups::delete_group(id: String) -> ();
    groups::list_group_tree(venue_id: String) -> Vec<GroupTreeNode>;
    groups::preview_selection_query(
        venue_id: String,
        query: String,
        seed: Option<u64>,
    ) -> Vec<PatchedFixture>;
    groups::highlight_selection(
        venue_id: String,
        selection: Selection,
    ) -> UniverseState;

    artnet::start_discovery() -> ();
    artnet::stop_discovery() -> ();
    artnet::get_discovered_nodes() -> Vec<ArtNetNode>;
    artnet::list_outputs() -> Vec<UniverseOutput>;
    artnet::bind_output(
        universe: i64,
        node_ip: String,
        node_port: i64,
        port_address: i64,
        node_name: Option<String>,
    ) -> ();
    artnet::unbind_output(universe: i64) -> ();

    waveforms::get_track_waveform(track_id: String) -> TrackWaveform;

    tracks::list_tracks() -> Vec<TrackSummary>;
    tracks::list_tracks_enriched(venue_id: Option<String>) -> Vec<TrackBrowserRow>;
    tracks::get_track_beats(track_id: String) -> Option<BeatGrid>;
    tracks::get_track_beat_validation(track_id: String) -> Option<BeatValidation>;
    tracks::set_track_beat_validation(track_id: String, grid: BeatGrid, verdict: BeatValidationVerdict, reason: Option<BeatValidationReason>) -> ();

    compositor::composite_track(
        score_id: String,
        graph_score: Option<luma_patterns::Score>,
    ) -> ();
    compositor::leave_track(score_id: String) -> ();

    scores::list_scores_for_track(track_id: String, venue_id: String) -> Vec<ScoreSummary>;
    scores::list_scores_across_venues(track_id: String) -> Vec<ScoreSummary>;
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
    scores::rename_score(score_id: String, name: String) -> ();
    scores::get_score_document(score_id: String) -> Option<luma_patterns::Score>;
    scores::apply_score_document(score_id: String, score: luma_patterns::Score) -> ();
    scores::preview_score_clip(score_id: String, clip_id: String, score: Option<luma_patterns::Score>) -> AnnotationPreview;
    scores::clip_graph_definitions() -> Value;
    scores::clip_presets() -> Value;

    distribute::distribute(
        venue_id: String,
        host_node_id: Option<String>,
        host_socket: Option<String>,
        fixture_path: String,
        mode_name: String,
        count: usize,
        layout: DistributeLayout,
        label_prefix: Option<String>,
    ) -> DistributeReport;
    distribute::redistribute(
        venue_id: String,
        member_node_id: String,
        count: usize,
        layout: DistributeLayout,
    ) -> DistributeReport;
    stage::get_venue_graph(venue_id: String) -> VenueGraphRows;
    stage::restore_graph(
        venue_id: String,
        rows: VenueGraphRows,
        patch: Vec<PatchedFixture>,
    ) -> ResolvedVenue;
    stage::get_resolved_venue(venue_id: String) -> ResolvedVenue;
    stage::attach(
        venue_id: String,
        kind: String,
        catalog_ref: Option<String>,
        label: Option<String>,
        parent_id: String,
        my_socket: Option<String>,
        their_socket: String,
        yaw: Option<f64>,
        params: Option<BTreeMap<String, f64>>,
    ) -> PlacementReport;
    stage::constrain(
        venue_id: String,
        node_id: String,
        my_socket: String,
        target_node: String,
        target_socket: String,
    ) -> PlacementReport;
    stage::place_free(
        venue_id: String,
        kind: String,
        catalog_ref: Option<String>,
        label: Option<String>,
        surface_node_id: Option<String>,
        surface_socket: Option<String>,
        my_socket: Option<String>,
        u: f64,
        v: f64,
        yaw: Option<f64>,
        trim: Option<f64>,
        params: Option<BTreeMap<String, f64>>,
    ) -> PlacementReport;
    stage::extend(
        venue_id: String,
        node_id: String,
        socket: String,
        length_m: Option<f64>,
    ) -> PlacementReport;
    stage::extend_reach(
        venue_id: String,
        node_id: String,
        socket: String,
    ) -> Option<Reach>;
    stage::duplicate(
        venue_id: String,
        node_id: String,
        parent_id: String,
        their_socket: String,
        flip: Option<bool>,
    ) -> PlacementReport;
    stage::set_params(
        venue_id: String,
        node_id: String,
        params: BTreeMap<String, f64>,
        label: Option<String>,
    ) -> PlacementReport;
    stage::delete_subtree(venue_id: String, node_id: String) -> ResolvedVenue;

    settings::get_settings() -> AppSettings;
    settings::set_setting(key: String, value: String) -> ();

    telemetry::append_render_telemetry(entry: Value) -> ();

    venues::list_venues() -> Vec<Venue>;
    venues::get_venue(id: String) -> Venue;
    venues::create_venue(name: String, description: Option<String>) -> Venue;
    venues::set_venue_environment(venue_id: String, environment: VenueEnvironment) -> ();
    venues::set_venue_haze(venue_id: String, haze: VenueHaze) -> ();

    midi::midi_list_modifiers(venue_id: String) -> Vec<ModifierDef>;
    midi::midi_create_modifier(input: CreateModifierInput) -> ModifierDef;
    midi::midi_delete_modifier(id: String) -> ();
    midi::midi_list_bindings(venue_id: String) -> Vec<MidiBinding>;
    midi::midi_create_binding(input: CreateBindingInput) -> MidiBinding;
    midi::midi_update_binding(input: UpdateBindingInput) -> MidiBinding;
    midi::midi_delete_binding(id: String) -> ();
    midi::midi_reload_mapping(venue_id: String) -> ();

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

    host_audio::host_load_track(track_id: String, session: u64) -> ();
    host_audio::host_load_segment(
        track_id: String,
        session: u64,
        start_time: f32,
        end_time: f32,
    ) -> ();
    host_audio::host_play(session: u64, seconds: f32) -> ();
    host_audio::host_pause(session: u64) -> ();
    host_audio::host_seek(session: u64, seconds: f32) -> ();
    host_audio::host_set_playback_range(session: u64, start_seconds: f32, end_seconds: f32, looping: bool) -> ();
    host_audio::host_set_loop_region(
        session: u64,
        start_seconds: Option<f32>,
        end_seconds: Option<f32>,
    ) -> ();
    host_audio::host_set_playback_rate(session: u64, rate: f32) -> ();
    host_audio::host_snapshot() -> HostAudioSnapshot;

    sync::sync_status() -> SyncStatus;

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

    auth::current_account() -> Option<crate::database::local::auth::AuthAccount>;
    auth::send_login_code(email: String) -> ();
    auth::verify_login_code(email: String, code: String) -> String;
    auth::get_session_item(key: String) -> Option<String>;
    auth::set_session_item(key: String, value: String) -> ();
    auth::remove_session_item(key: String) -> ();
    auth::wipe_database() -> ();
}

// -----------------------------------------------------------------------------
// Wire decoding
// -----------------------------------------------------------------------------
//
// Handler parameters are snake_case in Rust and camelCase on the wire.
// Accept both spellings for native callers and tools. An explicit null is
// treated as an absent key for optional arguments.

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
    fn dispatch_does_not_embed_all_command_futures() {
        fn size<F: std::future::Future>(
            _: impl FnOnce(&'static AppServices, &'static Value) -> F,
        ) -> usize {
            std::mem::size_of::<F>()
        }
        let bytes = size(|services, args| dispatch(services, "", args));
        assert!(bytes <= 1024, "dispatch embeds handler state ({bytes} bytes); keep per-command futures behind the boxed helper boundary");
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
                "Dispatcher `{command}` regressed to the pre-dispatch result shape"
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
