//! Cloud pull service -- downloads venue data from Supabase into local SQLite.
//!
//! Used by:
//! - DJs joining a venue (pull fixtures, groups, tags)
//! - Venue owners pulling all scores from all DJs

use serde::Deserialize;
use sqlx::SqlitePool;
use tauri::AppHandle;

use crate::database::local::patterns as local_patterns;
use crate::database::remote::common::SupabaseClient;
use crate::database::remote::queries as remote_queries;

// ============================================================================
// Remote row types (deserialized from Supabase JSON)
// ============================================================================

#[derive(Deserialize)]
pub struct RemoteFixture {
    pub id: String,
    pub uid: Option<String>,
    pub venue_id: String,
    pub universe: i64,
    pub address: i64,
    pub num_channels: i64,
    pub manufacturer: String,
    pub model: String,
    pub mode_name: String,
    pub fixture_path: String,
    pub label: Option<String>,
    pub pos_x: Option<f64>,
    pub pos_y: Option<f64>,
    pub pos_z: Option<f64>,
    pub rot_x: Option<f64>,
    pub rot_y: Option<f64>,
    pub rot_z: Option<f64>,
}

#[derive(Deserialize)]
pub struct RemoteScore {
    pub id: String,
    pub uid: Option<String>,
    pub track_id: String,
    pub venue_id: String,
    pub name: Option<String>,
    pub deleted_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Deserialize)]
pub struct RemoteTrackScore {
    pub id: String,
    pub uid: Option<String>,
    pub score_id: String,
    pub pattern_id: String,
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
    pub id: String,
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
}

#[derive(Deserialize)]
pub struct RemoteTrackBeats {
    pub id: String,
    pub track_id: String,
    pub bpm: Option<f64>,
    pub beats_json: Option<String>,
    pub downbeats_json: Option<String>,
    pub downbeat_offset: Option<f64>,
}

