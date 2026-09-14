use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct EngineDjTrack {
    pub id: i64,
    pub path: String,
    pub filename: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub bpm_analyzed: Option<f64>,
    pub length: Option<f64>,
    pub origin_database_uuid: Option<String>,
    pub origin_track_id: Option<i64>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct EngineDjPlaylist {
    pub id: i64,
    pub title: String,
    pub parent_id: Option<i64>,
    pub track_count: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct EngineDjLibraryInfo {
    pub database_uuid: String,
    pub library_path: String,
    pub track_count: i64,
}
