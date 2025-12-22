//! Cloud sync orchestration service
//!
//! Coordinates syncing between local SQLite and Supabase:
//! 1. Reads records from local database (via database/local/)
//! 2. Resolves foreign key relationships (ensures parents are synced first)
//! 3. Calls remote database functions to upsert/delete (via database/remote/)
//! 4. Updates local database with cloud-generated remote_ids

use sqlx::SqlitePool;

use crate::database::local::{
    categories as local_categories, fixtures as local_fixtures,
    implementations as local_implementations, patterns as local_patterns, scores as local_scores,
    tracks as local_tracks, venue_overrides as local_overrides, venues as local_venues,
};
use crate::database::remote::common::{SupabaseClient, SyncError};
use crate::database::remote::{
    categories, fixtures, implementations, overrides, patterns, scores, track_beats, track_roots,
    track_scores, track_stems, tracks, venues,
};
use crate::models::node_graph::BlendMode;
use crate::models::scores::TrackScore;
use crate::services::waveforms as waveform_service;
use serde_json::Value;

// ============================================================================
// Error Types
// ============================================================================

#[derive(Debug)]
pub enum CloudSyncError {
    /// Error from local database operation
    LocalDb(String),
    /// Error from remote Supabase operation
    Remote(SyncError),
    /// Record hasn't been synced yet (no remote_id)
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
                write!(f, "{} with id {} has not been synced yet", table, local_id)
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
    pub track_beats: usize,
    pub track_roots: usize,
    pub track_waveforms: usize,
    pub track_stems: usize,
    pub implementations: usize,
    pub track_scores: usize,
    pub venue_overrides: usize,
    pub errors: Vec<String>,
}

// ============================================================================
// Sync Context
// ============================================================================

/// Cloud sync orchestrator that coordinates local and remote database operations
pub struct CloudSync<'a> {
    pub pool: &'a SqlitePool,
    pub client: &'a SupabaseClient,
    pub access_token: &'a str,
}

impl<'a> CloudSync<'a> {
    pub fn new(pool: &'a SqlitePool, client: &'a SupabaseClient, access_token: &'a str) -> Self {
        Self {
            pool,
            client,
            access_token,
        }
    }

    // ========================================================================
    // Helper: Parse remote_id from Option<String>
    // ========================================================================

    fn parse_remote_id(remote_id: &Option<String>) -> Option<i64> {
        remote_id.as_ref().and_then(|s| s.parse::<i64>().ok())
    }

    fn require_parsed_remote_id(
        remote_id: &Option<String>,
        table: &str,
        local_id: &str,
    ) -> Result<i64, CloudSyncError> {
        Self::parse_remote_id(remote_id).ok_or_else(|| CloudSyncError::NotSynced {
            table: table.to_string(),
            local_id: local_id.to_string(),
        })
    }

    // ========================================================================
    // Tier 1: No Dependencies
    // ========================================================================

