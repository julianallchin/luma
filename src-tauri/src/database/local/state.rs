use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
    SqlitePool,
};
use tauri::{AppHandle, Manager};

pub struct StateDb(pub SqlitePool);

pub async fn init_state_db(app: &AppHandle) -> Result<StateDb, String> {
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

    let db_path = app_dir.join("state.db");
    let connect_options = SqliteConnectOptions::new()
        .filename(&db_path)
        .create_if_missing(true)
        .foreign_keys(true);

    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(connect_options)
        .await
        .map_err(|e| {
            format!(
                "Failed to connect to state database at {}: {}",
                db_path.display(),
                e
            )
        })?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS auth_session (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        )",
    )
    .execute(&pool)
    .await
    .map_err(|e| format!("Failed to initialize auth session table: {}", e))?;

    Ok(StateDb(pool))
}
