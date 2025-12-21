mod artnet;
mod audio;
mod auth;
mod beat_worker;
mod commands;
mod compositor;
mod database;
mod engine;
mod fixtures;
mod host_audio;
mod models;
mod python_env;
mod root_worker;
mod schema;
mod services;
mod settings;
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
                    .accelerator("CmdOrCtrl+, ")
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

            // Host audio state - unified playback for all contexts
            let host_audio = host_audio::HostAudioState::default();
            host_audio.spawn_broadcaster(app_handle.clone());
            app.manage(host_audio);
            let _ = tauri::async_runtime::block_on(host_audio::reload_settings(&app_handle));

            // Stem Cache for graph execution
            app.manage(audio::StemCache::new());

            // Shared FFT Service for audio analysis
            app.manage(audio::FftService::new());

            tracks::ensure_storage(&app_handle)?;
            app.manage(FixtureState(std::sync::Mutex::new(None)));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // registers routes for frontend
            schema::get_node_types,
            schema::run_graph,
            commands::patterns::get_pattern,
            commands::patterns::list_patterns,
            commands::patterns::create_pattern,
            commands::patterns::set_pattern_category,
            commands::patterns::get_pattern_graph,
            commands::patterns::get_pattern_args,
            commands::patterns::save_pattern_graph,
            commands::categories::list_pattern_categories,
            commands::categories::create_pattern_category,
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
            host_audio::host_snapshot,
            commands::scores::list_scores,
            commands::scores::create_score,
            commands::scores::update_score,
            commands::scores::delete_score,
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
            compositor::composite_track,
            // Settings
            settings::get_settings,
            settings::set_setting,
            // ArtNet
            artnet::start_discovery,
            artnet::stop_discovery,
            artnet::get_discovered_nodes,
            // Auth
            auth::get_session_item,
            auth::set_session_item,
            auth::remove_session_item,
            auth::log_session_from_state_db,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