#[derive(Deserialize)]
pub struct RemoteTrackRoots {
    pub id: String,
    pub track_id: String,
    pub sections_json: String,
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
/// With UUID PKs, the venue_id IS the cloud ID.
pub async fn pull_venue_fixtures(
    pool: &SqlitePool,
    client: &SupabaseClient,
    venue_id: &str,
    access_token: &str,
) -> Result<usize, String> {
    let fixtures: Vec<RemoteFixture> = client
        .select(
            "fixtures",
            &format!("venue_id=eq.{}&select=id,uid,venue_id,universe,address,num_channels,manufacturer,model,mode_name,fixture_path,label,pos_x,pos_y,pos_z,rot_x,rot_y,rot_z", venue_id),
            access_token,
        )
        .await
        .map_err(|e| format!("Failed to fetch venue fixtures: {}", e))?;

    let remote_ids: Vec<&str> = fixtures.iter().map(|f| f.id.as_str()).collect();

    let mut count = 0;
    for f in &fixtures {
        let px = f.pos_x.unwrap_or(0.0);
        let py = f.pos_y.unwrap_or(0.0);
        let pz = f.pos_z.unwrap_or(0.0);
        let rx = f.rot_x.unwrap_or(0.0);
        let ry = f.rot_y.unwrap_or(0.0);
        let rz = f.rot_z.unwrap_or(0.0);

        // Upsert by id (the UUID is shared between local and cloud)
        sqlx::query(
            "INSERT INTO fixtures (id, uid, venue_id, universe, address, num_channels, manufacturer, model, mode_name, fixture_path, label, pos_x, pos_y, pos_z, rot_x, rot_y, rot_z)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET
               manufacturer = excluded.manufacturer,
               model = excluded.model,
               mode_name = excluded.mode_name,
               fixture_path = excluded.fixture_path,
               label = excluded.label,
               pos_x = excluded.pos_x,
               pos_y = excluded.pos_y,
               pos_z = excluded.pos_z,
               rot_x = excluded.rot_x,
               rot_y = excluded.rot_y,
               rot_z = excluded.rot_z",
        )
        .bind(&f.id)
        .bind(&f.uid)
        .bind(venue_id)
        .bind(f.universe)
        .bind(f.address)
        .bind(f.num_channels)
        .bind(&f.manufacturer)
        .bind(&f.model)
        .bind(&f.mode_name)
        .bind(&f.fixture_path)
        .bind(&f.label)
        .bind(px)
        .bind(py)
        .bind(pz)
        .bind(rx)
        .bind(ry)
        .bind(rz)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to upsert fixture: {}", e))?;

        count += 1;
    }

    // Remove local fixtures that no longer exist in the cloud for this venue
    if remote_ids.is_empty() {
        sqlx::query("DELETE FROM fixtures WHERE venue_id = ?")
            .bind(venue_id)
            .execute(pool)
            .await
            .map_err(|e| format!("Failed to delete stale fixtures: {}", e))?;
    } else {
        // Build a comma-separated list of quoted IDs for the NOT IN clause
        let placeholders: Vec<String> = remote_ids.iter().map(|id| format!("'{}'", id)).collect();
        let query = format!(
            "DELETE FROM fixtures WHERE venue_id = ? AND id NOT IN ({})",
            placeholders.join(",")
        );
        sqlx::query(&query)
            .bind(venue_id)
            .execute(pool)
            .await
            .map_err(|e| format!("Failed to delete stale fixtures: {}", e))?;
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
    venue_id: &str,
    access_token: &str,
) -> Result<usize, String> {
    #[derive(Deserialize)]
    struct RemoteGroup {
        id: String,
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
        fixture_id: String,
        display_order: i64,
    }

    let groups: Vec<RemoteGroup> = client
        .select(
            "fixture_groups",
            &format!(
                "venue_id=eq.{}&select=id,uid,name,axis_lr,axis_fb,axis_ab,movement_config,display_order",
                venue_id
            ),
            access_token,
        )
        .await
        .map_err(|e| format!("Failed to fetch venue groups: {}", e))?;

    let remote_group_ids: Vec<&str> = groups.iter().map(|g| g.id.as_str()).collect();

    let mut count = 0;
    for g in &groups {
        // Upsert group locally -- use (venue_id, name) as the natural key
        let local_group_id: String = sqlx::query_scalar(
            "INSERT INTO fixture_groups (id, uid, venue_id, name, axis_lr, axis_fb, axis_ab, movement_config, display_order)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(venue_id, name) DO UPDATE SET
               axis_lr = excluded.axis_lr,
               axis_fb = excluded.axis_fb,
               axis_ab = excluded.axis_ab,
               movement_config = excluded.movement_config,
               display_order = excluded.display_order
             RETURNING id",
        )
        .bind(&g.id)
        .bind(&g.uid)
        .bind(venue_id)
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
                &format!("group_id=eq.{}&select=fixture_id,display_order", g.id),
                access_token,
            )
            .await
            .map_err(|e| format!("Failed to fetch group members: {}", e))?;

        let remote_member_fixture_ids: Vec<&str> =
            members.iter().map(|m| m.fixture_id.as_str()).collect();

        // Insert memberships (fixture IDs are UUIDs matching local IDs)
        for m in &members {
            // Check if fixture exists locally
            let fixture_exists: bool = sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM fixtures WHERE id = ? AND venue_id = ?",
            )
            .bind(&m.fixture_id)
            .bind(venue_id)
            .fetch_one(pool)
            .await
            .unwrap_or(0)
                > 0;

            if fixture_exists {
                sqlx::query(
                    "INSERT INTO fixture_group_members (fixture_id, group_id, display_order)
                     VALUES (?, ?, ?)
                     ON CONFLICT(fixture_id, group_id) DO UPDATE SET display_order = excluded.display_order",
                )
                .bind(&m.fixture_id)
                .bind(&local_group_id)
                .bind(m.display_order)
                .execute(pool)
                .await
                .map_err(|e| format!("Failed to insert group member: {}", e))?;
            }
        }

        // Remove local group members that no longer exist in the cloud for this group
        if remote_member_fixture_ids.is_empty() {
            sqlx::query("DELETE FROM fixture_group_members WHERE group_id = ?")
                .bind(&local_group_id)
                .execute(pool)
                .await
                .map_err(|e| format!("Failed to delete stale group members: {}", e))?;
        } else {
            let placeholders: Vec<String> = remote_member_fixture_ids
                .iter()
                .map(|id| format!("'{}'", id))
                .collect();
            let query = format!(
                "DELETE FROM fixture_group_members WHERE group_id = ? AND fixture_id NOT IN ({})",
                placeholders.join(",")
            );
            sqlx::query(&query)
                .bind(&local_group_id)
                .execute(pool)
                .await
                .map_err(|e| format!("Failed to delete stale group members: {}", e))?;
        }

        count += 1;
    }

