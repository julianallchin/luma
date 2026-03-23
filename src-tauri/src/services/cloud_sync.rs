//! Cloud sync orchestration service
//!
//! Coordinates syncing between local SQLite and Supabase.
//! All entity IDs are String UUIDs — local and cloud share the same ID space.
//! No remote_id mapping is needed; the model's `id` IS the cloud ID.
//!
//! 1. Reads records from local database (via database/local/)
//! 2. Calls remote database functions to upsert/delete (via database/remote/)

use sqlx::SqlitePool;

use crate::database::local::{
    categories as local_categories, fixtures as local_fixtures, groups as local_groups,
    implementations as local_implementations, patterns as local_patterns, scores as local_scores,
    tracks as local_tracks, venue_overrides as local_overrides, venues as local_venues,
    waveforms as local_waveforms,
};
use crate::database::remote::common::{SupabaseClient, SyncError};
use crate::database::remote::sync_trait::{
    delete_record, sync_record, sync_records, FixturePayload, GroupMemberPayload, GroupPayload,
    ImplementationPayload, PatternCategoryPayload, PatternPayload, ScorePayload, TrackBeatsPayload,
    TrackPayload, TrackRootsPayload, TrackScorePayload, TrackStemPayload, TrackWaveformPayload,
    VenueOverridePayload, VenuePayload,
};
use crate::services::waveforms as waveform_service;

// ============================================================================
// Error Types
// ============================================================================

#[derive(Debug)]
pub enum CloudSyncError {
    /// Error from local database operation
    LocalDb(String),
    /// Error from remote Supabase operation
    Remote(SyncError),
    /// Record is missing its uid (not owned / not syncable)
    NotSynced { table: String, local_id: String },
    /// Record not found in local database
    NotFound { table: String, local_id: String },
}

impl From<SyncError> for CloudSyncError {
    fn from(e: SyncError) -> Self {
        CloudSyncError::Remote(e)
    }
}

impl std::fmt::Display for CloudSyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CloudSyncError::LocalDb(msg) => write!(f, "Local DB error: {}", msg),
            CloudSyncError::Remote(e) => write!(f, "Remote sync error: {:?}", e),
            CloudSyncError::NotSynced { table, local_id } => {
                write!(f, "{} with id {} is missing uid", table, local_id)
            }
            CloudSyncError::NotFound { table, local_id } => {
                write!(f, "{} with id {} not found", table, local_id)
            }
        }
    }
}

// ============================================================================
// Sync Statistics
// ============================================================================

#[derive(Debug, Default)]
pub struct SyncStats {
    pub venues: usize,
    pub categories: usize,
    pub tracks: usize,
    pub fixtures: usize,
    pub patterns: usize,
    pub scores: usize,
    pub track_scores: usize,
    pub track_beats: usize,
    pub track_roots: usize,
    pub track_waveforms: usize,
    pub track_stems: usize,
    pub implementations: usize,
    pub venue_overrides: usize,
    pub groups: usize,
    pub dirty_total: usize,
    pub errors: Vec<String>,
}

// ============================================================================
// Helper: extract uid or return MissingField error
// ============================================================================

fn require_uid(uid: &Option<String>) -> Result<&str, SyncError> {
    uid.as_deref()
        .ok_or_else(|| SyncError::MissingField("uid".to_string()))
}

// ============================================================================
// Sync Context
// ============================================================================

/// Cloud sync orchestrator that coordinates local and remote database operations
pub struct CloudSync<'a> {
    pub pool: &'a SqlitePool,
    pub client: &'a SupabaseClient,
    pub access_token: &'a str,
    pub current_uid: &'a str,
}

impl<'a> CloudSync<'a> {
    pub fn new(
        pool: &'a SqlitePool,
        client: &'a SupabaseClient,
        access_token: &'a str,
        current_uid: &'a str,
    ) -> Self {
        Self {
            pool,
            client,
            access_token,
            current_uid,
        }
    }

    // ========================================================================
    // Tier 1: No Dependencies
    // ========================================================================

