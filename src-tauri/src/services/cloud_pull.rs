//! Cloud pull service — downloads venue data from Supabase into local SQLite.
//!
//! Used by:
//! - DJs joining a venue (pull fixtures, groups, tags)
//! - Venue owners pulling all scores from all DJs

use serde::Deserialize;
use sqlx::SqlitePool;
use tauri::AppHandle;

use crate::database::local::patterns as local_patterns;
use crate::database::local::venues as local_venues;
use crate::database::remote::common::SupabaseClient;
use crate::database::remote::implementations as remote_implementations;

// ============================================================================
// Remote row types (deserialized from Supabase JSON)
// ============================================================================

#[derive(Deserialize)]
pub struct RemoteFixture {
    pub id: i64,
    pub uid: Option<String>,
    pub venue_id: i64,
    pub universe: i64,
    pub address: i64,
    pub num_channels: i64,
    pub manufacturer: String,
    pub model: String,
    pub mode_name: String,
    pub fixture_path: String,
    pub label: Option<String>,
}

#[derive(Deserialize)]
pub struct RemoteScore {
    pub id: i64,
    pub uid: Option<String>,
    pub track_id: i64,
    pub venue_id: i64,
    pub name: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Deserialize)]
pub struct RemoteTrackScore {
    pub id: i64,
    pub uid: Option<String>,
    pub score_id: i64,
    pub pattern_id: i64,
    pub start_time: f64,
    pub end_time: f64,
    pub z_index: i64,
    pub blend_mode: String,
    pub args_json: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Deserialize)]
pub struct RemoteTrack {
    pub id: i64,
    pub uid: Option<String>,
    pub track_hash: String,
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub track_number: Option<i64>,
    pub disc_number: Option<i64>,
    pub duration_seconds: Option<f64>,
    pub storage_path: Option<String>,
    pub album_art_path: Option<String>,
    pub album_art_mime: Option<String>,
    pub source_type: Option<String>,
    pub source_id: Option<String>,
    pub source_filename: Option<String>,
}

#[derive(Deserialize)]
pub struct RemoteTrackBeats {
    pub id: i64,
    pub track_id: i64,
    pub bpm: Option<f64>,
    pub beats_json: Option<String>,
    pub downbeats_json: Option<String>,
    pub downbeat_offset: Option<f64>,
}

#[derive(Deserialize)]
pub struct RemoteTrackRoots {
    pub id: i64,
    pub track_id: i64,
    pub sections_json: String,
}

#[derive(Deserialize)]
pub struct RemoteTrackStem {
    pub id: i64,
    pub track_id: i64,
    pub stem_name: String,
    pub storage_path: Option<String>,
}

// ============================================================================
// Pull stats
// ============================================================================

#[derive(Debug, Default, serde::Serialize)]
pub struct VenuePullStats {
    pub fixtures: usize,
    pub scores: usize,
    pub track_scores: usize,
    pub tracks_created: usize,
    pub patterns_pulled: usize,
    pub audio_downloaded: usize,
    pub stems_downloaded: usize,
    pub errors: Vec<String>,
}

// ============================================================================
// Pull venue fixtures (for join)
// ============================================================================

/// Pull all fixtures for a venue from Supabase into local DB.
pub async fn pull_venue_fixtures(
    pool: &SqlitePool,
    client: &SupabaseClient,
    venue_remote_id: i64,
    local_venue_id: i64,
    access_token: &str,
) -> Result<usize, String> {
    let fixtures: Vec<RemoteFixture> = client
        .select(
            "fixtures",
            &format!("venue_id=eq.{}&select=id,uid,venue_id,universe,address,num_channels,manufacturer,model,mode_name,fixture_path,label", venue_remote_id),
            access_token,
        )
        .await
        .map_err(|e| format!("Failed to fetch venue fixtures: {}", e))?;

    let mut count = 0;
    for f in &fixtures {
        let remote_id_str = f.id.to_string();
        // Use UUID for local fixture ID (same as normal patching)
        let local_id = uuid::Uuid::new_v4().to_string();

        sqlx::query(
            "INSERT INTO fixtures (id, remote_id, uid, venue_id, universe, address, num_channels, manufacturer, model, mode_name, fixture_path, label, pos_x, pos_y, pos_z, rot_x, rot_y, rot_z)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, 0, 0, 0, 0, 0)
             ON CONFLICT(remote_id) DO UPDATE SET
               manufacturer = excluded.manufacturer,
               model = excluded.model,
               mode_name = excluded.mode_name,
               fixture_path = excluded.fixture_path,
               label = excluded.label",
        )
        .bind(&local_id)
        .bind(&remote_id_str)
        .bind(&f.uid)
        .bind(local_venue_id)
        .bind(f.universe)
        .bind(f.address)
        .bind(f.num_channels)
        .bind(&f.manufacturer)
        .bind(&f.model)
        .bind(&f.mode_name)
        .bind(&f.fixture_path)
        .bind(&f.label)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to insert fixture: {}", e))?;

        count += 1;
    }

    Ok(count)
}