    // Remove local groups that no longer exist in the cloud for this venue
    if remote_group_ids.is_empty() {
        sqlx::query("DELETE FROM fixture_groups WHERE venue_id = ?")
            .bind(venue_id)
            .execute(pool)
            .await
            .map_err(|e| format!("Failed to delete stale groups: {}", e))?;
    } else {
        let placeholders: Vec<String> = remote_group_ids
            .iter()
            .map(|id| format!("'{}'", id))
            .collect();
        let query = format!(
            "DELETE FROM fixture_groups WHERE venue_id = ? AND id NOT IN ({})",
            placeholders.join(",")
        );
        sqlx::query(&query)
            .bind(venue_id)
            .execute(pool)
            .await
            .map_err(|e| format!("Failed to delete stale groups: {}", e))?;
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
    venue_id: &str,
    access_token: &str,
) -> Result<VenuePullStats, String> {
    let mut stats = VenuePullStats::default();

    // 1. Fetch all scores for this venue from all DJs
    let remote_scores: Vec<RemoteScore> = client
        .select(
            "scores",
            &format!(
                "venue_id=eq.{}&select=id,uid,track_id,venue_id,name,deleted_at,created_at,updated_at",
                venue_id
            ),
            access_token,
        )
        .await
        .map_err(|e| format!("Failed to fetch venue scores: {}", e))?;

    for remote_score in &remote_scores {
        if remote_score.deleted_at.is_some() {
            continue;
        }

        // 2. Ensure the track exists locally
        let local_track_id = match ensure_track_local(
            pool,
            client,
            app_handle,
            &remote_score.track_id,
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

        // 3. Upsert the score locally (using id as the primary key)
        sqlx::query(
            "INSERT INTO scores (id, uid, track_id, venue_id, name, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET
               name = excluded.name,
               updated_at = excluded.updated_at",
        )
        .bind(&remote_score.id)
        .bind(&remote_score.uid)
        .bind(&local_track_id)
        .bind(venue_id)
        .bind(&remote_score.name)
        .bind(&remote_score.created_at)
        .bind(&remote_score.updated_at)
        .execute(pool)
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

        // Upsert track_scores from cloud (never delete local data)
        for rts in &remote_track_scores {
            // 5. Ensure the pattern + implementation exist locally
            let local_pattern_id =
                match ensure_pattern_local(pool, client, &rts.pattern_id, access_token, &mut stats)
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

            sqlx::query(
                "INSERT INTO track_scores (id, uid, score_id, pattern_id, start_time, end_time, z_index, blend_mode, args_json, created_at, updated_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT(id) DO UPDATE SET
                   pattern_id = excluded.pattern_id,
                   start_time = excluded.start_time,
                   end_time = excluded.end_time,
                   z_index = excluded.z_index,
                   blend_mode = excluded.blend_mode,
                   args_json = excluded.args_json,
                   updated_at = excluded.updated_at",
            )
            .bind(&rts.id)
            .bind(&rts.uid)
            .bind(&remote_score.id)
            .bind(&local_pattern_id)
            .bind(rts.start_time)
            .bind(rts.end_time)
            .bind(rts.z_index)
            .bind(&rts.blend_mode)
            .bind(&rts.args_json)
            .bind(&rts.created_at)
            .bind(&rts.updated_at)
            .execute(pool)
            .await
            .map_err(|e| format!("Failed to upsert track_score: {}", e))?;

            stats.track_scores += 1;
        }
    }

    Ok(stats)
}

// ============================================================================
// Helpers: ensure track/pattern exist locally
// ============================================================================

/// Ensure a track exists in local DB (by id). If not, fetch metadata from cloud
/// and create a stub row. Returns the local track ID.
async fn ensure_track_local(
    pool: &SqlitePool,
    client: &SupabaseClient,
    app_handle: &AppHandle,
    track_id: &str,
    access_token: &str,
    stats: &mut VenuePullStats,
) -> Result<String, String> {
    // Check if already exists locally
    let existing: Option<String> = sqlx::query_scalar("SELECT id FROM tracks WHERE id = ?")
        .bind(track_id)
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
                "id=eq.{}&select=id,uid,track_hash,title,artist,album,track_number,disc_number,duration_seconds,storage_path,album_art_path,album_art_mime",
                track_id
            ),
            access_token,
        )
        .await
        .map_err(|e| format!("Failed to fetch track: {}", e))?;

    let track = rows
        .into_iter()
        .next()
        .ok_or_else(|| format!("Track {} not found in cloud", track_id))?;

    // Create local stub -- file_path is empty until audio is downloaded
    let storage_dir = crate::services::tracks::storage_dirs(app_handle)?;
    let file_path = storage_dir
        .0
        .join(format!("{}.stub", track.track_hash))
        .to_string_lossy()
        .to_string();

    sqlx::query(
        "INSERT INTO tracks (id, uid, track_hash, title, artist, album, track_number, disc_number, duration_seconds, file_path, storage_path, album_art_path, album_art_mime)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(track_hash) DO UPDATE SET
           title = excluded.title,
           artist = excluded.artist,
           album = excluded.album,
           storage_path = excluded.storage_path",
    )
    .bind(&track.id)
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
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to insert stub track: {}", e))?;

    stats.tracks_created += 1;

    // Audio and stem downloads are handled by the file_sync service.

