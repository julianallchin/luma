//! Tauri commands for cloud sync operations

use serde::{Deserialize, Serialize};
use tauri::State;
use ts_rs::TS;

use crate::database::local::auth;
use crate::database::local::state::StateDb;
use crate::database::remote::common::SupabaseClient;
use crate::database::Db;
use crate::services::cloud_sync::{CloudSync, SyncStats};
use crate::services::community_patterns;

const SUPABASE_URL: &str = "https://smuuycypmsutwrkpctws.supabase.co";
const SUPABASE_ANON_KEY: &str = "sb_publishable_V8JRQkGliRYDAiGghjUrmQ_w8fpfjRb";

/// Entry identifying a score to sync (by track + venue)
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoreEntry {
    pub track_id: i64,
    pub venue_id: i64,
}

/// Result type for sync commands
#[derive(Debug, Serialize, TS)]
#[ts(export, export_to = "../../src/bindings/sync.ts")]
#[serde(rename_all = "camelCase")]
pub struct SyncResult {
    pub success: bool,
    pub message: String,
    pub stats: Option<SyncStatsDto>,
}

/// DTO for sync statistics
#[derive(Debug, Serialize, TS)]
#[ts(export, export_to = "../../src/bindings/sync.ts")]
#[serde(rename_all = "camelCase")]
pub struct SyncStatsDto {
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
    pub errors: Vec<String>,
}

impl From<SyncStats> for SyncStatsDto {
    fn from(s: SyncStats) -> Self {
        SyncStatsDto {
            venues: s.venues,
            categories: s.categories,
            tracks: s.tracks,
            fixtures: s.fixtures,
            patterns: s.patterns,
            scores: s.scores,
            track_scores: s.track_scores,
            track_beats: s.track_beats,
            track_roots: s.track_roots,
            track_waveforms: s.track_waveforms,
            track_stems: s.track_stems,
            implementations: s.implementations,
            venue_overrides: s.venue_overrides,
            errors: s.errors,
        }
    }
}

/// Helper to get access token or return error
async fn require_auth(state_db: &StateDb) -> Result<String, String> {
    auth::get_current_access_token(&state_db.0)
        .await?
        .ok_or_else(|| "Not authenticated - please sign in first".to_string())
}

/// Sync all local data to the cloud
#[tauri::command]
pub async fn sync_all(
    db: State<'_, Db>,
    state_db: State<'_, StateDb>,
) -> Result<SyncResult, String> {
    let token = require_auth(&state_db).await?;
    let client = SupabaseClient::new(SUPABASE_URL.to_string(), SUPABASE_ANON_KEY.to_string());
    let sync = CloudSync::new(&db.0, &client, &token);

    match sync.sync_all().await {
        Ok(stats) => {
            let error_count = stats.errors.len();
            let total = stats.venues
                + stats.categories
                + stats.tracks
                + stats.fixtures
                + stats.patterns
                + stats.scores
                + stats.track_scores
                + stats.implementations
                + stats.venue_overrides;
            println!(
                "[cloud_sync] Synced {} records ({} errors). venues={} fixtures={} tracks={} beats={} roots={} waveforms={} stems={} scores={} track_scores={} patterns={} impls={} categories={}",
                total, error_count, stats.venues, stats.fixtures, stats.tracks,
                stats.track_beats, stats.track_roots, stats.track_waveforms, stats.track_stems,
                stats.scores, stats.track_scores, stats.patterns, stats.implementations, stats.categories
            );
            for err in &stats.errors {
                println!("[cloud_sync] ERROR: {}", err);
            }
            Ok(SyncResult {
                success: error_count == 0,
                message: format!("Synced {} records with {} errors", total, error_count),
                stats: Some(stats.into()),
            })
        }
        Err(e) => Ok(SyncResult {
            success: false,
            message: format!("Sync failed: {}", e),
            stats: None,
        }),
    }
}