// ============================================================================
// Pull venue groups (for join)
// ============================================================================

/// Pull all fixture groups and their members for a venue from Supabase.
pub async fn pull_venue_groups(
    pool: &SqlitePool,
    client: &SupabaseClient,
    venue_remote_id: i64,
    local_venue_id: i64,
    access_token: &str,
) -> Result<usize, String> {
    #[derive(Deserialize)]
    struct RemoteGroup {
        id: i64,
        uid: Option<String>,
        name: Option<String>,
        axis_lr: Option<f64>,
        axis_fb: Option<f64>,
        axis_ab: Option<f64>,
        movement_config: Option<String>,
        display_order: i64,
    }

    #[derive(Deserialize)]
    struct RemoteGroupMember {
        fixture_id: i64,
        group_id: i64,
        display_order: i64,
    }

    let groups: Vec<RemoteGroup> = client
        .select(
            "fixture_groups",
            &format!(
                "venue_id=eq.{}&select=id,uid,name,axis_lr,axis_fb,axis_ab,movement_config,display_order",
                venue_remote_id
            ),
            access_token,
        )
        .await
        .map_err(|e| format!("Failed to fetch venue groups: {}", e))?;

    let mut count = 0;
    for g in &groups {
        let remote_id_str = g.id.to_string();

        // Upsert group locally
        let local_group_id: i64 = sqlx::query_scalar(
            "INSERT INTO fixture_groups (remote_id, uid, venue_id, name, axis_lr, axis_fb, axis_ab, movement_config, display_order)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(remote_id) DO UPDATE SET
               name = excluded.name,
               axis_lr = excluded.axis_lr,
               axis_fb = excluded.axis_fb,
               axis_ab = excluded.axis_ab,
               movement_config = excluded.movement_config,
               display_order = excluded.display_order
             RETURNING id",
        )
        .bind(&remote_id_str)
        .bind(&g.uid)
        .bind(local_venue_id)
        .bind(&g.name)
        .bind(g.axis_lr)
        .bind(g.axis_fb)
        .bind(g.axis_ab)
        .bind(&g.movement_config)
        .bind(g.display_order)
        .fetch_one(pool)
        .await
        .map_err(|e| format!("Failed to upsert group: {}", e))?;

        // Fetch group members
        let members: Vec<RemoteGroupMember> = client
            .select(
                "fixture_group_members",
                &format!(
                    "group_id=eq.{}&select=fixture_id,group_id,display_order",
                    g.id
                ),
                access_token,
            )
            .await
            .map_err(|e| format!("Failed to fetch group members: {}", e))?;

        // Map fixture remote_ids to local IDs and insert memberships
        for m in &members {
            let fixture_remote_id_str = m.fixture_id.to_string();
            // Find local fixture by remote_id
            let local_fixture_id: Option<String> =
                sqlx::query_scalar("SELECT id FROM fixtures WHERE remote_id = ? AND venue_id = ?")
                    .bind(&fixture_remote_id_str)
                    .bind(local_venue_id)
                    .fetch_optional(pool)
                    .await
                    .map_err(|e| format!("Failed to find fixture: {}", e))?;

            if let Some(fid) = local_fixture_id {
                sqlx::query(
                    "INSERT INTO fixture_group_members (fixture_id, group_id, display_order)
                     VALUES (?, ?, ?)
                     ON CONFLICT(fixture_id, group_id) DO UPDATE SET display_order = excluded.display_order",
                )
                .bind(&fid)
                .bind(local_group_id)
                .bind(m.display_order)
                .execute(pool)
                .await
                .map_err(|e| format!("Failed to insert group member: {}", e))?;
            }
        }

        count += 1;
    }

    Ok(count)
}

// ============================================================================
// Pull all scores for a venue (for owner)
// ============================================================================

