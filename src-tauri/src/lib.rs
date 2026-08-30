pub mod agent;
pub mod agent_execution;
pub mod annotation_preview;
mod artnet;
pub mod audio;
mod beat_worker;
mod canonical_json;
mod classifier_worker;
mod cmd_util;
mod compositor;
pub mod config;
mod controller_compositor;
mod controller_manager;
pub mod database;
pub mod dispatch;
mod engine_dj;
pub mod eval;
mod ffmpeg_env;
pub mod fixtures;
mod genre_worker;
pub mod headless_host;
pub mod host_audio;
mod mert_worker;
mod mixer_manager;
pub mod models;
mod n2n_worker;
pub mod node_graph;
mod preprocessing;
pub use preprocessing::AnalysisTaskGroup;
mod prodjlink_manager;
pub mod python_env;
pub mod recording;
mod rekordbox;
mod render_engine;
mod root_worker;
pub mod services;
pub mod settings;
pub mod stage_render;
mod stagelinq_manager;
mod stem_worker;
pub mod storage;
mod sync;
mod topo;
/// The venue graph: loading it, solving it, and converting what came before.
pub mod venue_graph;

/// The app's version, from `Cargo.toml`. The Tauri host reads it through
/// `@tauri-apps/api/app`'s `getVersion()`; a non-Tauri host has no such plugin,
/// so the same string is published here.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

use tauri::{Emitter, Manager};
use tauri_plugin_dialog::init as dialog_init;