/// Sync a specific venue to the cloud
#[tauri::command]
pub async fn sync_venue(
    db: State<'_, Db>,
    state_db: State<'_, StateDb>,
    venue_id: i64,
) -> Result<SyncResult, String> {
    let token = require_auth(&state_db).await?;
    let client = SupabaseClient::new(SUPABASE_URL.to_string(), SUPABASE_ANON_KEY.to_string());
    let sync = CloudSync::new(&db.0, &client, &token);

    match sync.sync_venue(venue_id).await {
        Ok(_) => Ok(SyncResult {
            success: true,
            message: "Venue synced successfully".to_string(),
            stats: None,
        }),
        Err(e) => Ok(SyncResult {
            success: false,
            message: format!("Failed to sync venue: {}", e),
            stats: None,
        }),
    }
}

/// Sync a venue with all its fixtures
#[tauri::command]
pub async fn sync_venue_with_fixtures(
    db: State<'_, Db>,
    state_db: State<'_, StateDb>,
    venue_id: i64,
) -> Result<SyncResult, String> {
    let token = require_auth(&state_db).await?;
    let client = SupabaseClient::new(SUPABASE_URL.to_string(), SUPABASE_ANON_KEY.to_string());
    let sync = CloudSync::new(&db.0, &client, &token);

    match sync.sync_venue_with_children(venue_id).await {
        Ok(_) => Ok(SyncResult {
            success: true,
            message: "Venue and fixtures synced successfully".to_string(),
            stats: None,
        }),
        Err(e) => Ok(SyncResult {
            success: false,
            message: format!("Failed to sync venue: {}", e),
            stats: None,
        }),
    }
}

/// Sync a specific track to the cloud
#[tauri::command]
pub async fn sync_track(
    db: State<'_, Db>,
    state_db: State<'_, StateDb>,
    track_id: i64,
) -> Result<SyncResult, String> {
    let token = require_auth(&state_db).await?;
    let client = SupabaseClient::new(SUPABASE_URL.to_string(), SUPABASE_ANON_KEY.to_string());
    let sync = CloudSync::new(&db.0, &client, &token);

    match sync.sync_track(track_id).await {
        Ok(_) => Ok(SyncResult {
            success: true,
            message: "Track synced successfully".to_string(),
            stats: None,
        }),
        Err(e) => Ok(SyncResult {
            success: false,
            message: format!("Failed to sync track: {}", e),
            stats: None,
        }),
    }
}

/// Sync a track with all its child data (beats, roots, waveform, stems)
#[tauri::command]
pub async fn sync_track_with_data(
    db: State<'_, Db>,
    state_db: State<'_, StateDb>,
    track_id: i64,
) -> Result<SyncResult, String> {
    let token = require_auth(&state_db).await?;
    let client = SupabaseClient::new(SUPABASE_URL.to_string(), SUPABASE_ANON_KEY.to_string());
    let sync = CloudSync::new(&db.0, &client, &token);

    match sync.sync_track_with_children(track_id).await {
        Ok(_) => Ok(SyncResult {
            success: true,
            message: "Track and related data synced successfully".to_string(),
            stats: None,
        }),
        Err(e) => Ok(SyncResult {
            success: false,
            message: format!("Failed to sync track: {}", e),
            stats: None,
        }),
    }
}

/// Sync a specific pattern to the cloud
#[tauri::command]
pub async fn sync_pattern(
    db: State<'_, Db>,
    state_db: State<'_, StateDb>,
    pattern_id: i64,
) -> Result<SyncResult, String> {
    let token = require_auth(&state_db).await?;
    let client = SupabaseClient::new(SUPABASE_URL.to_string(), SUPABASE_ANON_KEY.to_string());
    let sync = CloudSync::new(&db.0, &client, &token);

    match sync.sync_pattern(pattern_id).await {
        Ok(_) => Ok(SyncResult {
            success: true,
            message: "Pattern synced successfully".to_string(),
            stats: None,
        }),
        Err(e) => Ok(SyncResult {
            success: false,
            message: format!("Failed to sync pattern: {}", e),
            stats: None,
        }),
    }
}