/// Pull all scores for a venue from all DJs, including their dependent data.
pub async fn pull_venue_scores(
    pool: &SqlitePool,
    client: &SupabaseClient,
    app_handle: &AppHandle,
    local_venue_id: i64,
    access_token: &str,
) -> Result<VenuePullStats, String> {
    let mut stats = VenuePullStats::default();

    let venue = local_venues::get_venue(pool, local_venue_id).await?;
    let venue_remote_id: i64 = venue
        .remote_id
        .as_ref()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| "Venue has no remote_id — sync it first".to_string())?;

    // 1. Fetch all scores for this venue from all DJs
    let remote_scores: Vec<RemoteScore> = client
        .select(
            "scores",
            &format!(
                "venue_id=eq.{}&select=id,uid,track_id,venue_id,name,created_at,updated_at",
                venue_remote_id
            ),
            access_token,
        )
        .await
        .map_err(|e| format!("Failed to fetch venue scores: {}", e))?;

    for remote_score in &remote_scores {
        // 2. Ensure the track exists locally
        let local_track_id = match ensure_track_local(
            pool,
            client,
            app_handle,
            remote_score.track_id,
            access_token,
            &mut stats,
        )
        .await
        {
            Ok(id) => id,
            Err(e) => {
                stats
                    .errors
                    .push(format!("Track {}: {}", remote_score.track_id, e));
                continue;
            }
        };

        // 3. Upsert the score locally
        let score_remote_id_str = remote_score.id.to_string();
        let local_score_id: i64 = sqlx::query_scalar(
            "INSERT INTO scores (remote_id, uid, track_id, venue_id, name, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(remote_id) DO UPDATE SET
               name = excluded.name,
               updated_at = excluded.updated_at
             RETURNING id",
        )
        .bind(&score_remote_id_str)
        .bind(&remote_score.uid)
        .bind(local_track_id)
        .bind(local_venue_id)
        .bind(&remote_score.name)
        .bind(&remote_score.created_at)
        .bind(&remote_score.updated_at)
        .fetch_one(pool)
        .await
        .map_err(|e| format!("Failed to upsert score: {}", e))?;

        stats.scores += 1;

        // 4. Fetch and upsert track_scores for this score
        let remote_track_scores: Vec<RemoteTrackScore> = client
            .select(
                "track_scores",
                &format!(
                    "score_id=eq.{}&select=id,uid,score_id,pattern_id,start_time,end_time,z_index,blend_mode,args_json,created_at,updated_at",
                    remote_score.id
                ),
                access_token,
            )
            .await
            .map_err(|e| format!("Failed to fetch track_scores: {}", e))?;

        // Delete existing track_scores for this score, then re-insert (same strategy as push sync)
        sqlx::query("DELETE FROM track_scores WHERE score_id = ?")
            .bind(local_score_id)
            .execute(pool)
            .await
            .map_err(|e| format!("Failed to delete old track_scores: {}", e))?;

        for rts in &remote_track_scores {
            // 5. Ensure the pattern + implementation exist locally
            let local_pattern_id =
                match ensure_pattern_local(pool, client, rts.pattern_id, access_token, &mut stats)
                    .await
                {
                    Ok(id) => id,
                    Err(e) => {
                        stats
                            .errors
                            .push(format!("Pattern {}: {}", rts.pattern_id, e));
                        continue;
                    }
                };

            let ts_remote_id_str = rts.id.to_string();
            sqlx::query(
                "INSERT INTO track_scores (remote_id, uid, score_id, pattern_id, start_time, end_time, z_index, blend_mode, args_json, created_at, updated_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&ts_remote_id_str)
            .bind(&rts.uid)
            .bind(local_score_id)
            .bind(local_pattern_id)
            .bind(rts.start_time)
            .bind(rts.end_time)
            .bind(rts.z_index)
            .bind(&rts.blend_mode)
            .bind(&rts.args_json)
            .bind(&rts.created_at)
            .bind(&rts.updated_at)
            .execute(pool)
            .await
            .map_err(|e| format!("Failed to insert track_score: {}", e))?;

            stats.track_scores += 1;
        }
    }

    Ok(stats)
}

// ============================================================================
// Helpers: ensure track/pattern exist locally
// ============================================================================

