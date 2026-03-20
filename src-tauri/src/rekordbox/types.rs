use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// A track from the Rekordbox master.db, deserialized from the subprocess bridge.
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/rekordbox.ts")]
#[ts(rename_all = "camelCase")]
pub struct RekordboxTrack {
    /// Rekordbox content ID (string)
    pub id: String,
    /// Stable UUID — used as source_id for Luma linking
    pub uuid: String,
    /// Absolute path to the audio file
    pub file_path: Option<String>,
    /// Bare filename
    pub filename: Option<String>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    /// BPM as float (already converted from int*100)
    pub bpm: Option<f64>,
    /// Duration in seconds (integer in Rekordbox, converted to float)
    pub duration_seconds: Option<f64>,
    #[ts(type = "number | null")]
    pub file_size: Option<i32>,
    #[ts(type = "number | null")]
    pub sample_rate: Option<i32>,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/rekordbox.ts")]
#[ts(rename_all = "camelCase")]
pub struct RekordboxPlaylist {
    pub id: String,
    pub name: String,
    pub parent_id: Option<String>,
    #[ts(type = "number")]
    pub track_count: usize,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/rekordbox.ts")]
#[ts(rename_all = "camelCase")]
pub struct RekordboxLibraryInfo {
    #[ts(type = "number")]
    pub track_count: usize,
}
