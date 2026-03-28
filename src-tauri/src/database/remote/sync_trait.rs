// Generic sync trait + payload structs for Supabase upserts
//
// Replaces per-table upsert functions with a single `Syncable` trait.
// Each model that can be synced implements `Syncable` via a payload struct
// that contains exactly the fields sent to the cloud (no local-only fields).

use super::common::{SupabaseClient, SyncError};
use serde::Serialize;

/// Trait for models that can be synced to Supabase via upsert.
pub trait Syncable: Serialize {
    /// The Supabase table name.
    fn table_name() -> &'static str;
    /// The ON CONFLICT column(s) for upsert (comma-separated if composite).
    fn conflict_key() -> &'static str {
        "id"
    }
}

/// Upsert a single record to Supabase.
pub async fn sync_record<T: Syncable>(
    client: &SupabaseClient,
    record: &T,
    access_token: &str,
) -> Result<(), SyncError> {
    client
        .upsert_no_return(T::table_name(), record, T::conflict_key(), access_token)
        .await
}

/// Upsert multiple records to Supabase in a single batch request.
pub async fn sync_records<T: Syncable>(
    client: &SupabaseClient,
    records: &[T],
    access_token: &str,
) -> Result<(), SyncError> {
    if records.is_empty() {
        return Ok(());
    }
    client
        .upsert_batch_no_return(T::table_name(), records, T::conflict_key(), access_token)
        .await
}

/// Delete a record from Supabase by its `id` column.
pub async fn delete_record(
    client: &SupabaseClient,
    table: &str,
    id: &str,
    access_token: &str,
) -> Result<(), SyncError> {
    client.delete(table, id, access_token).await
}

// ============================================================================
// Payload structs (one per table that needs upsert)
// ============================================================================

// -- venues --

#[derive(Serialize)]
pub struct VenuePayload<'a> {
    pub id: &'a str,
    pub uid: &'a str,
    pub name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub share_code: Option<&'a str>,
}

impl Syncable for VenuePayload<'_> {
    fn table_name() -> &'static str {
        "venues"
    }
}

// -- fixtures --

#[derive(Serialize)]
pub struct FixturePayload<'a> {
    pub id: &'a str,
    pub uid: &'a str,
    pub venue_id: &'a str,
    pub universe: i64,
    pub address: i64,
    pub num_channels: i64,
    pub manufacturer: &'a str,
    pub model: &'a str,
    pub mode_name: &'a str,
    pub fixture_path: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<&'a str>,
    pub pos_x: f64,
    pub pos_y: f64,
    pub pos_z: f64,
    pub rot_x: f64,
    pub rot_y: f64,
    pub rot_z: f64,
}

impl Syncable for FixturePayload<'_> {
    fn table_name() -> &'static str {
        "fixtures"
    }
}

// -- pattern_categories --

#[derive(Serialize)]
pub struct PatternCategoryPayload<'a> {
    pub id: &'a str,
    pub uid: &'a str,
    pub name: &'a str,
}

impl Syncable for PatternCategoryPayload<'_> {
    fn table_name() -> &'static str {
        "pattern_categories"
    }
}

// -- tracks --

#[derive(Serialize)]
pub struct TrackPayload<'a> {
    pub id: &'a str,
    pub uid: &'a str,
    pub track_hash: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artist: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub album: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track_number: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disc_number: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage_path: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub album_art_path: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub album_art_mime: Option<&'a str>,
}

impl Syncable for TrackPayload<'_> {
    fn table_name() -> &'static str {
        "tracks"
    }
}

// -- track_beats --

#[derive(Serialize)]
pub struct TrackBeatsPayload<'a> {
    pub uid: &'a str,
    pub track_id: &'a str,
    pub beats_json: &'a str,
    pub downbeats_json: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bpm: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub downbeat_offset: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub beats_per_bar: Option<i64>,
}

impl Syncable for TrackBeatsPayload<'_> {
    fn table_name() -> &'static str {
        "track_beats"
    }
    fn conflict_key() -> &'static str {
        "track_id"
    }
}

// -- track_roots --

#[derive(Serialize)]
pub struct TrackRootsPayload<'a> {
    pub uid: &'a str,
    pub track_id: &'a str,
    pub sections_json: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logits_storage_path: Option<&'a str>,
}

