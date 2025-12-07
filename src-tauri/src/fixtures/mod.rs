pub mod layout;
pub mod models;
pub mod parser;
pub mod engine;

use self::models::{FixtureDefinition, FixtureEntry, PatchedFixture};
use self::parser::FixtureIndex;
use crate::database::ProjectDb;
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{command, AppHandle, Manager, State};
use uuid::Uuid;

// State to hold the index in memory
pub struct FixtureState(pub Mutex<Option<FixtureIndex>>);

#[command]
pub async fn initialize_fixtures(
    app: AppHandle,
    state: State<'_, FixtureState>,
) -> Result<usize, String> {
    // Try resource dir first, then fallback to relative path for dev
    let resource_path = app
        .path()
        .resource_dir()
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
pub fn search_fixtures(
    query: String,
    offset: usize,
    limit: usize,
    state: State<'_, FixtureState>,
) -> Result<Vec<FixtureEntry>, String> {
    let state_guard = state.0.lock().unwrap();

    let index = state_guard
        .as_ref()
        .ok_or("Fixtures not initialized. Call initialize_fixtures first.")?;

    let query = query.to_lowercase();

    // If query is empty, just paginate the whole list
    if query.is_empty() {
        return Ok(index
            .entries
            .iter()
            .skip(offset)
            .take(limit)
            .cloned()
            .collect());
    }

    // Otherwise filter then paginate
    let results: Vec<FixtureEntry> = index
        .entries
        .iter()
        .filter(|f| {
            f.manufacturer.to_lowercase().contains(&query)
                || f.model.to_lowercase().contains(&query)
        })
        .skip(offset)
        .take(limit)
        .cloned()
        .collect();

    Ok(results)
}

#[command]
pub fn get_fixture_definition(app: AppHandle, path: String) -> Result<FixtureDefinition, String> {
    let resource_path = app
        .path()
        .resource_dir()
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
pub async fn get_patch_hierarchy(
    app: AppHandle,
    project_db: State<'_, ProjectDb>,
) -> Result<Vec<models::FixtureNode>, String> {
    let project_pool = project_db.0.lock().await;
    let pool = project_pool.as_ref().ok_or("Project DB not initialized")?;

    // 1. Fetch patched fixtures
    let fixtures = sqlx::query_as::<_, PatchedFixture>(
        "SELECT id, universe, address, num_channels, manufacturer, model, mode_name, fixture_path, label, pos_x, pos_y, pos_z, rot_x, rot_y, rot_z FROM fixtures"
    )
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to fetch fixtures: {}", e))?;

    let mut hierarchy = Vec::new();

    // 2. Resolve resource root
    let resource_path = app
        .path()
        .resource_dir()
        .map(|p| p.join("resources/fixtures/2511260420"))
        .unwrap_or_else(|_| PathBuf::from("resources/fixtures/2511260420"));

    let final_path = if resource_path.exists() {
        resource_path
    } else {
        let cwd = std::env::current_dir().unwrap_or_default();
        let dev_path = cwd.join("../resources/fixtures/2511260420");
        if dev_path.exists() {
            dev_path
        } else {
            cwd.join("resources/fixtures/2511260420")
        }
    };

    // 3. Build nodes
    for fixture in fixtures {
        let def_path = final_path.join(&fixture.fixture_path);
        let mut children = Vec::new();

        // Load def to check heads
        if let Ok(def) = parser::parse_definition(&def_path) {
            // Find mode
            if let Some(mode) = def.modes.iter().find(|m| m.name == fixture.mode_name) {
                // If multi-head (more than 1 head defined)
                // Note: QLC+ sometimes defines 1 head for simple fixtures.
                // We usually only want to show children if there are >1 heads OR explicitly useful.
                // Let's show children if mode.heads.len() > 0.
                
                // Actually, if mode.heads is empty, it implies 1 head (the whole fixture).
                // If mode.heads has items, we list them.
                
                if !mode.heads.is_empty() {
                    for (i, _head) in mode.heads.iter().enumerate() {
                        children.push(models::FixtureNode {
                            id: format!("{}:{}", fixture.id, i),
                            label: format!("Head {}", i + 1),
                            type_: models::FixtureNodeType::Head,
                            children: vec![],
                        });
                    }
                }
            }
        }

        // If only 1 child and it covers all channels or is just 1 head, maybe flatten?
        // But for consistency with the selection logic:
        // Selection logic expands fixtureID -> all heads.
        // So selecting the Parent Node selects all children.
        // Selecting a Child Node selects just that head.
        
        hierarchy.push(models::FixtureNode {
            id: fixture.id.clone(),
            label: fixture.label.clone().unwrap_or_else(|| format!("{} {}", fixture.manufacturer, fixture.model)),
            type_: models::FixtureNodeType::Fixture,
            children,
        });
    }

    Ok(hierarchy)
}

#[command]
pub async fn patch_fixture(
    app: AppHandle,
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
    
    refresh_artnet(&app, pool).await?;

    Ok(patched_fixture)
}

#[command]
pub async fn get_patched_fixtures(
    project_db: State<'_, ProjectDb>,
) -> Result<Vec<PatchedFixture>, String> {
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
    app: AppHandle,
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
    
    refresh_artnet(&app, pool).await?;

    Ok(())
}

#[command]
pub async fn move_patched_fixture_spatial(
    app: AppHandle,
    project_db: State<'_, ProjectDb>,
    id: String,
    pos_x: f64,
    pos_y: f64,
    pos_z: f64,
    rot_x: f64,
    rot_y: f64,
    rot_z: f64,
) -> Result<(), String> {
    let project_pool = project_db.0.lock().await;
    let pool = project_pool.as_ref().ok_or("Project DB not initialized")?;

    let result = sqlx::query(
        "UPDATE fixtures SET pos_x = ?, pos_y = ?, pos_z = ?, rot_x = ?, rot_y = ?, rot_z = ? WHERE id = ?"
    )
    .bind(pos_x)
    .bind(pos_y)
    .bind(pos_z)
    .bind(rot_x)
    .bind(rot_y)
    .bind(rot_z)
    .bind(&id)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to update fixture spatial data: {}", e))?;

    if result.rows_affected() == 0 {
        return Err(format!("No fixture found to update for id {}", id));
    }
    
    refresh_artnet(&app, pool).await?;

    Ok(())
}

#[command]
pub async fn remove_patched_fixture(
    app: AppHandle,
    project_db: State<'_, ProjectDb>,
    id: String,
) -> Result<(), String> {
    let project_pool = project_db.0.lock().await;
    let pool = project_pool.as_ref().ok_or("Project DB not initialized")?;

    sqlx::query("DELETE FROM fixtures WHERE id = ?")
        .bind(&id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to remove patched fixture: {}", e))?;
    
    refresh_artnet(&app, pool).await?;

    Ok(())
}

#[command]
pub async fn rename_patched_fixture(
    app: AppHandle,
    project_db: State<'_, ProjectDb>,
    id: String,
    label: String,
) -> Result<(), String> {
    let project_pool = project_db.0.lock().await;
    let pool = project_pool.as_ref().ok_or("Project DB not initialized")?;

    let result = sqlx::query("UPDATE fixtures SET label = ? WHERE id = ?")
        .bind(label)
        .bind(&id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to rename patched fixture: {}", e))?;

    if result.rows_affected() == 0 {
        return Err(format!("No fixture found to rename for id {}", id));
    }
    
    refresh_artnet(&app, pool).await?;

    Ok(())
}

async fn refresh_artnet(app: &AppHandle, pool: &sqlx::SqlitePool) -> Result<(), String> {
    let fixtures = sqlx::query_as::<_, PatchedFixture>(
        "SELECT id, universe, address, num_channels, manufacturer, model, mode_name, fixture_path, label, pos_x, pos_y, pos_z, rot_x, rot_y, rot_z FROM fixtures"
    )
    .fetch_all(pool)
    .await
    .map_err(|e| format!("Failed to get patched fixtures: {}", e))?;
    
    if let Some(artnet) = app.try_state::<crate::artnet::ArtNetManager>() {
        artnet.update_patch(fixtures);
    }
    Ok(())
}