    // Pull beats
    if let Err(e) = pull_track_beats(pool, client, &track.id, access_token).await {
        stats.errors.push(format!("Beats {}: {}", track_id, e));
    }

    // Pull roots
    if let Err(e) = pull_track_roots(pool, client, &track.id, access_token).await {
        stats.errors.push(format!("Roots {}: {}", track_id, e));
    }

    Ok(track.id.clone())
}

/// Ensure a pattern + implementation exist locally (by id).
async fn ensure_pattern_local(
    pool: &SqlitePool,
    client: &SupabaseClient,
    pattern_id: &str,
    access_token: &str,
    stats: &mut VenuePullStats,
) -> Result<String, String> {
    // Check if already exists locally
    let existing: Option<String> = sqlx::query_scalar("SELECT id FROM patterns WHERE id = ?")
        .bind(pattern_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| format!("DB error: {}", e))?;

    if let Some(local_id) = existing {
        // Still update the implementation in case it changed
        if let Ok(Some(impl_row)) =
            remote_queries::fetch_implementation_by_pattern(client, pattern_id, access_token).await
        {
            let _ = local_patterns::upsert_community_implementation(
                pool,
                &impl_row.id,
                &impl_row.uid,
                &local_id,
                impl_row.name.as_deref(),
                &impl_row.graph_json,
            )
            .await;
        }
        return Ok(local_id);
    }

    // Fetch pattern metadata from cloud
    let rows: Vec<crate::database::remote::queries::RemotePatternRow> = client
        .select(
            "patterns",
            &format!(
                "id=eq.{}&select=id,uid,name,description,is_published,author_name,created_at,updated_at",
                pattern_id
            ),
            access_token,
        )
        .await
        .map_err(|e| format!("Failed to fetch pattern: {}", e))?;

    let pat = rows
        .into_iter()
        .next()
        .ok_or_else(|| format!("Pattern {} not found in cloud", pattern_id))?;

    let local_id = local_patterns::upsert_community_pattern(
        pool,
        &pat.id,
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
    match remote_queries::fetch_implementation_by_pattern(client, pattern_id, access_token).await {
        Ok(Some(impl_row)) => {
            local_patterns::upsert_community_implementation(
                pool,
                &impl_row.id,
                &impl_row.uid,
                &local_id,
                impl_row.name.as_deref(),
                &impl_row.graph_json,
            )
            .await?;
        }
        Ok(None) => {}
        Err(e) => {
            stats
                .errors
                .push(format!("Implementation for pattern {}: {}", pattern_id, e));
        }
    }

    Ok(local_id)
}

// ============================================================================
// Beats/roots pull helpers
// ============================================================================

async fn pull_track_beats(
    pool: &SqlitePool,
    client: &SupabaseClient,
    track_id: &str,
    access_token: &str,
) -> Result<(), String> {
    let rows: Vec<RemoteTrackBeats> = client
        .select(
            "track_beats",
            &format!(
                "track_id=eq.{}&select=id,track_id,bpm,beats_json,downbeats_json,downbeat_offset",
                track_id
            ),
            access_token,
        )
        .await
        .map_err(|e| format!("Failed to fetch beats: {}", e))?;

    let Some(beats) = rows.into_iter().next() else {
        return Ok(());
    };

    sqlx::query(
        "INSERT INTO track_beats (track_id, uid, bpm, beats_json, downbeats_json, downbeat_offset)
         VALUES (?, (SELECT uid FROM tracks WHERE id = ?), ?, ?, ?, ?)
         ON CONFLICT(track_id) DO UPDATE SET
           bpm = excluded.bpm,
           beats_json = excluded.beats_json,
           downbeats_json = excluded.downbeats_json,
           downbeat_offset = excluded.downbeat_offset",
    )
    .bind(track_id)
    .bind(track_id)
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
    track_id: &str,
    access_token: &str,
) -> Result<(), String> {
    let rows: Vec<RemoteTrackRoots> = client
        .select(
            "track_roots",
            &format!("track_id=eq.{}&select=id,track_id,sections_json", track_id),
            access_token,
        )
        .await
        .map_err(|e| format!("Failed to fetch roots: {}", e))?;

    let Some(roots) = rows.into_iter().next() else {
        return Ok(());
    };

    sqlx::query(
        "INSERT INTO track_roots (track_id, uid, sections_json)
         VALUES (?, (SELECT uid FROM tracks WHERE id = ?), ?)
         ON CONFLICT(track_id) DO UPDATE SET
           sections_json = excluded.sections_json",
    )
    .bind(track_id)
    .bind(track_id)
    .bind(&roots.sections_json)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to upsert roots: {}", e))?;

    Ok(())
}
