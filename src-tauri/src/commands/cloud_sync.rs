//! Tauri commands for cloud sync operations

use serde::{Deserialize, Serialize};
use tauri::State;
use ts_rs::TS;

use crate::config::{SUPABASE_ANON_KEY, SUPABASE_URL};
use crate::database::local::auth;
use crate::database::local::state::StateDb;
use crate::database::remote::common::SupabaseClient;
use crate::database::Db;
use crate::services::cloud_sync::{CloudSync, SyncStats};
use crate::services::community_patterns;
use crate::services::file_sync;

/// Entry identifying a score to sync (by track + venue)
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoreEntry {
    pub track_id: String,
    pub venue_id: String,
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

/// Helper to get access token and user ID, or return error
async fn require_auth(state_db: &StateDb) -> Result<(String, String), String> {
    let token = auth::get_current_access_token(&state_db.0)
        .await?
        .ok_or_else(|| "Not authenticated - please sign in first".to_string())?;
    let uid = auth::get_current_user_id(&state_db.0)
        .await?
        .ok_or_else(|| "Not authenticated - please sign in first".to_string())?;
    Ok((token, uid))
}

/// Sync all local data to the cloud
#[tauri::command]
pub async fn sync_all(
    db: State<'_, Db>,
    state_db: State<'_, StateDb>,
) -> Result<SyncResult, String> {
    let (token, uid) = require_auth(&state_db).await?;
    let client = SupabaseClient::new(SUPABASE_URL.to_string(), SUPABASE_ANON_KEY.to_string());
    let sync = CloudSync::new(&db.0, &client, &token, &uid);

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
    venue_id: String,
) -> Result<SyncResult, String> {
    let (token, uid) = require_auth(&state_db).await?;
    let client = SupabaseClient::new(SUPABASE_URL.to_string(), SUPABASE_ANON_KEY.to_string());
    let sync = CloudSync::new(&db.0, &client, &token, &uid);

    match sync.sync_venue(&venue_id).await {
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
    venue_id: String,
) -> Result<SyncResult, String> {
    let (token, uid) = require_auth(&state_db).await?;
    let client = SupabaseClient::new(SUPABASE_URL.to_string(), SUPABASE_ANON_KEY.to_string());
    let sync = CloudSync::new(&db.0, &client, &token, &uid);

    match sync.sync_venue_with_children(&venue_id).await {
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
    track_id: String,
) -> Result<SyncResult, String> {
    let (token, uid) = require_auth(&state_db).await?;
    let client = SupabaseClient::new(SUPABASE_URL.to_string(), SUPABASE_ANON_KEY.to_string());
    let sync = CloudSync::new(&db.0, &client, &token, &uid);

    match sync.sync_track(&track_id).await {
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
    track_id: String,
) -> Result<SyncResult, String> {
    let (token, uid) = require_auth(&state_db).await?;
    let client = SupabaseClient::new(SUPABASE_URL.to_string(), SUPABASE_ANON_KEY.to_string());
    let sync = CloudSync::new(&db.0, &client, &token, &uid);

    match sync.sync_track_with_children(&track_id).await {
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
    pattern_id: String,
) -> Result<SyncResult, String> {
    let (token, uid) = require_auth(&state_db).await?;
    let client = SupabaseClient::new(SUPABASE_URL.to_string(), SUPABASE_ANON_KEY.to_string());
    let sync = CloudSync::new(&db.0, &client, &token, &uid);

    match sync.sync_pattern(&pattern_id).await {
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
    pattern_id: String,
) -> Result<SyncResult, String> {
    let (token, uid) = require_auth(&state_db).await?;
    let client = SupabaseClient::new(SUPABASE_URL.to_string(), SUPABASE_ANON_KEY.to_string());
    let sync = CloudSync::new(&db.0, &client, &token, &uid);

    match sync.sync_pattern_with_children(&pattern_id).await {
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
    score_id: String,
) -> Result<SyncResult, String> {
    let (token, uid) = require_auth(&state_db).await?;
    let client = SupabaseClient::new(SUPABASE_URL.to_string(), SUPABASE_ANON_KEY.to_string());
    let sync = CloudSync::new(&db.0, &client, &token, &uid);

    match sync.sync_score(&score_id).await {
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
    let (token, uid) = require_auth(&state_db).await?;
    let client = SupabaseClient::new(SUPABASE_URL.to_string(), SUPABASE_ANON_KEY.to_string());
    let sync = CloudSync::new(&db.0, &client, &token, &uid);

    let mut synced = 0usize;
    let mut errors = Vec::new();

    for entry in &entries {
        // Find the score container for this (track, venue) pair
        let score_id = match crate::database::local::scores::get_score_id_for_track(
            &db.0,
            &entry.track_id,
            &entry.venue_id,
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

        match sync.sync_score(&score_id).await {
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
    let (token, uid) = require_auth(&state_db).await?;
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
    let (token, uid) = require_auth(&state_db).await?;
    let client = SupabaseClient::new(SUPABASE_URL.to_string(), SUPABASE_ANON_KEY.to_string());

    match community_patterns::pull_community_patterns(&db.0, &client, &token, &uid).await {
        Ok(stats) => Ok(SyncResult {
            success: true,
            message: format!(
                "Community patterns: {} added, {} updated",
                stats.added, stats.updated
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
    venue_id: String,
) -> Result<SyncResult, String> {
    let (token, _uid) = require_auth(&state_db).await?;
    let client = SupabaseClient::new(SUPABASE_URL.to_string(), SUPABASE_ANON_KEY.to_string());

    let venue = crate::database::local::venues::get_venue(&db.0, &venue_id).await?;

    // Refresh fixtures and groups if this is a joined venue
    if venue.is_member() {
        // With UUID PKs, the venue's local id IS the cloud id
        match crate::services::cloud_pull::pull_venue_fixtures(&db.0, &client, &venue_id, &token)
            .await
        {
            Ok(n) => println!("[pull_venue_data] Refreshed {} fixtures", n),
            Err(e) => eprintln!("[pull_venue_data] Failed to pull fixtures: {}", e),
        }

        match crate::services::cloud_pull::pull_venue_groups(&db.0, &client, &venue_id, &token)
            .await
        {
            Ok(n) => println!("[pull_venue_data] Refreshed {} groups", n),
            Err(e) => eprintln!("[pull_venue_data] Failed to pull groups: {}", e),
        }
    }

    // Pull all scores + dependencies
    match crate::services::cloud_pull::pull_venue_scores(&db.0, &client, &app, &venue_id, &token)
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

/// Sync files (audio + stems) to/from Supabase storage.
/// This runs independently from record sync — uploads pending local files
/// and downloads files for stub tracks.
#[tauri::command]
pub async fn sync_files(
    app: tauri::AppHandle,
    db: State<'_, Db>,
    state_db: State<'_, StateDb>,
) -> Result<SyncResult, String> {
    let (token, uid) = require_auth(&state_db).await?;
    let client = SupabaseClient::new(SUPABASE_URL.to_string(), SUPABASE_ANON_KEY.to_string());

    let mut errors: Vec<String> = Vec::new();
    let mut audio_uploaded = 0usize;
    let mut stems_uploaded = 0usize;
    let mut audio_downloaded = 0usize;
    let mut stems_downloaded = 0usize;

    // Upload pending audio
    match file_sync::upload_pending_audio(&db.0, &client, &uid, &token).await {
        Ok(stats) => {
            audio_uploaded = stats.audio_uploaded;
            errors.extend(stats.errors);
        }
        Err(e) => errors.push(format!("Audio upload: {}", e)),
    }

    // Upload pending stems
    match file_sync::upload_pending_stems(&db.0, &client, &uid, &token).await {
        Ok(n) => stems_uploaded = n,
        Err(e) => errors.push(format!("Stem upload: {}", e)),
    }

    // Download pending audio
    match file_sync::download_pending_audio(&db.0, &client, &app, &token).await {
        Ok(n) => audio_downloaded = n,
        Err(e) => errors.push(format!("Audio download: {}", e)),
    }

    // Download pending stems
    match file_sync::download_pending_stems(&db.0, &client, &app, &token).await {
        Ok(n) => stems_downloaded = n,
        Err(e) => errors.push(format!("Stem download: {}", e)),
    }

    let total = audio_uploaded + stems_uploaded + audio_downloaded + stems_downloaded;
    let error_count = errors.len();
    println!(
        "[file_sync] {} files synced ({} errors). audio_up={} stems_up={} audio_down={} stems_down={}",
        total, error_count, audio_uploaded, stems_uploaded, audio_downloaded, stems_downloaded
    );
    for err in &errors {
        println!("[file_sync] ERROR: {}", err);
    }

    Ok(SyncResult {
        success: error_count == 0,
        message: format!(
            "File sync: {} uploaded, {} downloaded ({} errors)",
            audio_uploaded + stems_uploaded,
            audio_downloaded + stems_downloaded,
            error_count
        ),
        stats: None,
    })
}

/// Look up display names for a list of user IDs from the profiles table.
/// Returns a map of uid -> display_name.
#[tauri::command]
pub async fn get_display_names(
    state_db: State<'_, StateDb>,
    uids: Vec<String>,
) -> Result<std::collections::HashMap<String, String>, String> {
    if uids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }

    let (token, _) = require_auth(&state_db).await?;
    let client = SupabaseClient::new(SUPABASE_URL.to_string(), SUPABASE_ANON_KEY.to_string());

    // Build PostgREST filter: id=in.(uid1,uid2,...)
    let ids_csv = uids.join(",");
    let query = format!("id=in.({})", ids_csv);

    #[derive(serde::Deserialize)]
    struct ProfileRow {
        id: String,
        display_name: Option<String>,
    }

    let rows: Vec<ProfileRow> = client
        .select(
            "profiles",
            &format!("{}&select=id,display_name", query),
            &token,
        )
        .await
        .map_err(|e| format!("Failed to fetch profiles: {:?}", e))?;

    let mut map = std::collections::HashMap::new();
    for row in rows {
        if let Some(name) = row.display_name {
            map.insert(row.id, name);
        }
    }
    Ok(map)
}