/// Ensure a track exists in local DB (by remote_id). If not, fetch metadata from cloud
/// and create a stub row. Returns the local track ID.
async fn ensure_track_local(
    pool: &SqlitePool,
    client: &SupabaseClient,
    app_handle: &AppHandle,
    track_remote_id: i64,
    access_token: &str,
    stats: &mut VenuePullStats,
) -> Result<i64, String> {
    let remote_id_str = track_remote_id.to_string();

    // Check if already exists locally
    let existing: Option<i64> = sqlx::query_scalar("SELECT id FROM tracks WHERE remote_id = ?")
        .bind(&remote_id_str)
        .fetch_optional(pool)
        .await
        .map_err(|e| format!("DB error: {}", e))?;

    if let Some(local_id) = existing {
        return Ok(local_id);
    }

    // Fetch from cloud
    let rows: Vec<RemoteTrack> = client
        .select(
            "tracks",
            &format!(
                "id=eq.{}&select=id,uid,track_hash,title,artist,album,track_number,disc_number,duration_seconds,storage_path,album_art_path,album_art_mime,source_type,source_id,source_filename",
                track_remote_id
            ),
            access_token,
        )
        .await
        .map_err(|e| format!("Failed to fetch track: {}", e))?;

    let track = rows
        .into_iter()
        .next()
        .ok_or_else(|| format!("Track {} not found in cloud", track_remote_id))?;

    // Create local stub — file_path is empty until audio is downloaded
    let storage_dir = crate::services::tracks::storage_dirs(app_handle)?;
    let file_path = storage_dir
        .0
        .join(format!("{}.stub", track.track_hash))
        .to_string_lossy()
        .to_string();

    let local_id: i64 = sqlx::query_scalar(
        "INSERT INTO tracks (remote_id, uid, track_hash, title, artist, album, track_number, disc_number, duration_seconds, file_path, storage_path, album_art_path, album_art_mime, source_type, source_id, source_filename)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(remote_id) DO UPDATE SET title = excluded.title
         RETURNING id",
    )
    .bind(&remote_id_str)
    .bind(&track.uid)
    .bind(&track.track_hash)
    .bind(&track.title)
    .bind(&track.artist)
    .bind(&track.album)
    .bind(track.track_number)
    .bind(track.disc_number)
    .bind(track.duration_seconds)
    .bind(&file_path)
    .bind(&track.storage_path)
    .bind(&track.album_art_path)
    .bind(&track.album_art_mime)
    .bind(&track.source_type)
    .bind(&track.source_id)
    .bind(&track.source_filename)
    .fetch_one(pool)
    .await
    .map_err(|e| format!("Failed to insert stub track: {}", e))?;

    stats.tracks_created += 1;

    // Download audio if storage_path exists
    if let Some(ref spath) = track.storage_path {
        match download_track_audio(
            pool,
            client,
            app_handle,
            local_id,
            &track.track_hash,
            spath,
            access_token,
        )
        .await
        {
            Ok(_) => stats.audio_downloaded += 1,
            Err(e) => stats
                .errors
                .push(format!("Audio download {}: {}", track_remote_id, e)),
        }
    }

    // Download stems
    match download_track_stems(
        pool,
        client,
        app_handle,
        track_remote_id,
        local_id,
        &track.track_hash,
        access_token,
    )
    .await
    {
        Ok(n) => stats.stems_downloaded += n,
        Err(e) => stats
            .errors
            .push(format!("Stems download {}: {}", track_remote_id, e)),
    }

    // Pull beats
    if let Err(e) = pull_track_beats(pool, client, track_remote_id, local_id, access_token).await {
        stats
            .errors
            .push(format!("Beats {}: {}", track_remote_id, e));
    }

    // Pull roots
    if let Err(e) = pull_track_roots(pool, client, track_remote_id, local_id, access_token).await {
        stats
            .errors
            .push(format!("Roots {}: {}", track_remote_id, e));
    }

    Ok(local_id)
}

