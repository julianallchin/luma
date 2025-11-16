use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
    SqlitePool,
};
use std::str::FromStr;
use tauri::{AppHandle, Manager};

pub struct Db(pub SqlitePool);

pub async fn init_db(app: &AppHandle) -> Result<Db, String> {
    let app_dir = app
        .path()
        .app_config_dir()
        .map_err(|e| format!("Failed to get app config dir: {}", e))?;
    std::fs::create_dir_all(&app_dir).map_err(|e| {
        format!(
            "Failed to create app config dir {}: {}",
            app_dir.display(),
            e
        )
    })?;

    let db_path = app_dir.join("luma.db");
    let connect_options = SqliteConnectOptions::from_str(&db_path.to_string_lossy())
        .map_err(|e| format!("Failed to create connection options: {}", e))?
        .create_if_missing(true);

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(connect_options)
        .await
        .map_err(|e| {
            format!(
                "Failed to connect to database at {}: {}",
                db_path.display(),
                e
            )
        })?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS patterns (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            description TEXT,
            graph_json TEXT NOT NULL DEFAULT '{\"nodes\":[],\"edges\":[]}',
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        )",
    )
    .execute(&pool)
    .await
    .map_err(|e| format!("Failed to create patterns table: {}", e))?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS tracks (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            track_hash TEXT NOT NULL UNIQUE,
            title TEXT,
            artist TEXT,
            album TEXT,
            track_number INTEGER,
            disc_number INTEGER,
            duration_seconds REAL,
            file_path TEXT NOT NULL,
            album_art_path TEXT,
            album_art_mime TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        )",
    )
    .execute(&pool)
    .await
    .map_err(|e| format!("Failed to create tracks table: {}", e))?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS track_beats (
            track_id INTEGER PRIMARY KEY,
            beats_json TEXT NOT NULL,
            downbeats_json TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            FOREIGN KEY(track_id) REFERENCES tracks(id) ON DELETE CASCADE
        )",
    )
    .execute(&pool)
    .await
    .map_err(|e| format!("Failed to create track beats table: {}", e))?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS track_roots (
            track_id INTEGER PRIMARY KEY,
            sections_json TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            FOREIGN KEY(track_id) REFERENCES tracks(id) ON DELETE CASCADE
        )",
    )
    .execute(&pool)
    .await
    .map_err(|e| format!("Failed to create track roots table: {}", e))?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS track_stems (
            track_id INTEGER NOT NULL,
            stem_name TEXT NOT NULL,
            file_path TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY(track_id, stem_name),
            FOREIGN KEY(track_id) REFERENCES tracks(id) ON DELETE CASCADE
        )",
    )
    .execute(&pool)
    .await
    .map_err(|e| format!("Failed to create track stems table: {}", e))?;

    let has_track_hash_column: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('tracks') WHERE name = 'track_hash'",
    )
    .fetch_one(&pool)
    .await
    .map_err(|e| format!("Failed to inspect tracks schema: {}", e))?;

    if has_track_hash_column == 0 {
        sqlx::query("ALTER TABLE tracks ADD COLUMN track_hash TEXT")
            .execute(&pool)
            .await
            .map_err(|e| format!("Failed to add track_hash column: {}", e))?;
    }

    sqlx::query("CREATE UNIQUE INDEX IF NOT EXISTS tracks_track_hash_idx ON tracks(track_hash)")
        .execute(&pool)
        .await
        .map_err(|e| format!("Failed to create track hash index: {}", e))?;

    Ok(Db(pool))
}
