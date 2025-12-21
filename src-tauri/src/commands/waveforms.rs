//! Tauri commands for waveform operations

use tauri::State;

use crate::database::Db;
use crate::models::waveforms::TrackWaveform;
use crate::services::waveforms as waveform_service;

#[tauri::command]
pub async fn get_track_waveform(db: State<'_, Db>, track_id: i64) -> Result<TrackWaveform, String> {
    waveform_service::get_track_waveform(&db.0, track_id).await
}
