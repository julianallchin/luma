mod annotations;
mod audio;
mod beat_worker;
mod compositor;
mod database;
mod engine;
mod fixtures;
mod host_audio;
mod models;
mod patterns;
mod project_manager;
mod python_env;
mod root_worker;
mod schema;
mod stem_worker;
pub mod tracks;
mod waveforms;

use tauri::Manager;
use tauri_plugin_dialog::init as dialog_init;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init()) // open files & URLs in browser
        .plugin(dialog_init()) // native OS file dialogs for uploading
        .setup(|app| {
            let app_handle = app.handle();

            // initializing luma.db
            let db = tauri::async_runtime::block_on(async {
                let db = database::init_app_db(&app_handle).await?;
                Ok::<_, String>(db)
            })?;

            // store shared state in the Manager
            app.manage(db);
            app.manage(database::ProjectDb(tokio::sync::Mutex::new(None)));

            // Host audio state - unified playback for all contexts
            let host_audio = host_audio::HostAudioState::default();
            host_audio.spawn_broadcaster(app_handle.clone());
            app.manage(host_audio);

            tracks::ensure_storage(&app_handle)?;
            app.manage(fixtures::FixtureState(std::sync::Mutex::new(None)));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // registers routes for frontend
            schema::get_node_types,
            schema::run_graph,
            patterns::get_pattern,
            patterns::list_patterns,
            patterns::create_pattern,
            patterns::get_pattern_graph,
            patterns::save_pattern_graph,
            tracks::list_tracks,
            tracks::import_track,
            tracks::get_melspec,
            tracks::wipe_tracks,
            tracks::get_track_beats,
            // Host audio commands
            host_audio::host_load_segment,
            host_audio::host_load_track,
            host_audio::host_play,
            host_audio::host_pause,
            host_audio::host_seek,
            host_audio::host_set_loop,
            host_audio::host_snapshot,
            project_manager::create_project,
            project_manager::open_project,
            project_manager::close_project,
            project_manager::get_recent_projects,
            annotations::list_annotations,
            annotations::create_annotation,
            annotations::update_annotation,
            annotations::delete_annotation,
            waveforms::get_track_waveform,
            fixtures::initialize_fixtures,
            fixtures::search_fixtures,
            fixtures::get_fixture_definition,
            fixtures::patch_fixture,
            fixtures::get_patched_fixtures,
            fixtures::get_patch_hierarchy,
            fixtures::move_patched_fixture,
            fixtures::move_patched_fixture_spatial,
            fixtures::remove_patched_fixture,
            fixtures::rename_patched_fixture,
            compositor::composite_track
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