/// Ensure a pattern + implementation exist locally (by remote_id).
async fn ensure_pattern_local(
    pool: &SqlitePool,
    client: &SupabaseClient,
    pattern_remote_id: i64,
    access_token: &str,
    stats: &mut VenuePullStats,
) -> Result<i64, String> {
    let remote_id_str = pattern_remote_id.to_string();

    // Check if already exists locally
    let existing: Option<i64> = sqlx::query_scalar("SELECT id FROM patterns WHERE remote_id = ?")
        .bind(&remote_id_str)
        .fetch_optional(pool)
        .await
        .map_err(|e| format!("DB error: {}", e))?;

    if let Some(local_id) = existing {
        // Still update the implementation in case it changed
        if let Ok(Some(impl_row)) = remote_implementations::fetch_implementation_by_pattern(
            client,
            pattern_remote_id,
            access_token,
        )
        .await
        {
            let impl_remote_id_str = impl_row.id.to_string();
            let _ = local_patterns::upsert_community_implementation(
                pool,
                &impl_remote_id_str,
                &impl_row.uid,
                local_id,
                impl_row.name.as_deref(),
                &impl_row.graph_json,
            )
            .await;
        }
        return Ok(local_id);
    }

    // Fetch pattern metadata from cloud
    let rows: Vec<crate::database::remote::patterns::RemotePatternRow> = client
        .select(
            "patterns",
            &format!(
                "id=eq.{}&select=id,uid,name,description,is_published,author_name,created_at,updated_at",
                pattern_remote_id
            ),
            access_token,
        )
        .await
        .map_err(|e| format!("Failed to fetch pattern: {}", e))?;

    let pat = rows
        .into_iter()
        .next()
        .ok_or_else(|| format!("Pattern {} not found in cloud", pattern_remote_id))?;

    let local_id = local_patterns::upsert_community_pattern(
        pool,
        &remote_id_str,
        &pat.uid,
        &pat.name,
        pat.description.as_deref(),
        pat.author_name.as_deref(),
        pat.is_published,
        &pat.created_at,
        &pat.updated_at,
    )
    .await?;

    stats.patterns_pulled += 1;

    // Fetch and upsert implementation
    match remote_implementations::fetch_implementation_by_pattern(
        client,
        pattern_remote_id,
        access_token,
    )
    .await
    {
        Ok(Some(impl_row)) => {
            let impl_remote_id_str = impl_row.id.to_string();
            local_patterns::upsert_community_implementation(
                pool,
                &impl_remote_id_str,
                &impl_row.uid,
                local_id,
                impl_row.name.as_deref(),
                &impl_row.graph_json,
            )
            .await?;
        }
        Ok(None) => {}
        Err(e) => {
            stats.errors.push(format!(
                "Implementation for pattern {}: {}",
                pattern_remote_id, e
            ));
        }
    }

    Ok(local_id)
}

// ============================================================================
// Audio/stems download helpers
// ============================================================================

