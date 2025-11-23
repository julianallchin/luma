use crate::database::{Db, ProjectDb, init_project_db};
use serde::Serialize;
use std::path::Path;
use tauri::State;

#[derive(Serialize, sqlx::FromRow)]
pub struct RecentProject {
    pub path: String,
    pub name: String,
    pub last_opened: String,
}

#[tauri::command]
pub async fn create_project(
    path: String,
    db: State<'_, Db>,
    project_db: State<'_, ProjectDb>,
) -> Result<(), String> {
    // Create the project file (initialize DB)
    let pool = init_project_db(&path).await?;
    
    // Update global state
    let mut lock = project_db.0.lock().await;
    *lock = Some(pool);
    
    // Add to recent projects
    let name = Path::new(&path)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    sqlx::query(
        "INSERT INTO recent_projects (path, name, last_opened) VALUES (?, ?, datetime('now'))
         ON CONFLICT(path) DO UPDATE SET last_opened = datetime('now')",
    )
    .bind(&path)
    .bind(&name)
    .execute(&db.0)
    .await
    .map_err(|e| format!("Failed to update recent projects: {}", e))?;

    Ok(())
}

#[tauri::command]
pub async fn open_project(
    path: String,
    db: State<'_, Db>,
    project_db: State<'_, ProjectDb>,
) -> Result<(), String> {
    if !Path::new(&path).exists() {
        return Err("Project file does not exist".to_string());
    }

    let pool = init_project_db(&path).await?;
    
    let mut lock = project_db.0.lock().await;
    *lock = Some(pool);
    
    let name = Path::new(&path)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    sqlx::query(
        "INSERT INTO recent_projects (path, name, last_opened) VALUES (?, ?, datetime('now'))
         ON CONFLICT(path) DO UPDATE SET last_opened = datetime('now')",
    )
    .bind(&path)
    .bind(&name)
    .execute(&db.0)
    .await
    .map_err(|e| format!("Failed to update recent projects: {}", e))?;

    Ok(())
}

#[tauri::command]
pub async fn close_project(project_db: State<'_, ProjectDb>) -> Result<(), String> {
    let mut lock = project_db.0.lock().await;
    if let Some(pool) = lock.take() {
        pool.close().await;
    }
    Ok(())
}

#[tauri::command]
pub async fn get_recent_projects(db: State<'_, Db>) -> Result<Vec<RecentProject>, String> {
    let projects = sqlx::query_as::<_, RecentProject>(
        "SELECT path, name, last_opened FROM recent_projects ORDER BY last_opened DESC LIMIT 10",
    )
    .fetch_all(&db.0)
    .await
    .map_err(|e| format!("Failed to fetch recent projects: {}", e))?;

    Ok(projects)
}