/// Sync a pattern with all its implementations
#[tauri::command]
pub async fn sync_pattern_with_implementations(
    db: State<'_, Db>,
    state_db: State<'_, StateDb>,
    pattern_id: i64,
) -> Result<SyncResult, String> {
    let token = require_auth(&state_db).await?;
    let client = SupabaseClient::new(SUPABASE_URL.to_string(), SUPABASE_ANON_KEY.to_string());
    let sync = CloudSync::new(&db.0, &client, &token);

    match sync.sync_pattern_with_children(pattern_id).await {
        Ok(_) => Ok(SyncResult {
            success: true,
            message: "Pattern and implementations synced successfully".to_string(),
            stats: None,
        }),
        Err(e) => Ok(SyncResult {
            success: false,
            message: format!("Failed to sync pattern: {}", e),
            stats: None,
        }),
    }
}

/// Sync a specific score to the cloud
#[tauri::command]
pub async fn sync_score(
    db: State<'_, Db>,
    state_db: State<'_, StateDb>,
    score_id: i64,
) -> Result<SyncResult, String> {
    let token = require_auth(&state_db).await?;
    let client = SupabaseClient::new(SUPABASE_URL.to_string(), SUPABASE_ANON_KEY.to_string());
    let sync = CloudSync::new(&db.0, &client, &token);

    match sync.sync_score(score_id).await {
        Ok(_) => Ok(SyncResult {
            success: true,
            message: "Score synced successfully".to_string(),
            stats: None,
        }),
        Err(e) => Ok(SyncResult {
            success: false,
            message: format!("Failed to sync score: {}", e),
            stats: None,
        }),
    }
}

/// Sync scores (with their track_scores) for given track+venue pairs.
/// The backend reads track_scores from local DB and syncs them as individual rows.
#[tauri::command]
pub async fn sync_scores(
    db: State<'_, Db>,
    state_db: State<'_, StateDb>,
    entries: Vec<ScoreEntry>,
) -> Result<SyncResult, String> {
    let token = require_auth(&state_db).await?;
    let client = SupabaseClient::new(SUPABASE_URL.to_string(), SUPABASE_ANON_KEY.to_string());
    let sync = CloudSync::new(&db.0, &client, &token);

    let mut synced = 0usize;
    let mut errors = Vec::new();

    for entry in &entries {
        // Find the score container for this (track, venue) pair
        let score_id = match crate::database::local::scores::get_score_id_for_track(
            &db.0,
            entry.track_id,
            entry.venue_id,
        )
        .await
        {
            Ok(Some(id)) => id,
            Ok(None) => continue,
            Err(e) => {
                errors.push(format!("Track {}: {}", entry.track_id, e));
                continue;
            }
        };

        match sync.sync_score(score_id).await {
            Ok(_) => synced += 1,
            Err(e) => errors.push(format!(
                "Score {} (track {}): {}",
                score_id, entry.track_id, e
            )),
        }
    }

    let error_count = errors.len();
    println!(
        "[cloud_sync] Score sync: {} scores synced, {} errors",
        synced, error_count
    );
    for err in &errors {
        println!("[cloud_sync] Score sync ERROR: {}", err);
    }

    Ok(SyncResult {
        success: error_count == 0,
        message: format!("Synced {} scores ({} errors)", synced, error_count),
        stats: None,
    })
}