/// Download a track's audio file from Supabase storage to local filesystem.
async fn download_track_audio(
    pool: &SqlitePool,
    client: &SupabaseClient,
    app_handle: &AppHandle,
    local_track_id: i64,
    track_hash: &str,
    storage_path: &str,
    access_token: &str,
) -> Result<(), String> {
    // Parse bucket and path from storage_path (format: "track-audio/uid/hash/audio.ext")
    let (bucket, path) = storage_path
        .split_once('/')
        .ok_or_else(|| format!("Invalid storage_path: {}", storage_path))?;

    let bytes = client
        .download_file(bucket, path, access_token)
        .await
        .map_err(|e| format!("Download failed: {}", e))?;

    // Determine extension from path
    let ext = path.rsplit('.').next().unwrap_or("bin");
    let storage_dir = crate::services::tracks::storage_dirs(app_handle)?;
    let dest = storage_dir.0.join(format!("{}.{}", track_hash, ext));

    std::fs::write(&dest, &bytes).map_err(|e| format!("Failed to write audio: {}", e))?;

    // Update local file_path
    sqlx::query("UPDATE tracks SET file_path = ? WHERE id = ?")
        .bind(dest.to_string_lossy().as_ref())
        .bind(local_track_id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to update file_path: {}", e))?;

    Ok(())
}

/// Download stems for a track from Supabase storage.
async fn download_track_stems(
    pool: &SqlitePool,
    client: &SupabaseClient,
    app_handle: &AppHandle,
    track_remote_id: i64,
    local_track_id: i64,
    track_hash: &str,
    access_token: &str,
) -> Result<usize, String> {
    let remote_stems: Vec<RemoteTrackStem> = client
        .select(
            "track_stems",
            &format!(
                "track_id=eq.{}&select=id,track_id,stem_name,storage_path",
                track_remote_id
            ),
            access_token,
        )
        .await
        .map_err(|e| format!("Failed to fetch stems: {}", e))?;

    let storage_dir = crate::services::tracks::storage_dirs(app_handle)?;
    let stems_dir = storage_dir.2.join(track_hash);
    std::fs::create_dir_all(&stems_dir)
        .map_err(|e| format!("Failed to create stems dir: {}", e))?;

    let mut count = 0;
    for stem in &remote_stems {
        let Some(ref spath) = stem.storage_path else {
            continue;
        };

        let (bucket, path) = match spath.split_once('/') {
            Some(bp) => bp,
            None => continue,
        };

        let bytes = match client.download_file(bucket, path, access_token).await {
            Ok(b) => b,
            Err(e) => {
                eprintln!("[pull] Failed to download stem {}: {}", stem.stem_name, e);
                continue;
            }
        };

        let ext = path.rsplit('.').next().unwrap_or("wav");
        let dest = stems_dir.join(format!("{}.{}", stem.stem_name, ext));
        std::fs::write(&dest, &bytes).map_err(|e| format!("Failed to write stem: {}", e))?;

        let remote_id_str = stem.id.to_string();
        sqlx::query(
            "INSERT INTO track_stems (track_id, remote_id, uid, stem_name, file_path, storage_path)
             VALUES (?, ?, (SELECT uid FROM tracks WHERE id = ?), ?, ?, ?)
             ON CONFLICT(track_id, stem_name) DO UPDATE SET
               file_path = excluded.file_path,
               storage_path = excluded.storage_path",
        )
        .bind(local_track_id)
        .bind(&remote_id_str)
        .bind(local_track_id)
        .bind(&stem.stem_name)
        .bind(dest.to_string_lossy().as_ref())
        .bind(spath)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to upsert stem: {}", e))?;

        count += 1;
    }

    Ok(count)
}

// ============================================================================
// Beats/roots pull helpers
// ============================================================================

async fn pull_track_beats(
    pool: &SqlitePool,
    client: &SupabaseClient,
    track_remote_id: i64,
    local_track_id: i64,
    access_token: &str,
) -> Result<(), String> {
    let rows: Vec<RemoteTrackBeats> = client
        .select(
            "track_beats",
            &format!(
                "track_id=eq.{}&select=id,track_id,bpm,beats_json,downbeats_json,downbeat_offset",
                track_remote_id
            ),
            access_token,
        )
        .await
        .map_err(|e| format!("Failed to fetch beats: {}", e))?;

    let Some(beats) = rows.into_iter().next() else {
        return Ok(());
    };

    let remote_id_str = beats.id.to_string();
    sqlx::query(
        "INSERT INTO track_beats (track_id, remote_id, uid, bpm, beats_json, downbeats_json, downbeat_offset)
         VALUES (?, ?, (SELECT uid FROM tracks WHERE id = ?), ?, ?, ?, ?)
         ON CONFLICT(track_id) DO UPDATE SET
           bpm = excluded.bpm,
           beats_json = excluded.beats_json,
           downbeats_json = excluded.downbeats_json,
           downbeat_offset = excluded.downbeat_offset",
    )
    .bind(local_track_id)
    .bind(&remote_id_str)
    .bind(local_track_id)
    .bind(beats.bpm)
    .bind(&beats.beats_json)
    .bind(&beats.downbeats_json)
    .bind(beats.downbeat_offset)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to upsert beats: {}", e))?;

    Ok(())
}

async fn pull_track_roots(
    pool: &SqlitePool,
    client: &SupabaseClient,
    track_remote_id: i64,
    local_track_id: i64,
    access_token: &str,
) -> Result<(), String> {
    let rows: Vec<RemoteTrackRoots> = client
        .select(
            "track_roots",
            &format!(
                "track_id=eq.{}&select=id,track_id,sections_json",
                track_remote_id
            ),
            access_token,
        )
        .await
        .map_err(|e| format!("Failed to fetch roots: {}", e))?;

    let Some(roots) = rows.into_iter().next() else {
        return Ok(());
    };

    let remote_id_str = roots.id.to_string();
    sqlx::query(
        "INSERT INTO track_roots (track_id, remote_id, uid, sections_json)
         VALUES (?, ?, (SELECT uid FROM tracks WHERE id = ?), ?)
         ON CONFLICT(track_id) DO UPDATE SET
           sections_json = excluded.sections_json",
    )
    .bind(local_track_id)
    .bind(&remote_id_str)
    .bind(local_track_id)
    .bind(&roots.sections_json)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to upsert roots: {}", e))?;

    Ok(())
}
