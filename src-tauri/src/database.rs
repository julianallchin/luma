use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
    SqlitePool,
};
use tauri::{AppHandle, Manager};

pub struct Db(pub SqlitePool);
pub struct ProjectDb(pub tokio::sync::Mutex<Option<SqlitePool>>);

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
    let connect_options = SqliteConnectOptions::new()
        .filename(&db_path)
        .journal_mode(SqliteJournalMode::Wal)
        .create_if_missing(true)
        .foreign_keys(true);

    let pool = SqlitePoolOptions::new()
        .max_connections(3)
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
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
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
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
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
            bpm REAL,
            downbeat_offset REAL,
            beats_per_bar INTEGER,
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
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
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
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
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            PRIMARY KEY(track_id, stem_name),
            FOREIGN KEY(track_id) REFERENCES tracks(id) ON DELETE CASCADE
        )",
    )
    .execute(&pool)
    .await
    .map_err(|e| format!("Failed to create track stems table: {}", e))?;

    // Track waveforms table (for timeline visualization)
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS track_waveforms (
            track_id INTEGER PRIMARY KEY,
            preview_samples_json TEXT NOT NULL,
            full_samples_json TEXT,
            colors_json TEXT,
            sample_rate INTEGER NOT NULL,
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            FOREIGN KEY(track_id) REFERENCES tracks(id) ON DELETE CASCADE
        )",
    )
    .execute(&pool)
    .await
    .map_err(|e| format!("Failed to create track waveforms table: {}", e))?;

    // Check for colors_json column
    let has_colors_json: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('track_waveforms') WHERE name = 'colors_json'",
    )
    .fetch_one(&pool)
    .await
    .map_err(|e| format!("Failed to inspect track_waveforms schema: {}", e))?;

    if has_colors_json == 0 {
        sqlx::query("ALTER TABLE track_waveforms ADD COLUMN colors_json TEXT")
            .execute(&pool)
            .await
            .map_err(|e| format!("Failed to add colors_json column: {}", e))?;
    }

    // Check for preview_colors_json column
    let has_preview_colors_json: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('track_waveforms') WHERE name = 'preview_colors_json'",
    )
    .fetch_one(&pool)
    .await
    .map_err(|e| format!("Failed to inspect track_waveforms schema: {}", e))?;

    if has_preview_colors_json == 0 {
        sqlx::query("ALTER TABLE track_waveforms ADD COLUMN preview_colors_json TEXT")
            .execute(&pool)
            .await
            .map_err(|e| format!("Failed to add preview_colors_json column: {}", e))?;
    }

    // Check for bands_json column (3-band envelopes for rekordbox-style waveform)
    let has_bands_json: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('track_waveforms') WHERE name = 'bands_json'",
    )
    .fetch_one(&pool)
    .await
    .map_err(|e| format!("Failed to inspect track_waveforms schema: {}", e))?;

    if has_bands_json == 0 {
        sqlx::query("ALTER TABLE track_waveforms ADD COLUMN bands_json TEXT")
            .execute(&pool)
            .await
            .map_err(|e| format!("Failed to add bands_json column: {}", e))?;
    }

    // Check for preview_bands_json column
    let has_preview_bands_json: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('track_waveforms') WHERE name = 'preview_bands_json'",
    )
    .fetch_one(&pool)
    .await
    .map_err(|e| format!("Failed to inspect track_waveforms schema: {}", e))?;

    if has_preview_bands_json == 0 {
        sqlx::query("ALTER TABLE track_waveforms ADD COLUMN preview_bands_json TEXT")
            .execute(&pool)
            .await
            .map_err(|e| format!("Failed to add preview_bands_json column: {}", e))?;
    }

    // Track annotations table (patterns placed on timeline)
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS track_annotations (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            track_id INTEGER NOT NULL,
            pattern_id INTEGER NOT NULL,
            start_time REAL NOT NULL,
            end_time REAL NOT NULL,
            z_index INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            FOREIGN KEY(track_id) REFERENCES tracks(id) ON DELETE CASCADE,
            FOREIGN KEY(pattern_id) REFERENCES patterns(id) ON DELETE CASCADE
        )",
    )
    .execute(&pool)
    .await
    .map_err(|e| format!("Failed to create track annotations table: {}", e))?;

    // Index for efficient annotation lookups
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS track_annotations_track_idx ON track_annotations(track_id)",
    )
    .execute(&pool)
    .await
    .map_err(|e| format!("Failed to create track annotations index: {}", e))?;

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

    // Recent Projects Table
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS recent_projects (
            path TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            last_opened TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        )",
    )
    .execute(&pool)
    .await
    .map_err(|e| format!("Failed to create recent_projects table: {}", e))?;

    // updated_at triggers for tables with mutable rows
    let updated_at_targets = [
        ("patterns", "id = OLD.id"),
        ("tracks", "id = OLD.id"),
        ("track_beats", "track_id = OLD.track_id"),
        ("track_roots", "track_id = OLD.track_id"),
        (
            "track_stems",
            "track_id = OLD.track_id AND stem_name = OLD.stem_name",
        ),
        ("track_waveforms", "track_id = OLD.track_id"),
        ("track_annotations", "id = OLD.id"),
    ];

    for (table, where_clause) in updated_at_targets {
        sqlx::query(&updated_at_trigger_sql(table, where_clause))
            .execute(&pool)
            .await
            .map_err(|e| format!("Failed to create {table} updated_at trigger: {e}"))?;
    }

    Ok(Db(pool))
}

pub async fn init_project_db(path: &str) -> Result<SqlitePool, String> {
    let connect_options = SqliteConnectOptions::new()
        .filename(path)
        .journal_mode(SqliteJournalMode::Wal)
        .create_if_missing(true)
        .foreign_keys(true);

    let pool = SqlitePoolOptions::new()
        .max_connections(3)
        .connect_with(connect_options)
        .await
        .map_err(|e| format!("Failed to connect to project database at {}: {}", path, e))?;

    // Initialize Project Schema
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS implementations (
            pattern_id INTEGER PRIMARY KEY,
            graph_json TEXT NOT NULL DEFAULT '{\"nodes\":[],\"edges\":[]}',
            updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        )",
    )
    .execute(&pool)
    .await
    .map_err(|e| format!("Failed to create implementations table: {}", e))?;

    sqlx::query(&updated_at_trigger_sql(
        "implementations",
        "pattern_id = OLD.pattern_id",
    ))
    .execute(&pool)
    .await
    .map_err(|e| format!("Failed to create implementations updated_at trigger: {}", e))?;

    Ok(pool)
}

fn updated_at_trigger_sql(table: &str, where_clause: &str) -> String {
    format!(
        "CREATE TRIGGER IF NOT EXISTS {table}_updated_at
        AFTER UPDATE ON {table}
        FOR EACH ROW
        WHEN NEW.updated_at = OLD.updated_at
        BEGIN
            UPDATE {table}
            SET updated_at = CURRENT_TIMESTAMP
            WHERE {where_clause};
        END;"
    )
}
