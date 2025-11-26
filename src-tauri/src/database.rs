use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
    SqlitePool,
};
use tauri::{AppHandle, Manager};

pub struct Db(pub SqlitePool);
pub struct ProjectDb(pub tokio::sync::Mutex<Option<SqlitePool>>);

/*
 * Initializes the app database, used to store app-level data
 * such as patterns, tracks, and annotations.
 */
pub async fn init_app_db(app: &AppHandle) -> Result<Db, String> {
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

    sqlx::migrate!("./migrations/app")
        .run(&pool)
        .await
        .map_err(|e| format!("Failed to run app migrations: {}", e))?;

    Ok(Db(pool))
}

/*
 * Initializes the project database, used to store project-level data
 * with implementation graphs and populated DMX universes.
 */
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

    sqlx::migrate!("./migrations/project")
        .run(&pool)
        .await
        .map_err(|e| format!("Failed to run project migrations: {}", e))?;

    Ok(pool)
}