    /// Sync a venue to the cloud.
    pub async fn sync_venue(&self, local_id: &str) -> Result<(), CloudSyncError> {
        let venue = local_venues::get_venue(self.pool, local_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;

        let uid = require_uid(&venue.uid)?;
        let payload = VenuePayload {
            id: &venue.id,
            uid,
            name: &venue.name,
            description: venue.description.as_deref(),
            share_code: venue.share_code.as_deref(),
        };

        sync_record(self.client, &payload, self.access_token).await?;
        Ok(())
    }

    /// Sync a pattern category to the cloud.
    pub async fn sync_category(&self, local_id: &str) -> Result<(), CloudSyncError> {
        let category = local_categories::get_category(self.pool, local_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;

        let uid = require_uid(&category.uid)?;
        let payload = PatternCategoryPayload {
            id: &category.id,
            uid,
            name: &category.name,
        };

        sync_record(self.client, &payload, self.access_token).await?;
        Ok(())
    }

    /// Sync a track to the cloud (metadata only).
    /// Audio file upload is handled separately by the file_sync service.
    pub async fn sync_track(&self, local_id: &str) -> Result<(), CloudSyncError> {
        let track = local_tracks::get_track(self.pool, local_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;

        // Upsert track metadata only — file upload handled by file_sync service
        let uid = require_uid(&track.uid)?;
        let payload = TrackPayload {
            id: &track.id,
            uid,
            track_hash: &track.track_hash,
            title: track.title.as_deref(),
            artist: track.artist.as_deref(),
            album: track.album.as_deref(),
            track_number: track.track_number,
            disc_number: track.disc_number,
            duration_seconds: track.duration_seconds,
            storage_path: track.storage_path.as_deref(),
            album_art_path: track.album_art_path.as_deref(),
            album_art_mime: track.album_art_mime.as_deref(),
        };

        sync_record(self.client, &payload, self.access_token).await?;
        Ok(())
    }

    // ========================================================================
    // Tier 2: Single Parent Dependency
    // ========================================================================

    /// Sync a fixture to the cloud.
    /// The fixture's venue_id is already the UUID used in the cloud.
    pub async fn sync_fixture(&self, local_id: &str) -> Result<(), CloudSyncError> {
        let fixture = local_fixtures::get_fixture(self.pool, local_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;

        let uid = require_uid(&fixture.uid)?;
        let payload = FixturePayload {
            id: &fixture.id,
            uid,
            venue_id: &fixture.venue_id,
            universe: fixture.universe,
            address: fixture.address,
            num_channels: fixture.num_channels,
            manufacturer: &fixture.manufacturer,
            model: &fixture.model,
            mode_name: &fixture.mode_name,
            fixture_path: &fixture.fixture_path,
            label: fixture.label.as_deref(),
            pos_x: fixture.pos_x,
            pos_y: fixture.pos_y,
            pos_z: fixture.pos_z,
            rot_x: fixture.rot_x,
            rot_y: fixture.rot_y,
            rot_z: fixture.rot_z,
        };

        sync_record(self.client, &payload, self.access_token).await?;
        Ok(())
    }

    /// Sync a pattern to the cloud.
    /// The pattern's category_id (if any) is already the UUID used in the cloud.
    pub async fn sync_pattern(&self, local_id: &str) -> Result<(), CloudSyncError> {
        let pattern = local_patterns::get_pattern_pool(self.pool, local_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;

        let uid = require_uid(&pattern.uid)?;
        let payload = PatternPayload {
            id: &pattern.id,
            uid,
            name: &pattern.name,
            description: pattern.description.as_deref(),
            category_id: pattern.category_id.as_deref(),
            is_published: pattern.is_published,
            author_name: pattern.author_name.as_deref(),
            forked_from_id: pattern.forked_from_id.as_deref(),
        };

        sync_record(self.client, &payload, self.access_token).await?;
        Ok(())
    }

    /// Sync a score to the cloud.
    /// The score's track_id and venue_id are already the UUIDs used in the cloud.
    /// Also syncs all child track_scores for this score.
    pub async fn sync_score(&self, local_id: &str) -> Result<(), CloudSyncError> {
        let score = local_scores::get_score(self.pool, local_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;

        let uid = require_uid(&score.uid)?;
        let payload = ScorePayload {
            id: &score.id,
            uid,
            track_id: &score.track_id,
            venue_id: &score.venue_id,
            name: score.name.as_deref(),
        };

        sync_record(self.client, &payload, self.access_token).await?;

        // Sync child track_scores
        self.sync_track_scores(&score.id).await?;

        Ok(())
    }

    /// Sync all track_scores for a score to the cloud.
    /// Pattern IDs in track_scores are already UUIDs -- no mapping needed.
    /// Returns the number of track_scores synced.
    pub async fn sync_track_scores(&self, score_id: &str) -> Result<usize, CloudSyncError> {
        let local_rows = local_scores::list_track_scores_for_score(self.pool, score_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;

        if local_rows.is_empty() {
            return Ok(0);
        }

        let payloads: Vec<TrackScorePayload> = local_rows
            .iter()
            .filter_map(|ts| {
                let uid = ts.uid.as_deref()?;
                let blend_mode_str = match serde_json::to_string(&ts.blend_mode) {
                    Ok(s) => s.trim_matches('"').to_string(),
                    Err(_) => "replace".to_string(),
                };

                Some(TrackScorePayload {
                    id: ts.id.clone(),
                    uid: uid.to_string(),
                    score_id: ts.score_id.clone(),
                    pattern_id: ts.pattern_id.clone(),
                    start_time: ts.start_time,
                    end_time: ts.end_time,
                    z_index: ts.z_index,
                    blend_mode: blend_mode_str,
                    args_json: ts.args.to_string(),
                })
            })
            .collect();

        sync_records(self.client, &payloads, self.access_token).await?;

        Ok(local_rows.len())
    }

    /// Sync track beats to the cloud.
    /// The beats' track_id is already the UUID used in the cloud.
    pub async fn sync_track_beats(&self, track_id: &str) -> Result<(), CloudSyncError> {
        let beats = local_tracks::get_track_beats(self.pool, track_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;

        let uid = require_uid(&beats.uid)?;
        let payload = TrackBeatsPayload {
            uid,
            track_id: &beats.track_id,
            beats_json: &beats.beats_json,
            downbeats_json: &beats.downbeats_json,
            bpm: beats.bpm,
            downbeat_offset: beats.downbeat_offset,
            beats_per_bar: beats.beats_per_bar,
        };

        sync_record(self.client, &payload, self.access_token).await?;
        Ok(())
    }

    /// Sync track roots to the cloud.
    /// The roots' track_id is already the UUID used in the cloud.
    pub async fn sync_track_roots(&self, track_id: &str) -> Result<(), CloudSyncError> {
        let roots = local_tracks::get_track_roots_model(self.pool, track_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;

        let uid = require_uid(&roots.uid)?;
        let payload = TrackRootsPayload {
            uid,
            track_id: &roots.track_id,
            sections_json: &roots.sections_json,
            logits_storage_path: roots.logits_storage_path.as_deref(),
        };

        sync_record(self.client, &payload, self.access_token).await?;
        Ok(())
    }

    /// Sync track waveform to the cloud.
    /// Note: Only preview waveform is synced; full waveform is regenerated locally.
    pub async fn sync_track_waveform(&self, track_id: &str) -> Result<(), CloudSyncError> {
        // Use the waveform service to get the properly deserialized waveform
        let waveform = waveform_service::get_track_waveform(self.pool, track_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;

        let uid = require_uid(&waveform.uid)?;
        let payload = TrackWaveformPayload {
            uid,
            track_id: &waveform.track_id,
            preview_samples: &waveform.preview_samples,
            preview_colors: waveform.preview_colors.as_deref(),
            preview_bands_low: waveform.preview_bands.as_ref().map(|b| b.low.as_slice()),
            preview_bands_mid: waveform.preview_bands.as_ref().map(|b| b.mid.as_slice()),
            preview_bands_high: waveform.preview_bands.as_ref().map(|b| b.high.as_slice()),
            sample_rate: waveform.sample_rate as i32,
            duration_seconds: waveform.duration_seconds,
        };

        sync_record(self.client, &payload, self.access_token).await?;
        Ok(())
    }

    /// Sync a track stem to the cloud (metadata only).
    /// Stem file upload is handled separately by the file_sync service.
    pub async fn sync_track_stem(
        &self,
        track_id: &str,
        stem_name: &str,
    ) -> Result<(), CloudSyncError> {
        let stem = local_tracks::get_track_stem(self.pool, track_id, stem_name)
            .await
            .map_err(CloudSyncError::LocalDb)?;

        // Upsert stem metadata only — file upload handled by file_sync service
        let uid = require_uid(&stem.uid)?;
        let payload = TrackStemPayload {
            uid,
            track_id: &stem.track_id,
            stem_name: &stem.stem_name,
            storage_path: stem.storage_path.as_deref(),
        };

        sync_record(self.client, &payload, self.access_token).await?;
        Ok(())
    }

    // ========================================================================
    // Tier 3: Multiple Dependencies
    // ========================================================================

    /// Sync an implementation to the cloud.
    /// The implementation's pattern_id is already the UUID used in the cloud.
    pub async fn sync_implementation(&self, local_id: &str) -> Result<(), CloudSyncError> {
        let implementation = local_implementations::get_implementation(self.pool, local_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;

        let uid = require_uid(&implementation.uid)?;
        let payload = ImplementationPayload {
            id: &implementation.id,
            uid,
            pattern_id: &implementation.pattern_id,
            name: implementation.name.as_deref(),
            graph_json: &implementation.graph_json,
        };

        sync_record(self.client, &payload, self.access_token).await?;
        Ok(())
    }

    // ========================================================================
    // Tier 4: Complex Dependencies
    // ========================================================================

    /// Sync a venue implementation override to the cloud.
    /// All FK IDs (venue_id, pattern_id, implementation_id) are already UUIDs.
    pub async fn sync_venue_override(
        &self,
        venue_id: &str,
        pattern_id: &str,
    ) -> Result<(), CloudSyncError> {
        let override_data = local_overrides::get_venue_override(self.pool, venue_id, pattern_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;

        let uid = require_uid(&override_data.uid)?;
        let payload = VenueOverridePayload {
            uid,
            venue_id: &override_data.venue_id,
            pattern_id: &override_data.pattern_id,
            implementation_id: &override_data.implementation_id,
        };

        sync_record(self.client, &payload, self.access_token).await?;
        Ok(())
    }

    // ========================================================================
    // Batch Operations
    // ========================================================================

    /// Sync a track with all its child data (beats, roots, waveform, stems)
    pub async fn sync_track_with_children(&self, track_id: &str) -> Result<(), CloudSyncError> {
        // Sync the track first
        self.sync_track(track_id).await?;

        // Sync beats if exists
        if local_tracks::track_has_beats(self.pool, track_id)
            .await
            .map_err(CloudSyncError::LocalDb)?
        {
            self.sync_track_beats(track_id).await?;
        }

        // Sync roots if exists
        if local_tracks::track_has_roots(self.pool, track_id)
            .await
            .map_err(CloudSyncError::LocalDb)?
        {
            self.sync_track_roots(track_id).await?;
        }

        // Sync waveform - uses the waveform service which handles missing waveforms
        if let Ok(_) = waveform_service::get_track_waveform(self.pool, track_id).await {
            self.sync_track_waveform(track_id).await?;
        }

        // Sync all stems
        let stem_names = local_tracks::list_track_stem_names(self.pool, track_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;
        for stem_name in stem_names {
            self.sync_track_stem(track_id, &stem_name).await?;
        }

        Ok(())
    }

    /// Sync a venue with all its fixtures and groups
    pub async fn sync_venue_with_children(&self, venue_id: &str) -> Result<(), CloudSyncError> {
        // Sync the venue first
        self.sync_venue(venue_id).await?;

        // Sync all fixtures for this venue
        let fixtures = local_fixtures::get_fixtures_for_venue(self.pool, venue_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;
        for fixture in fixtures {
            self.sync_fixture(&fixture.id).await?;
        }

        // Sync all groups for this venue
        let groups = local_groups::list_groups(self.pool, venue_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;
        for group in groups {
            let mc_json = group
                .movement_config
                .as_ref()
                .and_then(|mc| serde_json::to_string(mc).ok());
            let group_uid = group.uid.as_deref().unwrap_or(self.current_uid);
            let payload = GroupPayload {
                id: &group.id,
                uid: group_uid,
                venue_id,
                name: group.name.as_deref(),
                axis_lr: group.axis_lr,
                axis_fb: group.axis_fb,
                axis_ab: group.axis_ab,
                movement_config: mc_json.as_deref(),
                display_order: group.display_order,
            };
            if let Err(e) = sync_record(self.client, &payload, self.access_token).await {
                eprintln!("[cloud_sync] Failed to sync group {}: {}", group.id, e);
                continue;
            }

            // Sync group members
            let members = local_groups::get_group_member_ids(self.pool, &group.id)
                .await
                .map_err(CloudSyncError::LocalDb)?;
            if !members.is_empty() {
                let payloads: Vec<GroupMemberPayload> = members
                    .iter()
                    .map(|(fixture_id, display_order)| GroupMemberPayload {
                        fixture_id,
                        group_id: &group.id,
                        display_order: *display_order,
                    })
                    .collect();
                if let Err(e) = sync_records(self.client, &payloads, self.access_token).await {
                    eprintln!(
                        "[cloud_sync] Failed to sync group {} members: {}",
                        group.id, e
                    );
                }
            }
        }

        Ok(())
    }

    /// Sync a pattern with all its implementations
    pub async fn sync_pattern_with_children(&self, pattern_id: &str) -> Result<(), CloudSyncError> {
        // Sync the pattern first
        self.sync_pattern(pattern_id).await?;

        // Sync all implementations for this pattern
        let implementations =
            local_implementations::list_implementations_for_pattern(self.pool, pattern_id)
                .await
                .map_err(CloudSyncError::LocalDb)?;
        for impl_data in implementations {
            self.sync_implementation(&impl_data.id).await?;
        }

        Ok(())
    }

    /// Full sync: sync only dirty (changed since last sync) data in dependency order.
    /// On first sync after migration, all synced_at are NULL so everything syncs.
    /// Subsequent syncs only touch records where updated_at > synced_at.
    pub async fn sync_all(&self) -> Result<SyncStats, CloudSyncError> {
        let mut stats = SyncStats::default();

        // ================================================================
        // Tier 1: No dependencies — venues, categories, tracks
        // ================================================================

        // Venues (owner only)
        let dirty_venues = local_venues::list_dirty_venues(self.pool, self.current_uid)
            .await
            .map_err(CloudSyncError::LocalDb)?;
        stats.dirty_total += dirty_venues.len();
        for venue in &dirty_venues {
            match self.sync_venue(&venue.id).await {
                Ok(_) => {
                    stats.venues += 1;
                    local_venues::mark_venue_synced(self.pool, &venue.id)
                        .await
                        .map_err(CloudSyncError::LocalDb)?;
                }
                Err(e) => stats.errors.push(format!("Venue {}: {}", venue.id, e)),
            }
        }

        // Categories
        let dirty_categories = local_categories::list_dirty_categories(self.pool, self.current_uid)
            .await
            .map_err(CloudSyncError::LocalDb)?;
        stats.dirty_total += dirty_categories.len();
        for cat in &dirty_categories {
            match self.sync_category(&cat.id).await {
                Ok(_) => {
                    stats.categories += 1;
                    local_categories::mark_category_synced(self.pool, &cat.id)
                        .await
                        .map_err(CloudSyncError::LocalDb)?;
                }
                Err(e) => stats.errors.push(format!("Category {}: {}", cat.id, e)),
            }
        }

        // Tracks
        let dirty_tracks = local_tracks::list_dirty_tracks(self.pool, self.current_uid)
            .await
            .map_err(CloudSyncError::LocalDb)?;
        stats.dirty_total += dirty_tracks.len();
        for track in &dirty_tracks {
            match self.sync_track(&track.id).await {
                Ok(_) => {
                    stats.tracks += 1;
                    local_tracks::mark_track_synced(self.pool, &track.id)
                        .await
                        .map_err(CloudSyncError::LocalDb)?;
                }
                Err(e) => stats.errors.push(format!("Track {}: {}", track.id, e)),
            }
        }

        // ================================================================
        // Tier 2: Single parent — fixtures, groups, patterns, scores
        // ================================================================

        // Fixtures (via owned venues)
        let owned_venues = local_venues::list_venues_for_user(self.pool, self.current_uid)
            .await
            .map_err(CloudSyncError::LocalDb)?;

        for venue in &owned_venues {
            if !venue.is_owner() {
                continue;
            }
            let dirty_fixtures =
                local_fixtures::list_dirty_fixtures_for_venue(self.pool, &venue.id)
                    .await
                    .map_err(CloudSyncError::LocalDb)?;
            stats.dirty_total += dirty_fixtures.len();
            for fixture in &dirty_fixtures {
                match self.sync_fixture(&fixture.id).await {
                    Ok(_) => {
                        stats.fixtures += 1;
                        local_fixtures::mark_fixture_synced(self.pool, &fixture.id)
                            .await
                            .map_err(CloudSyncError::LocalDb)?;
                    }
                    Err(e) => stats.errors.push(format!("Fixture {}: {}", fixture.id, e)),
                }
            }
        }

        // Groups for owned venues (after fixtures so they exist in cloud)
        for venue in &owned_venues {
            if !venue.is_owner() {
                continue;
            }
            let dirty_groups = local_groups::list_dirty_groups(self.pool, &venue.id)
                .await
                .map_err(CloudSyncError::LocalDb)?;
            stats.dirty_total += dirty_groups.len();
            for group in &dirty_groups {
                let mc_json = group
                    .movement_config
                    .as_ref()
                    .and_then(|mc| serde_json::to_string(mc).ok());
                let group_uid = group.uid.as_deref().unwrap_or(self.current_uid);
                let payload = GroupPayload {
                    id: &group.id,
                    uid: group_uid,
                    venue_id: &venue.id,
                    name: group.name.as_deref(),
                    axis_lr: group.axis_lr,
                    axis_fb: group.axis_fb,
                    axis_ab: group.axis_ab,
                    movement_config: mc_json.as_deref(),
                    display_order: group.display_order,
                };
                match sync_record(self.client, &payload, self.access_token).await {
                    Ok(_) => {
                        stats.groups += 1;
                        // Always sync group members when the group is dirty
                        if let Ok(members) =
                            local_groups::get_group_member_ids(self.pool, &group.id).await
                        {
                            if !members.is_empty() {
                                let member_payloads: Vec<GroupMemberPayload> = members
                                    .iter()
                                    .map(|(fixture_id, display_order)| GroupMemberPayload {
                                        fixture_id,
                                        group_id: &group.id,
                                        display_order: *display_order,
                                    })
                                    .collect();
                                if let Err(e) =
                                    sync_records(self.client, &member_payloads, self.access_token)
                                        .await
                                {
                                    eprintln!(
                                        "[cloud_sync] Failed to sync group {} members ({} members): {}",
                                        group.id,
                                        members.len(),
                                        e
                                    );
                                }
                            }
                        }
                        local_groups::mark_group_synced(self.pool, &group.id)
                            .await
                            .map_err(CloudSyncError::LocalDb)?;
                    }
                    Err(e) => {
                        stats.errors.push(format!("Group {}: {:?}", group.id, e));
                    }
                }
            }
        }

        // Patterns
        let dirty_patterns = local_patterns::list_dirty_patterns(self.pool, self.current_uid)
            .await
            .map_err(CloudSyncError::LocalDb)?;
        stats.dirty_total += dirty_patterns.len();
        for pattern in &dirty_patterns {
            match self.sync_pattern(&pattern.id).await {
                Ok(_) => {
                    stats.patterns += 1;
                    local_patterns::mark_pattern_synced(self.pool, &pattern.id)
                        .await
                        .map_err(CloudSyncError::LocalDb)?;
                }
                Err(e) => stats.errors.push(format!("Pattern {}: {}", pattern.id, e)),
            }
        }

        // Scores (and their child track_scores)
        let dirty_scores = local_scores::list_dirty_scores(self.pool, self.current_uid)
            .await
            .map_err(CloudSyncError::LocalDb)?;
        stats.dirty_total += dirty_scores.len();
        for score in &dirty_scores {
            match self.sync_score(&score.id).await {
                Ok(_) => {
                    stats.scores += 1;
                    local_scores::mark_score_synced(self.pool, &score.id)
                        .await
                        .map_err(CloudSyncError::LocalDb)?;
                    // Mark all track_scores for this score as synced and count them
                    if let Ok(ts_list) =
                        local_scores::list_track_scores_for_score(self.pool, &score.id).await
                    {
                        stats.track_scores += ts_list.len();
                        for ts in &ts_list {
                            let _ = local_scores::mark_track_score_synced(self.pool, &ts.id).await;
                        }
                    }
                }
                Err(e) => stats.errors.push(format!("Score {}: {}", score.id, e)),
            }
        }

        // Also sync scores that are themselves clean but have dirty track_scores
        let scores_with_dirty_ts =
            local_scores::list_scores_with_dirty_track_scores(self.pool, self.current_uid)
                .await
                .map_err(CloudSyncError::LocalDb)?;
        for score_id in &scores_with_dirty_ts {
            // Re-sync all track_scores for this score (the score itself is already in cloud)
            match self.sync_track_scores(score_id).await {
                Ok(n) => {
                    stats.track_scores += n;
                    stats.dirty_total += n;
                    // Mark track_scores as synced
                    if let Ok(ts_list) =
                        local_scores::list_track_scores_for_score(self.pool, score_id).await
                    {
                        for ts in &ts_list {
                            let _ = local_scores::mark_track_score_synced(self.pool, &ts.id).await;
                        }
                    }
                }
                Err(e) => stats
                    .errors
                    .push(format!("TrackScores for {}: {}", score_id, e)),
            }
        }

        // Track child data — beats, roots, waveforms, stems (only dirty)
        let dirty_beats = local_tracks::list_dirty_track_beats(self.pool, self.current_uid)
            .await
            .map_err(CloudSyncError::LocalDb)?;
        stats.dirty_total += dirty_beats.len();
        for beats in &dirty_beats {
            match self.sync_track_beats(&beats.track_id).await {
                Ok(_) => {
                    stats.track_beats += 1;
                    local_tracks::mark_track_beats_synced(self.pool, &beats.track_id)
                        .await
                        .map_err(CloudSyncError::LocalDb)?;
                }
                Err(e) => stats
                    .errors
                    .push(format!("TrackBeats {}: {}", beats.track_id, e)),
            }
        }

        let dirty_roots = local_tracks::list_dirty_track_roots(self.pool, self.current_uid)
            .await
            .map_err(CloudSyncError::LocalDb)?;
        stats.dirty_total += dirty_roots.len();
        for roots in &dirty_roots {
            match self.sync_track_roots(&roots.track_id).await {
                Ok(_) => {
                    stats.track_roots += 1;
                    local_tracks::mark_track_roots_synced(self.pool, &roots.track_id)
                        .await
                        .map_err(CloudSyncError::LocalDb)?;
                }
                Err(e) => stats
                    .errors
                    .push(format!("TrackRoots {}: {}", roots.track_id, e)),
            }
        }

        let dirty_waveform_ids =
            local_waveforms::list_dirty_track_waveform_ids(self.pool, self.current_uid)
                .await
                .map_err(CloudSyncError::LocalDb)?;
        stats.dirty_total += dirty_waveform_ids.len();
        for track_id in &dirty_waveform_ids {
            match self.sync_track_waveform(track_id).await {
                Ok(_) => {
                    stats.track_waveforms += 1;
                    local_waveforms::mark_track_waveform_synced(self.pool, track_id)
                        .await
                        .map_err(CloudSyncError::LocalDb)?;
                }
                Err(e) => stats
                    .errors
                    .push(format!("TrackWaveform {}: {}", track_id, e)),
            }
        }

        let dirty_stems = local_tracks::list_dirty_track_stems(self.pool, self.current_uid)
            .await
            .map_err(CloudSyncError::LocalDb)?;
        stats.dirty_total += dirty_stems.len();
        for stem in &dirty_stems {
            match self.sync_track_stem(&stem.track_id, &stem.stem_name).await {
                Ok(_) => {
                    stats.track_stems += 1;
                    local_tracks::mark_track_stem_synced(
                        self.pool,
                        &stem.track_id,
                        &stem.stem_name,
                    )
                    .await
                    .map_err(CloudSyncError::LocalDb)?;
                }
                Err(e) => stats.errors.push(format!(
                    "TrackStem {}:{}: {}",
                    stem.track_id, stem.stem_name, e
                )),
            }
        }

        // ================================================================
        // Tier 3: Multiple dependencies — implementations
        // ================================================================
        let dirty_implementations =
            local_implementations::list_dirty_implementations(self.pool, self.current_uid)
                .await
                .map_err(CloudSyncError::LocalDb)?;
        stats.dirty_total += dirty_implementations.len();
        for impl_data in &dirty_implementations {
            match self.sync_implementation(&impl_data.id).await {
                Ok(_) => {
                    stats.implementations += 1;
                    local_implementations::mark_implementation_synced(self.pool, &impl_data.id)
                        .await
                        .map_err(CloudSyncError::LocalDb)?;
                }
                Err(e) => stats
                    .errors
                    .push(format!("Implementation {}: {}", impl_data.id, e)),
            }
        }

        // ================================================================
        // Tier 4: Complex dependencies — venue overrides
        // ================================================================
        let dirty_overrides =
            local_overrides::list_dirty_venue_overrides(self.pool, self.current_uid)
                .await
                .map_err(CloudSyncError::LocalDb)?;
        stats.dirty_total += dirty_overrides.len();
        for override_data in &dirty_overrides {
            match self
                .sync_venue_override(&override_data.venue_id, &override_data.pattern_id)
                .await
            {
                Ok(_) => {
                    stats.venue_overrides += 1;
                    local_overrides::mark_venue_override_synced(
                        self.pool,
                        &override_data.venue_id,
                        &override_data.pattern_id,
                    )
                    .await
                    .map_err(CloudSyncError::LocalDb)?;
                }
                Err(e) => stats.errors.push(format!(
                    "VenueOverride ({}, {}): {}",
                    override_data.venue_id, override_data.pattern_id, e
                )),
            }
        }

        Ok(stats)
    }

    // ========================================================================
    // Delete Operations
    // ========================================================================

    /// Delete a venue from the cloud (does not delete locally)
    pub async fn delete_venue_from_cloud(&self, local_id: &str) -> Result<(), CloudSyncError> {
        delete_record(self.client, "venues", local_id, self.access_token).await?;
        Ok(())
    }

    /// Delete a fixture from the cloud (does not delete locally)
    pub async fn delete_fixture_from_cloud(&self, local_id: &str) -> Result<(), CloudSyncError> {
        delete_record(self.client, "fixtures", local_id, self.access_token).await?;
        Ok(())
    }

    /// Delete a track from the cloud (does not delete locally)
    pub async fn delete_track_from_cloud(&self, local_id: &str) -> Result<(), CloudSyncError> {
        delete_record(self.client, "tracks", local_id, self.access_token).await?;
        Ok(())
    }

    /// Delete a pattern from the cloud (does not delete locally)
    pub async fn delete_pattern_from_cloud(&self, local_id: &str) -> Result<(), CloudSyncError> {
        delete_record(self.client, "patterns", local_id, self.access_token).await?;
        Ok(())
    }

    /// Delete a category from the cloud (does not delete locally)
    pub async fn delete_category_from_cloud(&self, local_id: &str) -> Result<(), CloudSyncError> {
        delete_record(
            self.client,
            "pattern_categories",
            local_id,
            self.access_token,
        )
        .await?;
        Ok(())
    }
}