/// Pull the current user's own patterns from the cloud
#[tauri::command]
pub async fn pull_own_patterns(
    db: State<'_, Db>,
    state_db: State<'_, StateDb>,
) -> Result<SyncResult, String> {
    let token = require_auth(&state_db).await?;
    let uid = auth::get_current_user_id(&state_db.0)
        .await?
        .ok_or_else(|| "Not authenticated".to_string())?;
    let client = SupabaseClient::new(SUPABASE_URL.to_string(), SUPABASE_ANON_KEY.to_string());

    match community_patterns::pull_own_patterns(&db.0, &client, &token, &uid).await {
        Ok(stats) => Ok(SyncResult {
            success: true,
            message: format!("Own patterns: {} added from cloud", stats.added),
            stats: None,
        }),
        Err(e) => Ok(SyncResult {
            success: false,
            message: format!("Failed to pull own patterns: {}", e),
            stats: None,
        }),
    }
}

/// Pull community (published) patterns from the cloud
#[tauri::command]
pub async fn pull_community_patterns(
    db: State<'_, Db>,
    state_db: State<'_, StateDb>,
) -> Result<SyncResult, String> {
    let token = require_auth(&state_db).await?;
    let uid = auth::get_current_user_id(&state_db.0)
        .await?
        .ok_or_else(|| "Not authenticated".to_string())?;
    let client = SupabaseClient::new(SUPABASE_URL.to_string(), SUPABASE_ANON_KEY.to_string());

    match community_patterns::pull_community_patterns(&db.0, &client, &token, &uid).await {
        Ok(stats) => Ok(SyncResult {
            success: true,
            message: format!(
                "Community patterns: {} added, {} updated, {} removed",
                stats.added, stats.updated, stats.removed
            ),
            stats: None,
        }),
        Err(e) => Ok(SyncResult {
            success: false,
            message: format!("Failed to pull community patterns: {}", e),
            stats: None,
        }),
    }
}

/// Pull all data for a venue from the cloud (scores, patterns, tracks, audio, stems, beats, roots).
/// Also refreshes fixtures/groups for joined venues.
#[tauri::command]
pub async fn pull_venue_data(
    app: tauri::AppHandle,
    db: State<'_, Db>,
    state_db: State<'_, StateDb>,
    venue_id: i64,
) -> Result<SyncResult, String> {
    let token = require_auth(&state_db).await?;
    let client = SupabaseClient::new(SUPABASE_URL.to_string(), SUPABASE_ANON_KEY.to_string());

    let venue = crate::database::local::venues::get_venue(&db.0, venue_id).await?;
    let venue_remote_id: i64 = venue
        .remote_id
        .as_ref()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| "Venue has no remote_id".to_string())?;

    // Refresh fixtures if this is a joined venue
    if venue.role == "member" {
        match crate::services::cloud_pull::pull_venue_fixtures(
            &db.0,
            &client,
            venue_remote_id,
            venue_id,
            &token,
        )
        .await
        {
            Ok(n) => println!("[pull_venue_data] Refreshed {} fixtures", n),
            Err(e) => eprintln!("[pull_venue_data] Failed to pull fixtures: {}", e),
        }
    }

    // Pull all scores + dependencies
    match crate::services::cloud_pull::pull_venue_scores(&db.0, &client, &app, venue_id, &token)
        .await
    {
        Ok(stats) => {
            let msg = format!(
                "Pulled {} scores, {} track_scores, {} new tracks, {} patterns, {} audio, {} stems ({} errors)",
                stats.scores,
                stats.track_scores,
                stats.tracks_created,
                stats.patterns_pulled,
                stats.audio_downloaded,
                stats.stems_downloaded,
                stats.errors.len()
            );
            println!("[pull_venue_data] {}", msg);
            if !stats.errors.is_empty() {
                for e in &stats.errors {
                    eprintln!("[pull_venue_data] Error: {}", e);
                }
            }
            Ok(SyncResult {
                success: stats.errors.is_empty(),
                message: msg,
                stats: None,
            })
        }
        Err(e) => Ok(SyncResult {
            success: false,
            message: format!("Failed to pull venue data: {}", e),
            stats: None,
        }),
    }
}
