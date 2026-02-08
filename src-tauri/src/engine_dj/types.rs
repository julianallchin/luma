use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/engine_dj.ts")]
#[ts(rename_all = "camelCase")]
pub struct EngineDjTrack {
    #[ts(type = "number")]
    pub id: i64,
    pub path: String,
    pub filename: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub bpm_analyzed: Option<f64>,
    pub length: Option<f64>,
    pub origin_database_uuid: Option<String>,
    #[ts(type = "number | null")]
    pub origin_track_id: Option<i64>,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/engine_dj.ts")]
#[ts(rename_all = "camelCase")]
pub struct EngineDjPlaylist {
    #[ts(type = "number")]
    pub id: i64,
    pub title: String,
    #[ts(type = "number | null")]
    pub parent_id: Option<i64>,
    #[ts(type = "number")]
    pub track_count: i64,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/engine_dj.ts")]
#[ts(rename_all = "camelCase")]
pub struct EngineDjLibraryInfo {
    pub database_uuid: String,
    pub library_path: String,
    #[ts(type = "number")]
    pub track_count: i64,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ImportProgressEvent {
    pub done: usize,
    pub total: usize,
    pub current_track: Option<String>,
    pub phase: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/engine_dj.ts")]
#[ts(rename_all = "camelCase")]
pub struct EngineDjSyncResult {
    #[ts(type = "number")]
    pub updated: i64,
    #[ts(type = "number")]
    pub missing: i64,
    #[ts(type = "number")]
    pub new_count: i64,
}
