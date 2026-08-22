use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use ts_rs::TS;

#[derive(TS, Serialize, Deserialize, Clone, Debug, FromRow)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct TrackSummary {
    pub id: String,
    pub uid: Option<String>,
    pub track_hash: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    #[ts(type = "number | null")]
    pub track_number: Option<i64>,
    #[ts(type = "number | null")]
    pub disc_number: Option<i64>,
    #[ts(type = "number | null")]
    pub duration_seconds: Option<f64>,
    pub file_path: String,
    pub storage_path: Option<String>,
    pub album_art_path: Option<String>,
    pub album_art_mime: Option<String>,
    pub album_art_storage_path: Option<String>,
    pub source_type: Option<String>,
    pub source_id: Option<String>,
    pub source_filename: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Durable phase-one result from any track import source.
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct TrackImportResult {
    pub import_id: String,
    pub tracks: Vec<TrackSummary>,
    pub failures: Vec<TrackImportFailure>,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct TrackImportFailure {
    pub source_id: String,
    pub message: String,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "snake_case")]
pub enum TrackImportPhase {
    Importing,
    Analyzing,
    Complete,
}

/// Host-neutral progress payload. Consumers branch on fields and enum values,
/// never on human-readable status text.
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct TrackImportProgress {
    pub import_id: String,
    pub source: String,
    pub phase: TrackImportPhase,
    /// Requested source items whose work for this import is complete. Phase-one
    /// failures and deduplicated existing rows already count when analysis
    /// begins; newly inserted rows count after their analysis attempt finishes.
    /// A panicked/cancelled analysis task therefore leaves terminal `done`
    /// below `total` rather than claiming work that never completed.
    pub done: usize,
    /// Original number of requested source items. Analysis must never replace
    /// this with the smaller number of rows that were eligible to analyze.
    pub total: usize,
    pub track_id: Option<String>,
    pub current_track: Option<String>,
    pub step: Option<String>,
    pub error: Option<String>,
}

/// Beat analysis data for a track
#[derive(TS, Serialize, Deserialize, Clone, Debug, FromRow)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct TrackBeats {
    #[sqlx(rename = "track_id")]
    pub track_id: String,
    pub uid: Option<String>,
    #[sqlx(rename = "beats_json")]
    pub beats_json: String,
    #[sqlx(rename = "downbeats_json")]
    pub downbeats_json: String,
    pub bpm: Option<f64>,
    #[sqlx(rename = "downbeat_offset")]
    pub downbeat_offset: Option<f64>,
    #[ts(type = "number | null")]
    #[sqlx(rename = "beats_per_bar")]
    pub beats_per_bar: Option<i64>,
    #[sqlx(rename = "created_at")]
    pub created_at: String,
    #[sqlx(rename = "updated_at")]
    pub updated_at: String,
}

/// One parsed entry of [`TrackRoots::sections_json`]: a time span with the
/// detected harmonic root.
///
/// The stored JSON is `{"start":2.716,"end":4.318,"root":9,"label":"A:(1)"}`.
/// `root` is a pitch class 0-11 or `null` (no-chord / low confidence); `label`
/// is the full chord symbol (`"G:maj"`, `"N"` for none) and may be absent in
/// older rows. Not TS-exported — nothing on the frontend consumes it yet.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ChordSection {
    pub start_s: f32,
    pub end_s: f32,
    pub root_pitch_class: Option<u8>,
    pub label: Option<String>,
}

/// Root/section analysis data for a track
#[derive(TS, Serialize, Deserialize, Clone, Debug, FromRow)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct TrackRoots {
    #[sqlx(rename = "track_id")]
    pub track_id: String,
    pub uid: Option<String>,
    #[sqlx(rename = "sections_json")]
    pub sections_json: String,
    /// Local file path to logits data
    #[sqlx(rename = "logits_path")]
    pub logits_path: Option<String>,
    /// Cloud storage path for compressed logits
    #[sqlx(rename = "logits_storage_path")]
    pub logits_storage_path: Option<String>,
    #[sqlx(rename = "created_at")]
    pub created_at: String,
    #[sqlx(rename = "updated_at")]
    pub updated_at: String,
}

/// Stem audio file for a track
#[derive(TS, Serialize, Deserialize, Clone, Debug, FromRow)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct TrackStem {
    #[sqlx(rename = "track_id")]
    pub track_id: String,
    pub uid: Option<String>,
    #[sqlx(rename = "stem_name")]
    pub stem_name: String,
    /// Local file path to stem audio
    #[sqlx(rename = "file_path")]
    pub file_path: String,
    /// Cloud storage path for compressed stem
    #[sqlx(rename = "storage_path")]
    pub storage_path: Option<String>,
    #[sqlx(rename = "created_at")]
    pub created_at: String,
    #[sqlx(rename = "updated_at")]
    pub updated_at: String,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug, FromRow)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct TrackBrowserRow {
    pub id: String,
    pub uid: Option<String>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    #[ts(type = "number | null")]
    pub duration_seconds: Option<f64>,
    pub album_art_path: Option<String>,
    pub album_art_mime: Option<String>,
    pub source_type: Option<String>,
    pub file_path: String,
    pub created_at: String,
    pub bpm: Option<f64>,
    #[ts(type = "number")]
    pub annotation_count: i64,
    /// Annotation count for the currently active venue (0 if no venue)
    #[ts(type = "number")]
    pub venue_annotation_count: i64,
    /// Number of scores for this track in the active venue. This is the
    /// durable venue-membership signal: an empty score still counts.
    #[ts(type = "number")]
    pub venue_score_count: i64,
    /// Convenience form of `venue_score_count > 0` for hosts that should not
    /// need to reproduce the membership rule.
    pub is_in_venue: bool,
    /// Total seconds of the track covered by venue annotations (intervals
    /// merged so overlaps don't double-count). 0 if no venue. Filled in Rust
    /// by `list_tracks_enriched`, not selected from SQL.
    #[sqlx(skip)]
    #[ts(type = "number")]
    pub venue_annotation_coverage_seconds: f64,
    pub has_storage: bool,
    pub has_beats: bool,
    pub has_stems: bool,
    pub has_roots: bool,
    pub has_drum_onsets: bool,
    pub has_bar_classifications: bool,
    pub has_genres: bool,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
pub struct MelSpec {
    pub width: usize,
    pub height: usize,
    pub data: Vec<f32>,
    pub beat_grid: Option<crate::models::node_graph::BeatGrid>,
}