impl Syncable for TrackRootsPayload<'_> {
    fn table_name() -> &'static str {
        "track_roots"
    }
    fn conflict_key() -> &'static str {
        "track_id"
    }
}

// -- track_stems --

#[derive(Serialize)]
pub struct TrackStemPayload<'a> {
    pub uid: &'a str,
    pub track_id: &'a str,
    pub stem_name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage_path: Option<&'a str>,
}

impl Syncable for TrackStemPayload<'_> {
    fn table_name() -> &'static str {
        "track_stems"
    }
    fn conflict_key() -> &'static str {
        "track_id,stem_name"
    }
}

// -- track_waveforms --

#[derive(Serialize)]
pub struct TrackWaveformPayload<'a> {
    pub uid: &'a str,
    pub track_id: &'a str,
    pub preview_samples: &'a [f32],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview_colors: Option<&'a [u8]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview_bands_low: Option<&'a [f32]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview_bands_mid: Option<&'a [f32]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview_bands_high: Option<&'a [f32]>,
    pub sample_rate: i32,
    pub duration_seconds: f64,
}

impl Syncable for TrackWaveformPayload<'_> {
    fn table_name() -> &'static str {
        "track_waveforms"
    }
    fn conflict_key() -> &'static str {
        "track_id"
    }
}

// -- patterns --

#[derive(Serialize)]
pub struct PatternPayload<'a> {
    pub id: &'a str,
    pub uid: &'a str,
    pub name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category_name: Option<&'a str>,
    pub is_verified: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author_name: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forked_from_id: Option<&'a str>,
}

impl Syncable for PatternPayload<'_> {
    fn table_name() -> &'static str {
        "patterns"
    }
}

// -- implementations --

#[derive(Serialize)]
pub struct ImplementationPayload<'a> {
    pub id: &'a str,
    pub uid: &'a str,
    pub pattern_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<&'a str>,
    pub graph_json: &'a str,
}

impl Syncable for ImplementationPayload<'_> {
    fn table_name() -> &'static str {
        "implementations"
    }
}

// -- venue_implementation_overrides --

#[derive(Serialize)]
pub struct VenueOverridePayload<'a> {
    pub uid: &'a str,
    pub venue_id: &'a str,
    pub pattern_id: &'a str,
    pub implementation_id: &'a str,
}

impl Syncable for VenueOverridePayload<'_> {
    fn table_name() -> &'static str {
        "venue_implementation_overrides"
    }
    fn conflict_key() -> &'static str {
        "venue_id,pattern_id"
    }
}

// -- scores --

#[derive(Serialize)]
pub struct ScorePayload<'a> {
    pub id: &'a str,
    pub uid: &'a str,
    pub track_id: &'a str,
    pub venue_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<&'a str>,
}

impl Syncable for ScorePayload<'_> {
    fn table_name() -> &'static str {
        "scores"
    }
}

// -- track_scores --

#[derive(Serialize)]
pub struct TrackScorePayload {
    pub id: String,
    pub uid: String,
    pub score_id: String,
    pub pattern_id: String,
    pub start_time: f64,
    pub end_time: f64,
    pub z_index: i64,
    pub blend_mode: String,
    pub args_json: String,
}

impl Syncable for TrackScorePayload {
    fn table_name() -> &'static str {
        "track_scores"
    }
}

// -- fixture_groups --

#[derive(Serialize)]
pub struct GroupPayload<'a> {
    pub id: &'a str,
    pub uid: &'a str,
    pub venue_id: &'a str,
    pub name: Option<&'a str>,
    pub axis_lr: Option<f64>,
    pub axis_fb: Option<f64>,
    pub axis_ab: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub movement_config: Option<&'a str>,
    pub display_order: i64,
}

impl Syncable for GroupPayload<'_> {
    fn table_name() -> &'static str {
        "fixture_groups"
    }
}

// -- fixture_group_members --

#[derive(Serialize)]
pub struct GroupMemberPayload<'a> {
    pub fixture_id: &'a str,
    pub group_id: &'a str,
    pub display_order: i64,
}

impl Syncable for GroupMemberPayload<'_> {
    fn table_name() -> &'static str {
        "fixture_group_members"
    }
    fn conflict_key() -> &'static str {
        "fixture_id,group_id"
    }
}
