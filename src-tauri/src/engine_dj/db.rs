use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{FromRow, SqlitePool};

use super::types::{EngineDjLibraryInfo, EngineDjPlaylist, EngineDjTrack};

/// Open an Engine DJ m.db database in read-only mode.
/// Each call creates a fresh connection — no long-lived locks on the user's library.
pub async fn open_engine_db(library_path: &str) -> Result<SqlitePool, String> {
    let db_path = std::path::Path::new(library_path)
        .join("Database2")
        .join("m.db");

    if !db_path.exists() {
        return Err(format!(
            "Engine DJ database not found at {}",
            db_path.display()
        ));
    }

    let options = SqliteConnectOptions::new()
        .filename(&db_path)
        .read_only(true)
        .create_if_missing(false);

    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .map_err(|e| format!("Failed to open Engine DJ database: {}", e))
}

/// Get library info (uuid, path, track count).
pub async fn get_library_info(
    pool: &SqlitePool,
    library_path: &str,
) -> Result<EngineDjLibraryInfo, String> {
    #[derive(FromRow)]
    struct InfoRow {
        uuid: String,
    }

    let info = sqlx::query_as::<_, InfoRow>("SELECT uuid FROM Information LIMIT 1")
        .fetch_optional(pool)
        .await
        .map_err(|e| format!("Failed to read Engine DJ Information table: {}", e))?
        .ok_or_else(|| "Engine DJ database has no Information record".to_string())?;

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM Track")
        .fetch_one(pool)
        .await
        .map_err(|e| format!("Failed to count tracks: {}", e))?;

    Ok(EngineDjLibraryInfo {
        database_uuid: info.uuid,
        library_path: library_path.to_string(),
        track_count: count,
    })
}

