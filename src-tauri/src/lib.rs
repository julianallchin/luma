mod annotations;
mod audio;
mod beat_worker;
mod database;
mod patterns;
mod playback;
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
        .plugin(tauri_plugin_opener::init())
        .plugin(dialog_init())
        .setup(|app| {
            let app_handle = app.handle();
            let db = tauri::async_runtime::block_on(async {
                let db = database::init_db(&app_handle).await?;
                Ok::<_, String>(db)
            })?;
            app.manage(db);
            app.manage(database::ProjectDb(tokio::sync::Mutex::new(None)));
            let playback_state = playback::PatternPlaybackState::default();
            playback_state.spawn_broadcaster(app_handle.clone());
            app.manage(playback_state);
            tracks::ensure_storage(&app_handle)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            schema::get_node_types,
            schema::run_graph,
            patterns::list_patterns,
            patterns::create_pattern,
            patterns::get_pattern_graph,
            patterns::save_pattern_graph,
            tracks::list_tracks,
            tracks::import_track,
            tracks::get_melspec,
            tracks::wipe_tracks,
            tracks::get_track_beats,
            tracks::load_track_playback,
            playback::playback_play_node,
            playback::playback_pause,
            playback::playback_seek,
            playback::playback_set_loop,
            playback::playback_snapshot,
            project_manager::create_project,
            project_manager::open_project,
            project_manager::close_project,
            project_manager::get_recent_projects,
            annotations::list_annotations,
            annotations::create_annotation,
            annotations::update_annotation,
            annotations::delete_annotation,
            waveforms::get_track_waveform
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
