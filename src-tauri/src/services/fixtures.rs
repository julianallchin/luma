//! Business logic for fixture operations.
//!
//! Database layer handles CRUD only. File/resource access, ArtNet refresh, and
//! in-memory fixture index live here.

use std::path::PathBuf;
use std::sync::Mutex;

use sqlx::SqlitePool;
use tauri::{AppHandle, Manager};

use crate::database::local::fixtures as fixtures_db;
use crate::fixtures::parser::{self, FixtureIndex};
use crate::models::fixtures::{
    FixtureDefinition, FixtureEntry, FixtureNode, FixtureNodeType, PatchedFixture,
};

// State to hold the index in memory
pub struct FixtureState(pub Mutex<Option<FixtureIndex>>);

/// Initialize the fixture library (file-system side)
pub async fn initialize_fixtures(app: &AppHandle, state: &FixtureState) -> Result<usize, String> {
    let final_path = resolve_fixtures_root(app)?;
    let index = parser::build_index(&final_path).map_err(|e| e.to_string())?;
    let count = index.entries.len();
    *state.0.lock().unwrap() = Some(index);
    Ok(count)
}

/// Search for fixtures in the library
pub fn search_fixtures(
    query: String,
    offset: usize,
    limit: usize,
    state: &FixtureState,
) -> Result<Vec<FixtureEntry>, String> {
    let state_guard = state.0.lock().unwrap();

    let index = state_guard
        .as_ref()
        .ok_or("Fixtures not initialized. Call initialize_fixtures first.")?;

    let query = query.to_lowercase();

    if query.is_empty() {
        return Ok(index
            .entries
            .iter()
            .skip(offset)
            .take(limit)
            .cloned()
            .collect());
    }

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

/// Get fixture definition from file
pub fn get_fixture_definition(app: &AppHandle, path: String) -> Result<FixtureDefinition, String> {
    let root = resolve_fixtures_root(app)?;
    let full_path = root.join(path);
    parser::parse_definition(&full_path).map_err(|e| e.to_string())
}

/// Patch a fixture to a venue
pub async fn patch_fixture(
    app: &AppHandle,
    pool: &SqlitePool,
    venue_id: i64,
    universe: i64,
    address: i64,
    num_channels: i64,
    manufacturer: String,
    model: String,
    mode_name: String,
    fixture_path: String,
    label: Option<String>,
    uid: Option<String>,
) -> Result<PatchedFixture, String> {
    let fixture = fixtures_db::insert_fixture(
        pool,
        venue_id,
        universe,
        address,
        num_channels,
        &manufacturer,
        &model,
        &mode_name,
        &fixture_path,
        label.as_deref(),
        uid.as_deref(),
    )
    .await?;

    refresh_artnet(app, pool, venue_id).await?;
    Ok(fixture)
}

/// Get all patched fixtures for a venue
pub async fn get_patched_fixtures(
    pool: &SqlitePool,
    venue_id: i64,
) -> Result<Vec<PatchedFixture>, String> {
    fixtures_db::get_patched_fixtures(pool, venue_id).await
}

/// Get patch hierarchy for a venue
pub async fn get_patch_hierarchy(
    app: &AppHandle,
    pool: &SqlitePool,
    venue_id: i64,
) -> Result<Vec<FixtureNode>, String> {
    let fixtures = fixtures_db::get_patched_fixtures(pool, venue_id).await?;
    let root = resolve_fixtures_root(app)?;

    let mut hierarchy = Vec::new();
    for fixture in fixtures {
        let def_path = root.join(&fixture.fixture_path);
        let mut children = Vec::new();

        if let Ok(def) = parser::parse_definition(&def_path) {
            if let Some(mode) = def.modes.iter().find(|m| m.name == fixture.mode_name) {
                if !mode.heads.is_empty() {
                    for (i, _head) in mode.heads.iter().enumerate() {
                        children.push(FixtureNode {
                            id: format!("{}:{}", fixture.id, i),
                            label: format!("Head {}", i + 1),
                            type_: FixtureNodeType::Head,
                            children: vec![],
                        });
                    }
                }
            }
        }

        hierarchy.push(FixtureNode {
            id: fixture.id.clone(),
            label: fixture
                .label
                .clone()
                .unwrap_or_else(|| format!("{} {}", fixture.manufacturer, fixture.model)),
            type_: FixtureNodeType::Fixture,
            children,
        });
    }

    Ok(hierarchy)
}

/// Move a patched fixture to a new address
pub async fn move_patched_fixture(
    app: &AppHandle,
    pool: &SqlitePool,
    venue_id: i64,
    id: String,
    address: i64,
) -> Result<(), String> {
    let rows = fixtures_db::update_fixture_address(pool, &id, address).await?;
    if rows == 0 {
        return Err(format!("No fixture found to move for id {}", id));
    }
    refresh_artnet(app, pool, venue_id).await
}

/// Move a patched fixture in 3D space
pub async fn move_patched_fixture_spatial(
    app: &AppHandle,
    pool: &SqlitePool,
    venue_id: i64,
    id: String,
    pos_x: f64,
    pos_y: f64,
    pos_z: f64,
    rot_x: f64,
    rot_y: f64,
    rot_z: f64,
) -> Result<(), String> {
    let rows =
        fixtures_db::update_fixture_spatial(pool, &id, pos_x, pos_y, pos_z, rot_x, rot_y, rot_z)
            .await?;
    if rows == 0 {
        return Err(format!("No fixture found to update for id {}", id));
    }
    refresh_artnet(app, pool, venue_id).await
}

/// Remove a patched fixture
pub async fn remove_patched_fixture(
    app: &AppHandle,
    pool: &SqlitePool,
    venue_id: i64,
    id: String,
) -> Result<(), String> {
    fixtures_db::delete_fixture(pool, &id).await?;
    refresh_artnet(app, pool, venue_id).await
}

/// Rename a patched fixture
pub async fn rename_patched_fixture(
    app: &AppHandle,
    pool: &SqlitePool,
    venue_id: i64,
    id: String,
    label: String,
) -> Result<(), String> {
    let rows = fixtures_db::update_fixture_label(pool, &id, &label).await?;
    if rows == 0 {
        return Err(format!("No fixture found to rename for id {}", id));
    }
    refresh_artnet(app, pool, venue_id).await
}

// -----------------------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------------------

fn resolve_fixtures_root(app: &AppHandle) -> Result<PathBuf, String> {
    let resource_path = app
        .path()
        .resource_dir()
        .map(|p| p.join("resources/fixtures/2511260420"))
        .unwrap_or_else(|_| PathBuf::from("resources/fixtures/2511260420"));

    if resource_path.exists() {
        return Ok(resource_path);
    }

    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let dev_path = cwd.join("../resources/fixtures/2511260420");
    if dev_path.exists() {
        return Ok(dev_path);
    }

    Ok(cwd.join("resources/fixtures/2511260420"))
}

async fn refresh_artnet(app: &AppHandle, pool: &SqlitePool, venue_id: i64) -> Result<(), String> {
    let fixtures = fixtures_db::get_patched_fixtures(pool, venue_id).await?;

    if let Some(artnet) = app.try_state::<crate::artnet::ArtNetManager>() {
        artnet.update_patch(fixtures);
    }
    Ok(())
}
