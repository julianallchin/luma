mod artnet;
mod audio;
mod beat_worker;
mod commands;
mod compositor;
mod database;
mod engine;
mod engine_dj;
mod fixtures;
mod host_audio;
mod models;
mod node_graph;
mod python_env;
mod render_engine;
mod root_worker;
mod services;
mod settings;
mod stagelinq_manager;
mod stem_worker;

use tauri::Manager;
use tauri_plugin_dialog::init as dialog_init;

use crate::services::fixtures::FixtureState;
use crate::services::tracks;
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init()) // open files & URLs in browser
        .plugin(dialog_init()) // native OS file dialogs for uploading
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
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // registers routes for frontend
            commands::node_graph::get_node_types,
            commands::node_graph::run_graph,
            commands::patterns::get_pattern,
            commands::patterns::list_patterns,
            commands::patterns::create_pattern,
            commands::patterns::update_pattern,
            commands::patterns::set_pattern_category,
            commands::patterns::get_pattern_graph,
            commands::patterns::get_pattern_args,
            commands::patterns::save_pattern_graph,
            commands::categories::list_pattern_categories,
            commands::categories::create_pattern_category,
            commands::tags::create_tag,
            commands::tags::list_tags_for_venue,
            commands::tags::get_tag,
            commands::tags::update_tag,
            commands::tags::delete_tag,
            commands::tags::assign_tag_to_fixture,
            commands::tags::remove_tag_from_fixture,
            commands::tags::get_tags_for_fixture,
            commands::tags::get_fixtures_with_tag,
            commands::tags::batch_assign_tag,
            commands::tags::regenerate_spatial_tags,
            commands::tags::initialize_venue_tags,
            commands::tracks::list_tracks,
            commands::tracks::import_track,
            commands::tracks::get_melspec,
            commands::tracks::delete_track,
            commands::tracks::wipe_tracks,
            commands::tracks::get_track_beats,
            // Host audio commands
            host_audio::host_load_segment,
            host_audio::host_load_track,
            host_audio::host_play,
            host_audio::host_pause,
            host_audio::host_seek,
            host_audio::host_set_loop,
            host_audio::host_set_playback_rate,
            host_audio::host_snapshot,
            commands::scores::list_track_scores,
            commands::scores::create_track_score,
            commands::scores::update_track_score,
            commands::scores::delete_track_score,
            commands::waveforms::get_track_waveform,
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
            commands::groups::ensure_fixtures_grouped,
            commands::groups::get_predefined_tags,
            commands::groups::add_tag_to_group,
            commands::groups::remove_tag_from_group,
            commands::groups::set_group_tags,
            compositor::composite_track,
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
            // Venues
            commands::venues::get_venue,
            commands::venues::list_venues,
            commands::venues::create_venue,
            commands::venues::update_venue,
            commands::venues::delete_venue,
            // Cloud Sync
            commands::cloud_sync::sync_all,
            commands::cloud_sync::sync_venue,
            commands::cloud_sync::sync_venue_with_fixtures,
            commands::cloud_sync::sync_track,
            commands::cloud_sync::sync_track_with_data,
            commands::cloud_sync::sync_pattern,
            commands::cloud_sync::sync_pattern_with_implementations,
            commands::cloud_sync::sync_score,
            // StageLinQ / Perform
            commands::perform::stagelinq_connect,
            commands::perform::stagelinq_disconnect,
            commands::perform::perform_match_track,
            commands::perform::render_composite_deck,
            render_engine::render_set_deck_states,
            render_engine::render_clear_perform,
            // Engine DJ
            commands::engine_dj::engine_dj_open_library,
            commands::engine_dj::engine_dj_list_playlists,
            commands::engine_dj::engine_dj_list_tracks,
            commands::engine_dj::engine_dj_get_playlist_tracks,
            commands::engine_dj::engine_dj_search_tracks,
            commands::engine_dj::engine_dj_import_tracks,
            commands::engine_dj::engine_dj_sync_library,
            commands::engine_dj::engine_dj_default_library_path,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
