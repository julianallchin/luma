use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
    SqlitePool,
};
use std::path::Path;
use tauri::{AppHandle, Manager};

/// Another handle to the same pool, never a second pool — see
/// [`super::database::Db`].
#[derive(Clone)]
pub struct StateDb(pub SqlitePool);

pub async fn init_state_db(app: &AppHandle) -> Result<StateDb, String> {
    let app_dir = app
        .path()
        .app_config_dir()
        .map_err(|e| format!("Failed to get app config dir: {}", e))?;
    init_state_db_at(&app_dir).await
}

/// [`init_state_db`] against an explicit config dir — see [`super::database::init_app_db_at`].
pub async fn init_state_db_at(app_dir: &Path) -> Result<StateDb, String> {
    std::fs::create_dir_all(app_dir).map_err(|e| {
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

    super::auth::initialize_auth_state_schema(&pool).await?;

    Ok(StateDb(pool))
}
