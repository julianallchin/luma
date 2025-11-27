pub mod models;
mod parser;

use tauri::{AppHandle, Manager, State, command};
use std::path::PathBuf;
use std::sync::Mutex;
use self::models::{FixtureDefinition, FixtureEntry, PatchedFixture};
use self::parser::FixtureIndex;
use crate::database::ProjectDb;
use uuid::Uuid;

// State to hold the index in memory
pub struct FixtureState(pub Mutex<Option<FixtureIndex>>);

#[command]
pub async fn initialize_fixtures(app: AppHandle, state: State<'_, FixtureState>) -> Result<usize, String> {
    // Try resource dir first, then fallback to relative path for dev
    let resource_path = app.path().resource_dir()
        .map(|p| p.join("resources/fixtures/2511260420"))
        .unwrap_or_else(|_| PathBuf::from("resources/fixtures/2511260420"));
    
    // If resource path doesn't exist (common in dev if not copied), try absolute from CWD
    let final_path = if resource_path.exists() {
            resource_path
        } else {
            let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
            let dev_path = cwd.join("../resources/fixtures/2511260420");
            if dev_path.exists() {
                dev_path
            } else {
                // Fallback to just resources/ in case CWD is root
                cwd.join("resources/fixtures/2511260420")
            }
        };
    if !final_path.exists() {
        return Err(format!("Fixtures directory not found at {:?}", final_path));
    }

    let index = parser::build_index(&final_path).map_err(|e| e.to_string())?;
    let count = index.entries.len();
    *state.0.lock().unwrap() = Some(index);
    Ok(count)
}

#[command]
pub fn search_fixtures(query: String, offset: usize, limit: usize, state: State<'_, FixtureState>) -> Result<Vec<FixtureEntry>, String> {
    let state_guard = state.0.lock().unwrap();
    
    let index = state_guard.as_ref().ok_or("Fixtures not initialized. Call initialize_fixtures first.")?;
    
    let query = query.to_lowercase();
    
    // If query is empty, just paginate the whole list
    if query.is_empty() {
         return Ok(index.entries.iter().skip(offset).take(limit).cloned().collect());
    }

    // Otherwise filter then paginate
    let results: Vec<FixtureEntry> = index.entries.iter()
        .filter(|f| f.manufacturer.to_lowercase().contains(&query) || f.model.to_lowercase().contains(&query))
        .skip(offset)
        .take(limit)
        .cloned()
        .collect();
    
    Ok(results)
}

#[command]
pub fn get_fixture_definition(app: AppHandle, path: String) -> Result<FixtureDefinition, String> {
    let resource_path = app.path().resource_dir()
        .map(|p| p.join("resources/fixtures/2511260420"))
        .unwrap_or_else(|_| PathBuf::from("resources/fixtures/2511260420"));

         // If resource path doesn't exist (common in dev if not copied), try absolute from CWD
         let final_path = if resource_path.exists() {
             resource_path
         } else {
             let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
             let dev_path = cwd.join("../resources/fixtures/2511260420");
             if dev_path.exists() {
                 dev_path
             } else {
                 // Fallback to just resources/ in case CWD is root
                 cwd.join("resources/fixtures/2511260420")
             }
         };
    let full_path = final_path.join(path);
        
    parser::parse_definition(&full_path).map_err(|e| e.to_string())
}

#[command]
pub async fn patch_fixture(
    project_db: State<'_, ProjectDb>,
    universe: i64,
    address: i64,
    num_channels: i64,
    manufacturer: String,
    model: String,
    mode_name: String,
    fixture_path: String,
    label: Option<String>,
) -> Result<PatchedFixture, String> {
    let project_pool = project_db.0.lock().await;
    let pool = project_pool.as_ref().ok_or("Project DB not initialized")?;

    let id = Uuid::new_v4().to_string();

    sqlx::query(
        "INSERT INTO fixtures (id, universe, address, num_channels, manufacturer, model, mode_name, fixture_path, label, pos_x, pos_y, pos_z, rot_x, rot_y, rot_z)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
    )
    .bind(&id)
    .bind(universe)
    .bind(address)
    .bind(num_channels)
    .bind(&manufacturer)
    .bind(&model)
    .bind(&mode_name)
    .bind(&fixture_path)
    .bind(&label)
    .bind(0.0) // Default pos_x
    .bind(0.0) // Default pos_y
    .bind(0.0) // Default pos_z
    .bind(0.0) // Default rot_x
    .bind(0.0) // Default rot_y
    .bind(0.0) // Default rot_z
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to patch fixture: {}", e))?;

    let patched_fixture = PatchedFixture {
        id,
        universe,
        address,
        num_channels,
        manufacturer: manufacturer.clone(),
        model: model.clone(),
        mode_name: mode_name.clone(),
        fixture_path: fixture_path.clone(),
        label: label.clone(),
        pos_x: 0.0,
        pos_y: 0.0,
        pos_z: 0.0,
        rot_x: 0.0,
        rot_y: 0.0,
        rot_z: 0.0,
    };

    Ok(patched_fixture)
}

#[command]
pub async fn get_patched_fixtures(project_db: State<'_, ProjectDb>) -> Result<Vec<PatchedFixture>, String> {
    let project_pool = project_db.0.lock().await;
    let pool = project_pool.as_ref().ok_or("Project DB not initialized")?;

    let fixtures = sqlx::query_as::<_, PatchedFixture>(
        "SELECT id, universe, address, num_channels, manufacturer, model, mode_name, fixture_path, label, pos_x, pos_y, pos_z, rot_x, rot_y, rot_z FROM fixtures"
    )
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to get patched fixtures: {}", e))?;

    Ok(fixtures)
}

#[command]
pub async fn move_patched_fixture(
    project_db: State<'_, ProjectDb>,
    id: String,
    address: i64,
) -> Result<(), String> {
    let project_pool = project_db.0.lock().await;
    let pool = project_pool.as_ref().ok_or("Project DB not initialized")?;

    let result = sqlx::query("UPDATE fixtures SET address = ? WHERE id = ?")
        .bind(address)
        .bind(&id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to move patched fixture: {}", e))?;

    if result.rows_affected() == 0 {
        return Err(format!("No fixture found to move for id {}", id));
    }

    Ok(())
}

#[command]
pub async fn remove_patched_fixture(project_db: State<'_, ProjectDb>, id: String) -> Result<(), String> {
    let project_pool = project_db.0.lock().await;
    let pool = project_pool.as_ref().ok_or("Project DB not initialized")?;

    sqlx::query("DELETE FROM fixtures WHERE id = ?")
        .bind(&id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to remove patched fixture: {}", e))?;

    Ok(())
}