/// List all tracks from the Engine DJ library.
pub async fn list_tracks(pool: &SqlitePool) -> Result<Vec<EngineDjTrack>, String> {
    #[derive(FromRow)]
    struct TrackRow {
        id: i64,
        path: String,
        filename: String,
        title: Option<String>,
        artist: Option<String>,
        album: Option<String>,
        #[sqlx(rename = "bpmAnalyzed")]
        bpm_analyzed: Option<f64>,
        length: Option<f64>,
        #[sqlx(rename = "originDatabaseUuid")]
        origin_database_uuid: Option<String>,
        #[sqlx(rename = "originTrackId")]
        origin_track_id: Option<i64>,
    }

    let rows = sqlx::query_as::<_, TrackRow>(
        "SELECT id, path, filename, title, artist, album, bpmAnalyzed, CAST(length AS REAL) AS length, originDatabaseUuid, originTrackId FROM Track ORDER BY title",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to list Engine DJ tracks: {}", e))?;

    Ok(rows
        .into_iter()
        .map(|r| EngineDjTrack {
            id: r.id,
            path: r.path,
            filename: r.filename,
            title: r.title,
            artist: r.artist,
            album: r.album,
            bpm_analyzed: r.bpm_analyzed,
            length: r.length,
            origin_database_uuid: r.origin_database_uuid,
            origin_track_id: r.origin_track_id,
        })
        .collect())
}

/// List all playlists from the Engine DJ library.
pub async fn list_playlists(pool: &SqlitePool) -> Result<Vec<EngineDjPlaylist>, String> {
    #[derive(FromRow)]
    struct PlaylistRow {
        id: i64,
        title: String,
        #[sqlx(rename = "parentListId")]
        parent_id: Option<i64>,
    }

    let rows = sqlx::query_as::<_, PlaylistRow>(
        "SELECT id, title, parentListId FROM Playlist ORDER BY title",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to list Engine DJ playlists: {}", e))?;

    // Compute track count per playlist
    #[derive(FromRow)]
    struct CountRow {
        list_id: i64,
        cnt: i64,
    }

    let counts = sqlx::query_as::<_, CountRow>(
        "SELECT listId as list_id, COUNT(*) as cnt FROM PlaylistEntity GROUP BY listId",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to count playlist tracks: {}", e))?;

    let count_map: std::collections::HashMap<i64, i64> =
        counts.into_iter().map(|c| (c.list_id, c.cnt)).collect();

    Ok(rows
        .into_iter()
        .map(|r| EngineDjPlaylist {
            id: r.id,
            title: r.title,
            parent_id: r.parent_id,
            track_count: *count_map.get(&r.id).unwrap_or(&0),
        })
        .collect())
}

/// Get tracks in a specific playlist.
pub async fn get_playlist_tracks(
    pool: &SqlitePool,
    playlist_id: i64,
) -> Result<Vec<EngineDjTrack>, String> {
    #[derive(FromRow)]
    struct TrackRow {
        id: i64,
        path: String,
        filename: String,
        title: Option<String>,
        artist: Option<String>,
        album: Option<String>,
        #[sqlx(rename = "bpmAnalyzed")]
        bpm_analyzed: Option<f64>,
        length: Option<f64>,
        #[sqlx(rename = "originDatabaseUuid")]
        origin_database_uuid: Option<String>,
        #[sqlx(rename = "originTrackId")]
        origin_track_id: Option<i64>,
    }

    let rows = sqlx::query_as::<_, TrackRow>(
        "SELECT t.id, t.path, t.filename, t.title, t.artist, t.album, t.bpmAnalyzed, CAST(t.length AS REAL) AS length, t.originDatabaseUuid, t.originTrackId
         FROM Track t
         INNER JOIN PlaylistEntity pe ON pe.trackId = t.id
         WHERE pe.listId = ?
         ORDER BY t.title",
    )
    .bind(playlist_id)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to get playlist tracks: {}", e))?;

    Ok(rows
        .into_iter()
        .map(|r| EngineDjTrack {
            id: r.id,
            path: r.path,
            filename: r.filename,
            title: r.title,
            artist: r.artist,
            album: r.album,
            bpm_analyzed: r.bpm_analyzed,
            length: r.length,
            origin_database_uuid: r.origin_database_uuid,
            origin_track_id: r.origin_track_id,
        })
        .collect())
}

/// Search tracks by title, artist, or filename.
pub async fn search_tracks(pool: &SqlitePool, query: &str) -> Result<Vec<EngineDjTrack>, String> {
    #[derive(FromRow)]
    struct TrackRow {
        id: i64,
        path: String,
        filename: String,
        title: Option<String>,
        artist: Option<String>,
        album: Option<String>,
        #[sqlx(rename = "bpmAnalyzed")]
        bpm_analyzed: Option<f64>,
        length: Option<f64>,
        #[sqlx(rename = "originDatabaseUuid")]
        origin_database_uuid: Option<String>,
        #[sqlx(rename = "originTrackId")]
        origin_track_id: Option<i64>,
    }

    let pattern = format!("%{}%", query);
    let rows = sqlx::query_as::<_, TrackRow>(
        "SELECT id, path, filename, title, artist, album, bpmAnalyzed, CAST(length AS REAL) AS length, originDatabaseUuid, originTrackId
         FROM Track
         WHERE title LIKE ? OR artist LIKE ? OR filename LIKE ?
         ORDER BY title
         LIMIT 200",
    )
    .bind(&pattern)
    .bind(&pattern)
    .bind(&pattern)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to search Engine DJ tracks: {}", e))?;

    Ok(rows
        .into_iter()
        .map(|r| EngineDjTrack {
            id: r.id,
            path: r.path,
            filename: r.filename,
            title: r.title,
            artist: r.artist,
            album: r.album,
            bpm_analyzed: r.bpm_analyzed,
            length: r.length,
            origin_database_uuid: r.origin_database_uuid,
            origin_track_id: r.origin_track_id,
        })
        .collect())
}
