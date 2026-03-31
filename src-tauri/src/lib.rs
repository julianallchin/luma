mod annotation_preview;
mod artnet;
pub mod audio;
mod beat_worker;
mod cmd_util;
mod commands;
mod compositor;
pub mod config;
mod database;
mod engine;
mod engine_dj;
mod ffmpeg_env;
mod fixtures;
mod host_audio;
pub mod models;
mod node_graph;
mod python_env;
mod rekordbox;
mod render_engine;
mod root_worker;
pub mod services;
mod settings;
mod stagelinq_manager;
mod stem_worker;
mod sync;

use tauri::Manager;
use tauri_plugin_dialog::init as dialog_init;

use crate::services::fixtures::FixtureState;
use crate::services::tracks;
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
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
                        if let Some(window) = app.get_webview_window("settings") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                });
            }

            // Resolve bundled ffmpeg path for audio decoding + Python workers
            ffmpeg_env::init(app_handle);

            // initializing luma.db
            let db = tauri::async_runtime::block_on(async {
                let db = database::init_app_db(&app_handle).await?;
                Ok::<_, String>(db)
            })?;
            let state_db = tauri::async_runtime::block_on(async {
                let db = database::init_state_db(&app_handle).await?;
                Ok::<_, String>(db)
            })?;

            // store shared state in the Manager
            app.manage(db);
            app.manage(state_db);

            // Sync engine — create after both DB pools are available
            {
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
                );
                let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
                tauri::async_runtime::spawn(sync::push::run_sync_loop(
                    engine.pool().clone(),
                    engine.state_pool().clone(),
                    engine.remote().clone(),
                    engine.push_notify.clone(),
                    engine.sync_lock.clone(),
                    app_handle.clone(),
                    shutdown_rx,
                ));
                app.manage(engine);
                app.manage(shutdown_tx);
            }

            // ArtNet Manager
            let artnet_manager = artnet::ArtNetManager::new(app_handle.clone());
            app.manage(artnet_manager);

            // Host audio state - audio playback only
            let host_audio = host_audio::HostAudioState::default();
            host_audio.spawn_broadcaster(app_handle.clone());
            app.manage(host_audio);
            let _ = tauri::async_runtime::block_on(host_audio::reload_settings(&app_handle));

            // Render engine - rendering, universe state, ArtNet output
            let render_engine = render_engine::RenderEngine::default();
            render_engine.spawn_render_loop(app_handle.clone());
            app.manage(render_engine);

            // Stem Cache for graph execution
            app.manage(audio::StemCache::new());

            // Shared FFT Service for audio analysis
            app.manage(audio::FftService::new());

            tracks::ensure_storage(&app_handle)?;

            app.manage(FixtureState(std::sync::Mutex::new(None)));

            // StageLinQ Manager
            app.manage(stagelinq_manager::StageLinqManager::new());

            // Start Python environment setup in the background
            python_env::setup_python_env_background(app_handle.clone());

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // registers routes for frontend
            commands::node_graph::get_node_types,
            commands::node_graph::run_graph,
            commands::node_graph::preview_pattern,
            commands::patterns::get_pattern,
            commands::patterns::list_patterns,
            commands::patterns::create_pattern,
            commands::patterns::update_pattern,
            commands::patterns::set_pattern_category,
            commands::patterns::get_pattern_graph,
            commands::patterns::get_pattern_args,
            commands::patterns::save_pattern_graph,
            commands::patterns::delete_pattern,
            commands::categories::list_pattern_categories,
            commands::categories::create_pattern_category,
            commands::tracks::list_tracks,
            commands::tracks::list_tracks_enriched,
            commands::tracks::get_venue_annotation_counts,
            commands::tracks::import_track,
            commands::tracks::import_tracks,
            commands::tracks::get_melspec,
            commands::tracks::delete_track,
            commands::tracks::reprocess_track,
            commands::tracks::wipe_tracks,
            commands::tracks::get_track_beats,
            commands::tracks::get_track_audio_base64,
            // Host audio commands
            host_audio::host_load_segment,
            host_audio::host_load_track,
            host_audio::host_play,
            host_audio::host_pause,
            host_audio::host_seek,
            host_audio::host_set_loop,
            host_audio::host_set_playback_rate,
            host_audio::host_unload,
            host_audio::host_snapshot,
            commands::scores::list_scores_for_track,
            commands::scores::create_score,
            commands::scores::list_track_scores,
            commands::scores::create_track_score,
            commands::scores::update_track_score,
            commands::scores::delete_score,
            commands::scores::delete_track_score,
            commands::scores::replace_track_scores,
            commands::waveforms::get_track_waveform,
            commands::waveforms::reprocess_waveform,
            commands::fixtures::initialize_fixtures,
            commands::fixtures::search_fixtures,
            commands::fixtures::get_fixture_definition,
            commands::fixtures::patch_fixture,
            commands::fixtures::get_patched_fixtures,
            commands::fixtures::get_patch_hierarchy,
            commands::fixtures::move_patched_fixture,
            commands::fixtures::move_patched_fixture_spatial,
            commands::fixtures::remove_patched_fixture,
            commands::fixtures::rename_patched_fixture,
            // Groups
            commands::groups::create_group,
            commands::groups::get_group,
            commands::groups::list_groups,
            commands::groups::update_group,
            commands::groups::delete_group,
            commands::groups::add_fixture_to_group,
            commands::groups::remove_fixture_from_group,
            commands::groups::get_fixtures_in_group,
            commands::groups::get_groups_for_fixture,
            commands::groups::get_grouped_hierarchy,
            commands::groups::preview_selection_query,
            commands::groups::get_ungrouped_fixtures,
            commands::groups::update_movement_config,
            compositor::composite_track,
            compositor::leave_track,
            compositor::verify_dsl_roundtrip,
            // Annotation Previews
            annotation_preview::generate_annotation_previews,
            annotation_preview::invalidate_annotation_previews,
            // Settings
            settings::get_settings,
            settings::set_setting,
            // ArtNet
            artnet::start_discovery,
            artnet::stop_discovery,
            artnet::get_discovered_nodes,
            // Auth
            commands::auth::get_session_item,
            commands::auth::set_session_item,
            commands::auth::remove_session_item,
            commands::auth::log_session_from_state_db,
            commands::auth::wipe_database,
            // Venues
            commands::venues::get_venue,
            commands::venues::list_venues,
            commands::venues::create_venue,
            commands::venues::update_venue,
            commands::venues::delete_venue,
            commands::venues::get_or_create_share_code,
            commands::venues::join_venue,
            commands::venues::leave_venue,
            // New sync engine
            commands::sync::sync_full,
            commands::sync::sync_pull,
            commands::sync::sync_files_v2,
            commands::sync::get_sync_status,
            commands::sync::get_pending_errors,
            commands::sync::retry_pending_op,
            // Remote queries
            commands::cloud_sync::search_patterns_remote,
            commands::cloud_sync::get_display_names,
            commands::patterns::verify_pattern,
            commands::patterns::fork_pattern,
            // StageLinQ / Perform
            commands::perform::stagelinq_connect,
            commands::perform::stagelinq_disconnect,
            commands::perform::perform_match_track,
            commands::perform::render_composite_deck,
            render_engine::render_set_deck_states,
            render_engine::render_clear_perform,
            render_engine::render_clear_active_layer,
            render_engine::render_identify_fixture,
            // Engine DJ
            commands::engine_dj::engine_dj_open_library,
            commands::engine_dj::engine_dj_list_playlists,
            commands::engine_dj::engine_dj_list_tracks,
            commands::engine_dj::engine_dj_get_playlist_tracks,
            commands::engine_dj::engine_dj_search_tracks,
            commands::engine_dj::engine_dj_import_tracks,
            commands::engine_dj::engine_dj_sync_library,
            commands::engine_dj::engine_dj_default_library_path,
            // Rekordbox
            commands::rekordbox::rekordbox_open_library,
            commands::rekordbox::rekordbox_list_tracks,
            commands::rekordbox::rekordbox_list_playlists,
            commands::rekordbox::rekordbox_get_playlist_tracks,
            commands::rekordbox::rekordbox_search_tracks,
            commands::rekordbox::rekordbox_import_tracks,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { .. } = &event {
                if let Some(window) = app_handle.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                } else {
                    if let Ok(builder) = tauri::WebviewWindowBuilder::from_config(
                        app_handle,
                        &app_handle.config().app.windows[0],
                    ) {
                        let _ = builder.build();
                    }
                }
            }
        });

    // Signal the sync loop to shut down gracefully (fires after run() returns).
    // The watch channel may already be dropped if app exited abruptly — that's OK.
}