use crate::services::fixtures::FixtureState;
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let _sentry_guard = if cfg!(not(debug_assertions)) {
        Some(sentry::init((
            "https://01abb3c36939abaf0327f3117d387f98@o4511152136257536.ingest.us.sentry.io/4511152144711680",
            sentry::ClientOptions {
                release: sentry::release_name!(),
                ..Default::default()
            },
        )))
    } else {
        None
    };

    tauri::Builder::default()
        .plugin(
            tauri_plugin_log::Builder::new()
                .targets([
                    tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Stdout),
                    tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::LogDir {
                        file_name: Some("luma".into()),
                    }),
                    tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Webview),
                ])
                .level(log::LevelFilter::Warn)
                .level_for("luma", log::LevelFilter::Debug)
                .build(),
        )
        .plugin(tauri_plugin_opener::init()) // open files & URLs in browser
        .plugin(dialog_init()) // native OS file dialogs for uploading
        .plugin(tauri_plugin_macos_fps::init()) // unlock 120Hz+ on ProMotion displays
        .plugin(tauri_plugin_updater::Builder::new().build()) // auto-updates via GitHub Releases
        .plugin(tauri_plugin_process::init()) // relaunch after update
        // Wrap the Tauri event dispatcher so a race between event emission and
        // handler unregistration can never crash the WKWebView content process.
        // Runs before any page JS, after __TAURI_INTERNALS__ is initialised.
        .append_invoke_initialization_script(
            r#";(function() {
                var t = window.__TAURI_INTERNALS__;
                if (!t || !t.runCallback) return;
                var orig = t.runCallback.bind(t);
                t.runCallback = function(id, data) {
                    try { return orig(id, data); } catch(e) {}
                };
            })();
            "#,
        )
        .setup(|app| {
            let app_handle = app.handle();

            #[cfg(target_os = "macos")]
            {
                use tauri::menu::{Menu, MenuItemBuilder, PredefinedMenuItem, SubmenuBuilder};

                let settings = MenuItemBuilder::new("Settings...")
                    .id("settings")
                    .accelerator("CmdOrCtrl+,")
                    .build(app_handle)?;

                let app_menu = SubmenuBuilder::new(app_handle, "Luma")
                    .item(&PredefinedMenuItem::about(app_handle, None, None)?)
                    .separator()
                    .item(&settings)
                    .separator()
                    .item(&PredefinedMenuItem::services(app_handle, None)?)
                    .separator()
                    .item(&PredefinedMenuItem::hide(app_handle, None)?)
                    .item(&PredefinedMenuItem::hide_others(app_handle, None)?)
                    .item(&PredefinedMenuItem::show_all(app_handle, None)?)
                    .separator()
                    .item(&PredefinedMenuItem::quit(app_handle, None)?)
                    .build()?;

                let file_menu = SubmenuBuilder::new(app_handle, "File")
                    .item(&PredefinedMenuItem::close_window(app_handle, None)?)
                    .build()?;

                let edit_menu = SubmenuBuilder::new(app_handle, "Edit")
                    .item(&PredefinedMenuItem::undo(app_handle, None)?)
                    .item(&PredefinedMenuItem::redo(app_handle, None)?)
                    .separator()
                    .item(&PredefinedMenuItem::cut(app_handle, None)?)
                    .item(&PredefinedMenuItem::copy(app_handle, None)?)
                    .item(&PredefinedMenuItem::paste(app_handle, None)?)
                    .item(&PredefinedMenuItem::select_all(app_handle, None)?)
                    .build()?;

                let view_menu = SubmenuBuilder::new(app_handle, "View")
                    .item(&PredefinedMenuItem::fullscreen(app_handle, None)?)
                    .build()?;

                let window_menu = SubmenuBuilder::new(app_handle, "Window")
                    .item(&PredefinedMenuItem::minimize(app_handle, None)?)
                    .separator()
                    .item(&PredefinedMenuItem::separator(app_handle)?)
                    .build()?;

                let menu = Menu::new(app_handle)?;
                menu.append(&app_menu)?;
                menu.append(&file_menu)?;
                menu.append(&edit_menu)?;
                menu.append(&view_menu)?;
                menu.append(&window_menu)?;

                app.set_menu(menu)?;

                app.on_menu_event(move |app, event| {
                    if event.id() == "settings" {
                        let _ = app.emit("open-settings", ());
                    }
                });
            }

            // Resolve bundled ffmpeg path for audio decoding + Python workers
            ffmpeg_env::init(app_handle);

            // initializing luma.db
            let db = tauri::async_runtime::block_on(async {
                let db = database::init_app_db(app_handle).await?;
                Ok::<_, String>(db)
            })?;
            let state_db = tauri::async_runtime::block_on(async {
                let db = database::init_state_db(app_handle).await?;
                Ok::<_, String>(db)
            })?;

            // store shared state in the Manager
            app.manage(db);
            app.manage(state_db);

            let startup_activation = {
                let pool = app.state::<database::Db>().inner().0.clone();
                let state_pool = app
                    .state::<database::local::state::StateDb>()
                    .inner()
                    .0
                    .clone();
                let recovery = tauri::async_runtime::block_on(async {
                    let mut state_connection = state_pool.acquire().await.map_err(|error| {
                        format!("Failed to lock authenticated session at startup: {error}")
                    })?;
                    database::local::auth::recover_committed_signout(
                        &pool,
                        &mut state_connection,
                    )
                    .await
                });
                match recovery {
                    Err(error) => {
                        return Err(format!(
                            "committed sign-out recovery failed; refusing to expose the app: {error}"
                        )
                        .into());
                    }
                    Ok(true) => {
                        // The app DB is the crash journal. Preserve its closed,
                        // principal-bound row until the renderer consumes the
                        // recovered one-shot transition.
                        None
                    }
                    Ok(false) => {
                        let principal = match tauri::async_runtime::block_on(async {
                        database::local::auth::get_session_item(
                            &state_pool,
                            database::local::auth::SUPABASE_SESSION_KEY,
                        )
                        .await?;
                        database::local::auth::load_verified_principal(&state_pool).await
                        }) {
                            Ok(principal) => principal,
                            Err(error) => {
                                eprintln!(
                                    "[auth] persisted session could not be host-verified; signed writes remain closed: {error}"
                                );
                                None
                            }
                        };
                        Some(tauri::async_runtime::block_on(
                            database::local::auth::arm_write_admission_for_identity_switch(
                                &pool,
                                principal
                                    .as_ref()
                                    .map(|principal| principal.user_id.as_str()),
                            ),
                        )?)
                    }
                }
            };

            let authored_storage = storage::StorageRoot::from_app(app_handle)?;
            let authored =
                services::authored_documents::AuthoredDocuments::new(authored_storage.clone());
            app.manage(authored.clone());

            // Sync can observe a terminal agent-thread deletion on its first
            // pull. Register the two process-local resource stores before the
            // sync engine starts so every production pull can finish the same
            // durable cleanup used at startup and by the local delete command.
            // Held behind `Arc`: neither of these two shares its interior, so
            // every holder must reference one instance or the state forks.
            let workspaces = std::sync::Arc::new(agent_execution::tauri_env::workspace_service(
                app_handle,
                &authored_storage,
            ));
            let graph_runs = std::sync::Arc::new(agent_execution::graph_runs::GraphRunStore::new());
            app.manage(workspaces.clone());
            app.manage(graph_runs.clone());

            // A checkpoint-era live projection must become canonical revision
            // bytes before any sync worker can publish catalog state. A closed
            // admission means an identity transition is still being recovered;
            // its renderer-session path performs the same gate when admission
            // reopens.
            match tauri::async_runtime::block_on(database::local::auth::admitted_principal(
                &app.state::<database::Db>().inner().0,
            )) {
                Ok(principal) => {
                    if let Err(error) = tauri::async_runtime::block_on(
                        authored.bootstrap_live_projections(
                        &app.state::<database::Db>().inner().0,
                        principal.as_deref(),
                        ),
                    ) {
                        let close = startup_activation.as_ref().map(|activation| {
                            tauri::async_runtime::block_on(
                                database::local::auth::suspend_write_admission_for_rollback(
                                    &app.state::<database::Db>().inner().0,
                                    activation,
                                ),
                            )
                        });
                        let close_note = match close {
                            Some(Ok(_)) => "signed writes were closed".to_string(),
                            Some(Err(close_error)) => format!(
                                "closing the activated admission also failed: {close_error}"
                            ),
                            None => "admission was already closed".to_string(),
                        };
                        return Err(format!(
                            "authored projection bootstrap failed; refusing to start sync ({close_note}): {error}"
                        )
                        .into());
                    }
                }
                Err(error) if error == "App database admission is closed" => {
                    // Committed-signout recovery deliberately keeps admission
                    // closed until the renderer consumes the one-shot state;
                    // that activation path runs the same bootstrap fence.
                }
                Err(error) => {
                    let close_note = if let Some(activation) = startup_activation.as_ref() {
                        match tauri::async_runtime::block_on(
                            database::local::auth::suspend_write_admission_for_rollback(
                                &app.state::<database::Db>().inner().0,
                                activation,
                            ),
                        ) {
                            Ok(_) => "signed writes were closed".to_string(),
                            Err(close_error) => format!(
                                "closing the activated admission also failed: {close_error}"
                            ),
                        }
                    } else {
                        "admission was already closed".to_string()
                    };
                    return Err(format!(
                        "failed to inspect authored bootstrap admission; refusing to start sync ({close_note}): {error}"
                    )
                    .into());
                }
            }

            // The one registry: the startup sweep, the sync host and the
            // command services all read the same leases, so a workspace is
            // never retired out from under a running child.
            let subagents: std::sync::Arc<agent::subagent::SubagentRegistry> =
                std::sync::Arc::default();

            // Sync engine — create after both DB pools are available
            let sync_engine = {
                let db_ref: &database::Db = app.state::<database::Db>().inner();
                let state_ref: &database::local::state::StateDb =
                    app.state::<database::local::state::StateDb>().inner();
                let supabase_client = crate::database::remote::common::SupabaseClient::new(
                    config::SUPABASE_URL.to_string(),
                    config::SUPABASE_ANON_KEY.to_string(),
                );
                let engine = sync::orchestrator::SyncEngine::new(
                    db_ref.0.clone(),
                    state_ref.0.clone(),
                    std::sync::Arc::new(supabase_client),
                    authored.clone(),
                );
                let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
                tauri::async_runtime::spawn(sync::push::run_sync_loop(
                    engine.pool().clone(),
                    engine.state_pool().clone(),
                    engine.remote().clone(),
                    engine.push_notify.clone(),
                    engine.sync_lock.clone(),
                    engine.authored().clone(),
                    sync::host::SyncHost {
                        storage: authored_storage.clone(),
                events: dispatch::tauri_events(app_handle),
                        workspaces: workspaces.clone(),
                        graph_runs: graph_runs.clone(),
                        subagents: subagents.clone(),
                    },
                    shutdown_rx,
                ));
                app.manage(engine.clone());
                app.manage(shutdown_tx);
                engine
            };

            // ArtNet Manager
            let artnet_manager = std::sync::Arc::new(artnet::ArtNetManager::new(app_handle.clone()));
            app.manage(artnet_manager.clone());

            // Host audio state - audio playback only
            let host_audio = host_audio::HostAudioState::default();
            host_audio.spawn_broadcaster(app_handle.clone());
            let _ = tauri::async_runtime::block_on(host_audio::reload_settings(
                &host_audio,
                &app.state::<database::Db>().inner().0,
            ));
            app.manage(host_audio.clone());

            // Render engine - rendering, universe state, ArtNet output
            let render_engine = render_engine::RenderEngine::default();
            render_engine.spawn_render_loop(app_handle.clone());
            // Clone before move so ControllerManager can share the same inner Arc
            let midi_mgr = std::sync::Arc::new(controller_manager::ControllerManager::new(
                render_engine.clone(),
                Some(artnet_manager.clone()),
            ));
            let mixer_mgr = std::sync::Arc::new(mixer_manager::MixerManager::new());
            // Perform-deck telemetry. Reached only through `AppServices`, so
            // unlike the MIDI managers these are not `manage`d.
            let stagelinq_mgr = std::sync::Arc::new(stagelinq_manager::StageLinqManager::new());
            let prodjlink_mgr = std::sync::Arc::new(prodjlink_manager::ProDJLinkManager::new());
            app.manage(render_engine.clone());

            // Stem Cache for graph execution
            let stem_cache = audio::StemCache::new();
            app.manage(stem_cache.clone());
            let analysis_tasks = preprocessing::AnalysisTaskGroup::new();
            app.manage(analysis_tasks.clone());

            // Shared FFT Service for audio analysis
            let fft_service = audio::FftService::new();
            app.manage(fft_service.clone());

            storage::StorageRoot::from_app(app_handle)?.ensure_track_storage()?;
            {
                let pool = app.state::<database::Db>().inner().0.clone();
                if let Err(error) = tauri::async_runtime::block_on(
                    services::tracks::recover_track_deletions(&pool, &storage::StorageRoot::from_app(app_handle)?),
                ) {
                    // A damaged deletion stage must not brick the application.
                    // Leave it in place for a later retry/manual recovery and
                    // refuse new track deletions until it is resolved.
                    eprintln!("[tracks] startup deletion recovery: {error}");
                }
            }

            // A thread deletion is a durable terminal state, not an
            // in-memory UI gesture. Resume any cleanup interrupted by a crash
            // now that all owned-resource services are available. The registry
            // is still empty here, so every active subagent workspace is
            // stranded by definition.
            let db = app.state::<database::Db>().inner().clone();
            if let Err(error) =
                tauri::async_runtime::block_on(agent_execution::thread_cleanup::recover_threads(
                    &db.0,
                    &authored,
                    &workspaces,
                    &graph_runs,
                    &subagents,
                ))
            {
                eprintln!("[agent-threads] startup recovery: {error}");
            }

            let fixtures_root = services::fixtures::resolve_fixtures_root(app_handle)?;
            let fixture_state = std::sync::Arc::new(FixtureState::empty());

            // The command-dispatcher seam. A dispatched command reaches its
            // services through this struct; the generated `#[tauri::command]`
            // wrappers in `dispatch::adapter` are the only Tauri-shaped code in
            // front of them. Assembled from the singletons above rather than
            // built here, because several are already wired into loops.
            app.manage(dispatch::AppServices {
                db,
                state_db: app
                    .state::<database::local::state::StateDb>()
                    .inner()
                    .clone(),
                authored: authored.clone(),
                workspaces,
                graph_runs,
                analysis_tasks,
                workers: preprocessing::WorkerEnvironment::new(
                    app_handle
                        .path()
                        .app_cache_dir()
                        .map_err(|error| format!("Failed to locate app cache dir: {error}"))?,
                    app_handle.path().resource_dir().ok(),
                )
                .wait_for_setup(),
                track_sources: dispatch::system_track_sources(),
                fft: fft_service,
                stem_cache,
                render_engine,
                controller: std::sync::Arc::clone(&midi_mgr),
                mixer: std::sync::Arc::clone(&mixer_mgr),
                stagelinq: stagelinq_mgr,
                prodjlink: prodjlink_mgr,
                sync: sync_engine,
                artnet: Some(artnet_manager),
                host_audio,
                storage: authored_storage,
                fixtures_root: fixtures_root.clone(),
                fixtures: std::sync::Arc::clone(&fixture_state),
                agent_turns: std::sync::Arc::default(),
                subagents,
                events: dispatch::tauri_events(app_handle),
                host: dispatch::tauri_host(app_handle),
                fixture_principal: None,
            }
            // Shared, because the agent's turn loop outlives the command that
            // starts it and cannot borrow a command body's `&AppServices`.
            .into_shared());

            // Build fixture index eagerly so search works before any UI page mounts
            if let Err(e) = tauri::async_runtime::block_on(
                crate::services::fixtures::initialize_fixtures(&fixtures_root, &fixture_state),
            ) {
                eprintln!("Failed to initialize fixture index: {e}");
            }

            // MIDI Managers
            app.manage(midi_mgr);
            app.manage(mixer_mgr);

            // Start Python environment setup in the background
            python_env::setup_python_env_background(app_handle.clone());

            // Queue analysis for tracks with missing/stale artifacts against
            // the same cache/resource paths the environment setup owns.
            {
                let pool = app.state::<database::Db>().inner().0.clone();
                let storage = storage::StorageRoot::from_app(app_handle)?;
                let workers = preprocessing::WorkerEnvironment::new(
                    app_handle
                        .path()
                        .app_cache_dir()
                        .map_err(|error| format!("Failed to locate app cache dir: {error}"))?,
                    app_handle.path().resource_dir().ok(),
                )
                .wait_for_setup();
                let events = dispatch::tauri_events(app_handle);
                let cache = app.state::<audio::StemCache>().inner().clone();
                let tasks = app
                    .state::<preprocessing::AnalysisTaskGroup>()
                    .inner()
                    .clone();
                let epoch = tasks.current_epoch()?;
                tasks.spawn(epoch, move |analysis| async move {
                    if let Err(e) =
                        preprocessing::scheduler::reconcile_on_startup(
                            pool,
                            storage,
                            workers,
                            events,
                            cache,
                            analysis,
                        )
                        .await
                    {
                        log::warn!("[startup] Preprocessing reconciliation failed: {e}");
                    }
                })?;
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // registers routes for frontend
            dispatch::adapter::get_node_types,
            dispatch::adapter::run_graph,
            dispatch::adapter::preview_pattern,
            dispatch::adapter::get_pattern,
            dispatch::adapter::list_patterns,
            dispatch::adapter::create_pattern,
            dispatch::adapter::update_pattern,
            dispatch::adapter::set_pattern_category,
            dispatch::adapter::get_pattern_graph_document,
            dispatch::adapter::get_pattern_args,
            dispatch::adapter::save_pattern_graph_document,
            dispatch::adapter::delete_pattern,
            dispatch::adapter::score_dsl_export,
            dispatch::adapter::score_dsl_validate,
            dispatch::adapter::score_dsl_import,
            dispatch::adapter::list_pattern_categories,
            dispatch::adapter::list_tracks,
            dispatch::adapter::list_tracks_enriched,
            dispatch::adapter::get_venue_annotation_counts,
            dispatch::adapter::import_tracks,
            dispatch::adapter::reprocess_track,
            dispatch::adapter::delete_track,
            dispatch::adapter::get_track_beats,
            dispatch::adapter::get_track_bar_classifications,
            dispatch::adapter::get_track_drum_onsets,
            dispatch::adapter::get_classifier_thresholds,
            dispatch::adapter::get_track_audio_base64,
            dispatch::adapter::update_track_metadata,
            // Host audio commands
            dispatch::adapter::host_load_segment,
            dispatch::adapter::host_load_track,
            dispatch::adapter::host_play,
            dispatch::adapter::host_pause,
            dispatch::adapter::host_seek,
            dispatch::adapter::host_set_loop,
            dispatch::adapter::host_set_loop_region,
            dispatch::adapter::host_set_playback_rate,
            dispatch::adapter::host_snapshot,
            dispatch::adapter::list_scores_for_track,
            dispatch::adapter::list_scores_across_venues,
            dispatch::adapter::create_score,
            dispatch::adapter::ensure_venue_score,
            dispatch::adapter::list_track_scores,
            dispatch::adapter::create_track_score,
            dispatch::adapter::update_track_score,
            dispatch::adapter::delete_score,
            dispatch::adapter::delete_track_score,
            dispatch::adapter::replace_track_scores,
            dispatch::adapter::get_track_waveform,
            dispatch::adapter::get_track_waveform_window,
            dispatch::adapter::reprocess_waveform,
            dispatch::adapter::initialize_fixtures,
            dispatch::adapter::search_fixtures,
            dispatch::adapter::get_fixture_definition,
            dispatch::adapter::patch_fixture,
            dispatch::adapter::get_patched_fixtures,
            dispatch::adapter::get_fixture_facings,
            dispatch::adapter::set_fixture_address,
            dispatch::adapter::set_fixture_mode,
            dispatch::adapter::set_address_pinned,
            dispatch::adapter::fixture_role,
            dispatch::adapter::auto_patch,
            dispatch::adapter::universe_occupancy,
            dispatch::adapter::universes_in_use,
            dispatch::adapter::next_addresses,
            dispatch::adapter::remove_patched_fixture,
            dispatch::adapter::rename_patched_fixture,
            // Stage pieces
            dispatch::adapter::get_venue_graph,
            dispatch::adapter::get_resolved_venue,
            dispatch::adapter::venue_tiles,
            dispatch::adapter::attach,
            dispatch::adapter::reattach,
            dispatch::adapter::constrain,
            dispatch::adapter::place_free,
            dispatch::adapter::detach,
            dispatch::adapter::distribute,
            dispatch::adapter::set_params,
            dispatch::adapter::delete_subtree,
            dispatch::adapter::extend,
            dispatch::adapter::extend_reach,
            dispatch::adapter::duplicate,
            dispatch::adapter::describe_venue,
            dispatch::adapter::stage_catalog,
            // Groups
            dispatch::adapter::create_group,
            dispatch::adapter::list_groups,
            dispatch::adapter::list_group_tree,
            dispatch::adapter::rename_group_node,
            dispatch::adapter::move_group_node,
            dispatch::adapter::merge_group_nodes,
            dispatch::adapter::reset_group_node,
            dispatch::adapter::update_group,
            dispatch::adapter::delete_group,
            dispatch::adapter::add_fixture_to_group,
            dispatch::adapter::remove_fixture_from_group,
            dispatch::adapter::get_grouped_hierarchy,
            dispatch::adapter::preview_selection_query,
            dispatch::adapter::highlight_selection,
            dispatch::adapter::get_ungrouped_fixtures,
            dispatch::adapter::update_movement_config,
            dispatch::adapter::composite_track,
            dispatch::adapter::leave_track,
            // Annotation Previews
            dispatch::adapter::generate_annotation_previews,
            dispatch::adapter::preview_annotation,
            dispatch::adapter::preview_pattern_image,
            dispatch::adapter::preview_graph_image,
            dispatch::adapter::view_composite_image,
            // Settings
            dispatch::adapter::get_settings,
            dispatch::adapter::set_setting,
            // ArtNet
            dispatch::adapter::start_discovery,
            dispatch::adapter::stop_discovery,
            dispatch::adapter::get_discovered_nodes,
            dispatch::adapter::list_outputs,
            dispatch::adapter::bind_output,
            dispatch::adapter::unbind_output,
            // Auth
            dispatch::adapter::current_account,
            dispatch::adapter::send_login_code,
            dispatch::adapter::verify_login_code,
            dispatch::adapter::get_session_item,
            dispatch::adapter::set_session_item,
            dispatch::adapter::remove_session_item,
            dispatch::adapter::wipe_database,
            // Venues
            dispatch::adapter::get_venue,
            dispatch::adapter::list_venues,
            dispatch::adapter::create_venue,
            dispatch::adapter::update_venue,
            dispatch::adapter::delete_venue,
            dispatch::adapter::get_or_create_share_code,
            dispatch::adapter::join_venue,
            dispatch::adapter::leave_venue,
            // New sync engine
            dispatch::adapter::sync_full,
            dispatch::adapter::force_quit,
            dispatch::adapter::append_render_telemetry,
            // Remote queries
            dispatch::adapter::search_patterns_remote,
            dispatch::adapter::get_display_names,
            dispatch::adapter::verify_pattern,
            dispatch::adapter::fork_pattern,
            // StageLinQ / Perform
            dispatch::adapter::stagelinq_connect,
            dispatch::adapter::stagelinq_disconnect,
            dispatch::adapter::prodjlink_discover,
            dispatch::adapter::prodjlink_connect,
            dispatch::adapter::prodjlink_disconnect,
            dispatch::adapter::perform_match_track,
            dispatch::adapter::perform_match_track_by_metadata,
            dispatch::adapter::render_composite_deck,
            dispatch::adapter::render_composite_deck_unmatched,
            dispatch::adapter::render_set_deck_states,
            dispatch::adapter::render_clear_perform,
            dispatch::adapter::render_clear_active_layer,
            dispatch::adapter::render_identify,
            // Live controller device + state
            dispatch::adapter::controller_connect,
            dispatch::adapter::controller_disconnect,
            dispatch::adapter::controller_get_status,
            dispatch::adapter::controller_init_for_venue,
            dispatch::adapter::controller_start_learn,
            dispatch::adapter::controller_cancel_learn,
            dispatch::adapter::controller_set_active,
            dispatch::adapter::controller_get_state,
            // MIDI Mixer (fader/crossfader for Pioneer CDJ + DJM setups)
            dispatch::adapter::mixer_list_ports,
            dispatch::adapter::mixer_open_port,
            dispatch::adapter::mixer_connect,
            dispatch::adapter::mixer_disconnect,
            dispatch::adapter::mixer_get_status,
            dispatch::adapter::mixer_init_for_venue,
            dispatch::adapter::mixer_start_learn,
            dispatch::adapter::mixer_cancel_learn,
            // MIDI cue/binding/modifier CRUD
            dispatch::adapter::midi_list_cues,
            dispatch::adapter::midi_create_cue,
            dispatch::adapter::midi_update_cue,
            dispatch::adapter::midi_delete_cue,
            dispatch::adapter::midi_list_modifiers,
            dispatch::adapter::midi_create_modifier,
            dispatch::adapter::midi_delete_modifier,
            dispatch::adapter::midi_list_bindings,
            dispatch::adapter::midi_create_binding,
            dispatch::adapter::midi_update_binding,
            dispatch::adapter::midi_delete_binding,
            dispatch::adapter::midi_reload_mapping,
            dispatch::adapter::midi_fire_cue,
            dispatch::adapter::midi_release_cue,
            dispatch::adapter::midi_compile_cues_for_deck,
            // Engine DJ
            dispatch::adapter::engine_dj_open_library,
            dispatch::adapter::engine_dj_list_playlists,
            dispatch::adapter::engine_dj_list_tracks,
            dispatch::adapter::engine_dj_get_playlist_tracks,
            dispatch::adapter::engine_dj_search_tracks,
            dispatch::adapter::engine_dj_import_tracks,
            dispatch::adapter::engine_dj_default_library_path,
            // Rekordbox
            dispatch::adapter::rekordbox_open_library,
            dispatch::adapter::rekordbox_list_tracks,
            dispatch::adapter::rekordbox_list_playlists,
            dispatch::adapter::rekordbox_get_playlist_tracks,
            dispatch::adapter::rekordbox_search_tracks,
            dispatch::adapter::rekordbox_import_tracks,
            // Agent threads
            dispatch::adapter::agent_thread_create,
            dispatch::adapter::agent_thread_get,
            dispatch::adapter::agent_thread_list,
            dispatch::adapter::agent_thread_append_messages,
            dispatch::adapter::agent_thread_delete,
            dispatch::adapter::agent_thread_rename,
            dispatch::adapter::agent_thread_set_actor,
            dispatch::adapter::agent_thread_record_usage,
            dispatch::adapter::agent_turn_start,
            dispatch::adapter::agent_turn_cancel,
            dispatch::adapter::agent_steer,
            dispatch::adapter::skills_listing,
            dispatch::adapter::get_skill,
            // Relational authored document history
            dispatch::adapter::authored_state_prepare_turn,
            dispatch::adapter::authored_state_finalize_turn,
            dispatch::adapter::authored_state_recover_turns,
            dispatch::adapter::authored_state_set_session_actor,
            dispatch::adapter::authored_state_list_history,
            dispatch::adapter::authored_state_restore,
            dispatch::adapter::authored_state_create_workspace,
            dispatch::adapter::authored_state_check_workspace,
            dispatch::adapter::authored_state_commit_workspace,
            dispatch::adapter::authored_state_merge_workspace,
            dispatch::adapter::authored_state_remove_workspace,
            // Agent code execution
            dispatch::adapter::run_python_cell,
            dispatch::adapter::cancel_python_cell,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            if let tauri::RunEvent::WindowEvent {
                label,
                event: tauri::WindowEvent::CloseRequested { .. },
                ..
            } = event
            {
                if label == "main" {
                    app_handle.exit(0);
                }
            }
        });

    // Signal the sync loop to shut down gracefully (fires after run() returns).
    // The watch channel may already be dropped if app exited abruptly — that's OK.
}