    /// Sync a venue to the cloud. Returns the cloud remote_id.
    pub async fn sync_venue(&self, local_id: i64) -> Result<i64, CloudSyncError> {
        let venue = local_venues::get_venue(self.pool, local_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;

        let remote_id = venues::upsert_venue(self.client, &venue, self.access_token).await?;
        local_venues::set_remote_id(self.pool, local_id, remote_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;
        Ok(remote_id)
    }

    /// Sync a pattern category to the cloud. Returns the cloud remote_id.
    pub async fn sync_category(&self, local_id: i64) -> Result<i64, CloudSyncError> {
        let category = local_categories::get_category(self.pool, local_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;

        let remote_id =
            categories::upsert_category(self.client, &category, self.access_token).await?;
        local_categories::set_remote_id(self.pool, local_id, remote_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;
        Ok(remote_id)
    }

    /// Sync a track to the cloud. Returns the cloud remote_id.
    pub async fn sync_track(&self, local_id: i64) -> Result<i64, CloudSyncError> {
        let track = local_tracks::get_track(self.pool, local_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;

        let remote_id = tracks::upsert_track(self.client, &track, self.access_token).await?;
        local_tracks::set_remote_id(self.pool, local_id, remote_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;
        Ok(remote_id)
    }

    // ========================================================================
    // Tier 2: Single Parent Dependency
    // ========================================================================

    /// Sync a fixture to the cloud. Ensures parent venue is synced first.
    pub async fn sync_fixture(&self, local_id: &str) -> Result<i64, CloudSyncError> {
        let fixture = local_fixtures::get_fixture(self.pool, local_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;

        // Ensure parent venue is synced - get its remote_id or sync it
        let venue = local_venues::get_venue(self.pool, fixture.venue_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;
        let venue_remote_id = match Self::parse_remote_id(&venue.remote_id) {
            Some(id) => id,
            None => self.sync_venue(fixture.venue_id).await?,
        };

        let remote_id =
            fixtures::upsert_fixture(self.client, &fixture, venue_remote_id, self.access_token)
                .await?;
        local_fixtures::set_remote_id(self.pool, local_id, remote_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;
        Ok(remote_id)
    }

    /// Sync a pattern to the cloud. Ensures parent category is synced first (if any).
    pub async fn sync_pattern(&self, local_id: i64) -> Result<i64, CloudSyncError> {
        let pattern = local_patterns::get_pattern_pool(self.pool, local_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;

        // Ensure parent category is synced (if any)
        let category_remote_id = match pattern.category_id {
            Some(cat_id) => {
                let cat = local_categories::get_category(self.pool, cat_id)
                    .await
                    .map_err(CloudSyncError::LocalDb)?;
                match Self::parse_remote_id(&cat.remote_id) {
                    Some(id) => Some(id),
                    None => Some(self.sync_category(cat_id).await?),
                }
            }
            None => None,
        };

        let remote_id =
            patterns::upsert_pattern(self.client, &pattern, category_remote_id, self.access_token)
                .await?;
        local_patterns::set_remote_id(self.pool, local_id, remote_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;
        Ok(remote_id)
    }

    /// Sync a score to the cloud. Ensures parent track is synced first.
    pub async fn sync_score(&self, local_id: i64) -> Result<i64, CloudSyncError> {
        let score = local_scores::get_score(self.pool, local_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;

        // Ensure parent track is synced
        let track = local_tracks::get_track(self.pool, score.track_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;
        let track_remote_id = match Self::parse_remote_id(&track.remote_id) {
            Some(id) => id,
            None => self.sync_track(score.track_id).await?,
        };

        let remote_id =
            scores::upsert_score(self.client, &score, track_remote_id, self.access_token).await?;
        local_scores::set_score_remote_id(self.pool, local_id, remote_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;
        Ok(remote_id)
    }

    /// Sync track beats to the cloud. Ensures parent track is synced first.
    pub async fn sync_track_beats(&self, track_id: i64) -> Result<i64, CloudSyncError> {
        let beats = local_tracks::get_track_beats(self.pool, track_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;

        // Ensure parent track is synced
        let track = local_tracks::get_track(self.pool, track_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;
        let track_remote_id = match Self::parse_remote_id(&track.remote_id) {
            Some(id) => id,
            None => self.sync_track(track_id).await?,
        };

        let remote_id = track_beats::upsert_track_beats(
            self.client,
            &beats,
            track_remote_id,
            self.access_token,
        )
        .await?;
        local_tracks::set_track_beats_remote_id(self.pool, track_id, remote_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;
        Ok(remote_id)
    }

    /// Sync track roots to the cloud. Ensures parent track is synced first.
    pub async fn sync_track_roots(&self, track_id: i64) -> Result<i64, CloudSyncError> {
        let roots = local_tracks::get_track_roots_model(self.pool, track_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;

        // Ensure parent track is synced
        let track = local_tracks::get_track(self.pool, track_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;
        let track_remote_id = match Self::parse_remote_id(&track.remote_id) {
            Some(id) => id,
            None => self.sync_track(track_id).await?,
        };

        let remote_id = track_roots::upsert_track_roots(
            self.client,
            &roots,
            track_remote_id,
            self.access_token,
        )
        .await?;
        local_tracks::set_track_roots_remote_id(self.pool, track_id, remote_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;
        Ok(remote_id)
    }

    /// Sync track waveform to the cloud. Ensures parent track is synced first.
    /// Note: Only preview waveform is synced; full waveform is regenerated locally.
    pub async fn sync_track_waveform(&self, track_id: i64) -> Result<(), CloudSyncError> {
        // Use the waveform service to get the properly deserialized waveform
        let waveform = waveform_service::get_track_waveform(self.pool, track_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;

        // Ensure parent track is synced
        let track = local_tracks::get_track(self.pool, track_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;
        let track_remote_id = match Self::parse_remote_id(&track.remote_id) {
            Some(id) => id,
            None => self.sync_track(track_id).await?,
        };

        // Note: upsert_track_waveform returns () (waveforms don't track their own cloud ID separately)
        track_waveforms::upsert_track_waveform(
            self.client,
            &waveform,
            track_remote_id,
            self.access_token,
        )
        .await?;
        Ok(())
    }

    /// Sync a track stem to the cloud. Ensures parent track is synced first.
    pub async fn sync_track_stem(
        &self,
        track_id: i64,
        stem_name: &str,
    ) -> Result<i64, CloudSyncError> {
        let stem = local_tracks::get_track_stem(self.pool, track_id, stem_name)
            .await
            .map_err(CloudSyncError::LocalDb)?;

        // Ensure parent track is synced
        let track = local_tracks::get_track(self.pool, track_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;
        let track_remote_id = match Self::parse_remote_id(&track.remote_id) {
            Some(id) => id,
            None => self.sync_track(track_id).await?,
        };

        let remote_id =
            track_stems::upsert_track_stem(self.client, &stem, track_remote_id, self.access_token)
                .await?;
        local_tracks::set_track_stem_remote_id(self.pool, track_id, stem_name, remote_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;
        Ok(remote_id)
    }

    // ========================================================================
    // Tier 3: Multiple Dependencies
    // ========================================================================

    /// Sync an implementation to the cloud. Ensures parent pattern is synced first.
    pub async fn sync_implementation(&self, local_id: i64) -> Result<i64, CloudSyncError> {
        let implementation = local_implementations::get_implementation(self.pool, local_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;

        // Ensure parent pattern is synced
        let pattern = local_patterns::get_pattern_pool(self.pool, implementation.pattern_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;
        let pattern_remote_id = match Self::parse_remote_id(&pattern.remote_id) {
            Some(id) => id,
            None => self.sync_pattern(implementation.pattern_id).await?,
        };

        let remote_id = implementations::upsert_implementation(
            self.client,
            &implementation,
            pattern_remote_id,
            self.access_token,
        )
        .await?;
        local_implementations::set_remote_id(self.pool, local_id, remote_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;
        Ok(remote_id)
    }

    /// Sync a track score to the cloud. Ensures parent score and pattern are synced first.
    pub async fn sync_track_score(&self, local_id: i64) -> Result<i64, CloudSyncError> {
        // TrackScore doesn't implement FromRow, so we get raw row and convert
        let row = local_scores::get_track_score_row(self.pool, local_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;

        let (
            id,
            remote_id,
            uid,
            score_id,
            pattern_id,
            start_time,
            end_time,
            z_index,
            blend_mode_str,
            args_json,
            created_at,
            updated_at,
        ) = row;

        let blend_mode: BlendMode =
            serde_json::from_str(&format!("\"{}\"", blend_mode_str)).unwrap_or(BlendMode::Replace);
        let args: Value =
            serde_json::from_str(&args_json).unwrap_or_else(|_| Value::Object(Default::default()));

        let track_score = TrackScore {
            id,
            remote_id,
            uid,
            score_id,
            pattern_id,
            start_time,
            end_time,
            z_index,
            blend_mode,
            args,
            created_at,
            updated_at,
        };

        // Ensure parent score is synced
        let score = local_scores::get_score(self.pool, track_score.score_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;
        let score_remote_id = match Self::parse_remote_id(&score.remote_id) {
            Some(id) => id,
            None => self.sync_score(track_score.score_id).await?,
        };

        // Ensure parent pattern is synced
        let pattern = local_patterns::get_pattern_pool(self.pool, track_score.pattern_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;
        let pattern_remote_id = match Self::parse_remote_id(&pattern.remote_id) {
            Some(id) => id,
            None => self.sync_pattern(track_score.pattern_id).await?,
        };

        let cloud_id = track_scores::upsert_track_score(
            self.client,
            &track_score,
            score_remote_id,
            pattern_remote_id,
            self.access_token,
        )
        .await?;
        local_scores::set_track_score_remote_id(self.pool, local_id, cloud_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;
        Ok(cloud_id)
    }

    // ========================================================================
    // Tier 4: Complex Dependencies
    // ========================================================================

    /// Sync a venue implementation override to the cloud.
    /// Ensures parent venue, pattern, and implementation are synced first.
    pub async fn sync_venue_override(
        &self,
        venue_id: i64,
        pattern_id: i64,
    ) -> Result<i64, CloudSyncError> {
        let override_data = local_overrides::get_venue_override(self.pool, venue_id, pattern_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;

        // Ensure parent venue is synced
        let venue = local_venues::get_venue(self.pool, venue_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;
        let venue_remote_id = match Self::parse_remote_id(&venue.remote_id) {
            Some(id) => id,
            None => self.sync_venue(venue_id).await?,
        };

        // Ensure parent pattern is synced
        let pattern = local_patterns::get_pattern_pool(self.pool, pattern_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;
        let pattern_remote_id = match Self::parse_remote_id(&pattern.remote_id) {
            Some(id) => id,
            None => self.sync_pattern(pattern_id).await?,
        };

        // Ensure parent implementation is synced
        let implementation =
            local_implementations::get_implementation(self.pool, override_data.implementation_id)
                .await
                .map_err(CloudSyncError::LocalDb)?;
        let impl_remote_id = match Self::parse_remote_id(&implementation.remote_id) {
            Some(id) => id,
            None => {
                self.sync_implementation(override_data.implementation_id)
                    .await?
            }
        };

        let cloud_id = overrides::upsert_venue_override(
            self.client,
            &override_data,
            venue_remote_id,
            pattern_remote_id,
            impl_remote_id,
            self.access_token,
        )
        .await?;
        local_overrides::set_remote_id(self.pool, venue_id, pattern_id, cloud_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;
        Ok(cloud_id)
    }

    // ========================================================================
    // Batch Operations
    // ========================================================================

    /// Sync all venues to the cloud
    pub async fn sync_all_venues(&self) -> Result<Vec<i64>, CloudSyncError> {
        let venues = local_venues::list_venues(self.pool)
            .await
            .map_err(CloudSyncError::LocalDb)?;

        let mut remote_ids = Vec::new();
        for venue in venues {
            remote_ids.push(self.sync_venue(venue.id).await?);
        }
        Ok(remote_ids)
    }

    /// Sync all categories to the cloud
    pub async fn sync_all_categories(&self) -> Result<Vec<i64>, CloudSyncError> {
        let categories = local_categories::list_pattern_categories_pool(self.pool)
            .await
            .map_err(CloudSyncError::LocalDb)?;

        let mut remote_ids = Vec::new();
        for cat in categories {
            remote_ids.push(self.sync_category(cat.id).await?);
        }
        Ok(remote_ids)
    }

    /// Sync all tracks to the cloud
    pub async fn sync_all_tracks(&self) -> Result<Vec<i64>, CloudSyncError> {
        let tracks = local_tracks::list_tracks(self.pool)
            .await
            .map_err(CloudSyncError::LocalDb)?;

        let mut remote_ids = Vec::new();
        for track in tracks {
            remote_ids.push(self.sync_track(track.id).await?);
        }
        Ok(remote_ids)
    }

    /// Sync a track with all its child data (beats, roots, waveform, stems)
    pub async fn sync_track_with_children(&self, track_id: i64) -> Result<(), CloudSyncError> {
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

    /// Sync a venue with all its fixtures
    pub async fn sync_venue_with_children(&self, venue_id: i64) -> Result<(), CloudSyncError> {
        // Sync the venue first
        self.sync_venue(venue_id).await?;

        // Sync all fixtures for this venue
        let fixtures = local_fixtures::get_fixtures_for_venue(self.pool, venue_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;
        for fixture in fixtures {
            self.sync_fixture(&fixture.id).await?;
        }

        Ok(())
    }

    /// Sync a pattern with all its implementations
    pub async fn sync_pattern_with_children(&self, pattern_id: i64) -> Result<(), CloudSyncError> {
        // Sync the pattern first
        self.sync_pattern(pattern_id).await?;

        // Sync all implementations for this pattern
        let implementations =
            local_implementations::list_implementations_for_pattern(self.pool, pattern_id)
                .await
                .map_err(CloudSyncError::LocalDb)?;
        for impl_data in implementations {
            self.sync_implementation(impl_data.id).await?;
        }

        Ok(())
    }

    /// Full sync: sync all data to the cloud in dependency order
    pub async fn sync_all(&self) -> Result<SyncStats, CloudSyncError> {
        let mut stats = SyncStats::default();

        // Tier 1: No dependencies
        match self.sync_all_venues().await {
            Ok(ids) => stats.venues = ids.len(),
            Err(e) => stats.errors.push(format!("Venues: {}", e)),
        }

        match self.sync_all_categories().await {
            Ok(ids) => stats.categories = ids.len(),
            Err(e) => stats.errors.push(format!("Categories: {}", e)),
        }

        match self.sync_all_tracks().await {
            Ok(ids) => stats.tracks = ids.len(),
            Err(e) => stats.errors.push(format!("Tracks: {}", e)),
        }

        // Tier 2: Single parent
        let fixtures = local_fixtures::list_all_fixtures(self.pool)
            .await
            .map_err(CloudSyncError::LocalDb)?;
        for fixture in fixtures {
            match self.sync_fixture(&fixture.id).await {
                Ok(_) => stats.fixtures += 1,
                Err(e) => stats.errors.push(format!("Fixture {}: {}", fixture.id, e)),
            }
        }

        let patterns = local_patterns::list_patterns_pool(self.pool)
            .await
            .map_err(CloudSyncError::LocalDb)?;
        for pattern in patterns {
            match self.sync_pattern(pattern.id).await {
                Ok(_) => stats.patterns += 1,
                Err(e) => stats.errors.push(format!("Pattern {}: {}", pattern.id, e)),
            }
        }

        let scores = local_scores::list_scores(self.pool)
            .await
            .map_err(CloudSyncError::LocalDb)?;
        for score in scores {
            match self.sync_score(score.id).await {
                Ok(_) => stats.scores += 1,
                Err(e) => stats.errors.push(format!("Score {}: {}", score.id, e)),
            }
        }

        // Track child data - iterate over all tracks
        let tracks = local_tracks::list_tracks(self.pool)
            .await
            .map_err(CloudSyncError::LocalDb)?;
        for track in &tracks {
            // Beats
            if local_tracks::track_has_beats(self.pool, track.id)
                .await
                .unwrap_or(false)
            {
                match self.sync_track_beats(track.id).await {
                    Ok(_) => stats.track_beats += 1,
                    Err(e) => stats.errors.push(format!("TrackBeats {}: {}", track.id, e)),
                }
            }

            // Roots
            if local_tracks::track_has_roots(self.pool, track.id)
                .await
                .unwrap_or(false)
            {
                match self.sync_track_roots(track.id).await {
                    Ok(_) => stats.track_roots += 1,
                    Err(e) => stats.errors.push(format!("TrackRoots {}: {}", track.id, e)),
                }
            }

            // Waveform
            if let Ok(_) = waveform_service::get_track_waveform(self.pool, track.id).await {
                match self.sync_track_waveform(track.id).await {
                    Ok(_) => stats.track_waveforms += 1,
                    Err(e) => stats
                        .errors
                        .push(format!("TrackWaveform {}: {}", track.id, e)),
                }
            }

            // Stems
            if let Ok(stem_names) = local_tracks::list_track_stem_names(self.pool, track.id).await {
                for stem_name in stem_names {
                    match self.sync_track_stem(track.id, &stem_name).await {
                        Ok(_) => stats.track_stems += 1,
                        Err(e) => stats
                            .errors
                            .push(format!("TrackStem {}:{}: {}", track.id, stem_name, e)),
                    }
                }
            }
        }

        // Tier 3: Multiple dependencies
        let implementations = local_implementations::list_implementations(self.pool)
            .await
            .map_err(CloudSyncError::LocalDb)?;
        for impl_data in implementations {
            match self.sync_implementation(impl_data.id).await {
                Ok(_) => stats.implementations += 1,
                Err(e) => stats
                    .errors
                    .push(format!("Implementation {}: {}", impl_data.id, e)),
            }
        }

        let track_score_ids = local_scores::list_track_score_ids(self.pool)
            .await
            .map_err(CloudSyncError::LocalDb)?;
        for id in track_score_ids {
            match self.sync_track_score(id).await {
                Ok(_) => stats.track_scores += 1,
                Err(e) => stats.errors.push(format!("TrackScore {}: {}", id, e)),
            }
        }

        // Tier 4: Complex dependencies
        let venue_overrides = local_overrides::list_venue_overrides(self.pool)
            .await
            .map_err(CloudSyncError::LocalDb)?;
        for override_data in venue_overrides {
            match self
                .sync_venue_override(override_data.venue_id, override_data.pattern_id)
                .await
            {
                Ok(_) => stats.venue_overrides += 1,
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
    pub async fn delete_venue_from_cloud(&self, local_id: i64) -> Result<(), CloudSyncError> {
        let venue = local_venues::get_venue(self.pool, local_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;
        let remote_id =
            Self::require_parsed_remote_id(&venue.remote_id, "venues", &local_id.to_string())?;

        venues::delete_venue(self.client, remote_id, self.access_token).await?;
        local_venues::clear_remote_id(self.pool, local_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;
        Ok(())
    }

    /// Delete a fixture from the cloud (does not delete locally)
    pub async fn delete_fixture_from_cloud(&self, local_id: &str) -> Result<(), CloudSyncError> {
        let fixture = local_fixtures::get_fixture(self.pool, local_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;
        let remote_id = Self::require_parsed_remote_id(&fixture.remote_id, "fixtures", local_id)?;

        fixtures::delete_fixture(self.client, remote_id, self.access_token).await?;
        local_fixtures::clear_remote_id(self.pool, local_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;
        Ok(())
    }

    /// Delete a track from the cloud (does not delete locally)
    pub async fn delete_track_from_cloud(&self, local_id: i64) -> Result<(), CloudSyncError> {
        let track = local_tracks::get_track(self.pool, local_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;
        let remote_id =
            Self::require_parsed_remote_id(&track.remote_id, "tracks", &local_id.to_string())?;

        tracks::delete_track(self.client, remote_id, self.access_token).await?;
        local_tracks::clear_remote_id(self.pool, local_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;
        Ok(())
    }

    /// Delete a pattern from the cloud (does not delete locally)
    pub async fn delete_pattern_from_cloud(&self, local_id: i64) -> Result<(), CloudSyncError> {
        let pattern = local_patterns::get_pattern_pool(self.pool, local_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;
        let remote_id =
            Self::require_parsed_remote_id(&pattern.remote_id, "patterns", &local_id.to_string())?;

        patterns::delete_pattern(self.client, remote_id, self.access_token).await?;
        local_patterns::clear_remote_id(self.pool, local_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;
        Ok(())
    }

    /// Delete a category from the cloud (does not delete locally)
    pub async fn delete_category_from_cloud(&self, local_id: i64) -> Result<(), CloudSyncError> {
        let category = local_categories::get_category(self.pool, local_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;
        let remote_id = Self::require_parsed_remote_id(
            &category.remote_id,
            "pattern_categories",
            &local_id.to_string(),
        )?;

        categories::delete_category(self.client, remote_id, self.access_token).await?;
        local_categories::clear_remote_id(self.pool, local_id)
            .await
            .map_err(CloudSyncError::LocalDb)?;
        Ok(())
    }
}

// Bring track_waveforms into scope for the waveform sync
use crate::database::remote::track_waveforms;
