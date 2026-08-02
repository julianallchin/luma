//! Business logic for fixture operations.
//!
//! Database layer handles CRUD only. File/resource access, ArtNet refresh, and
//! in-memory fixture index live here.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tauri::{AppHandle, Manager};

use crate::database::local::fixtures as fixtures_db;
use crate::database::local::venue_access::AuthorizedVenue;
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

/// Get all patched fixtures for a venue
pub async fn get_patched_fixtures(
    access: &mut impl AuthorizedVenue,
) -> Result<Vec<PatchedFixture>, String> {
    fixtures_db::get_patched_fixtures(access).await
}

/// Get patch hierarchy for a venue
pub async fn get_patch_hierarchy(
    app: &AppHandle,
    access: &mut impl AuthorizedVenue,
) -> Result<Vec<FixtureNode>, String> {
    let fixtures = fixtures_db::get_patched_fixtures(access).await?;
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

// -----------------------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------------------

pub fn resolve_fixtures_root(app: &AppHandle) -> Result<PathBuf, String> {
    resolve_fixtures_root_from(app.path().resource_dir().ok().as_deref())
}

/// [`resolve_fixtures_root`] without an `AppHandle`. Headless binaries pass
/// `None` (or an explicit resource dir) and get the identical search order.
pub fn resolve_fixtures_root_from(resource_dir: Option<&Path>) -> Result<PathBuf, String> {
    // In debug builds, prefer the source directory so newly added fixture files
    // are picked up immediately without needing a full Tauri resource re-bundle.
    #[cfg(debug_assertions)]
    {
        let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
        let dev_path = cwd.join("../resources/fixtures/2511260420");
        if dev_path.exists() {
            return Ok(dev_path);
        }
    }

    if let Some(resource_dir) = resource_dir {
        // Bundled app: "../resources/fixtures" maps to "_up_/resources/fixtures"
        let bundled = resource_dir.join("_up_/resources/fixtures/2511260420");
        if bundled.exists() {
            return Ok(bundled);
        }
    }

    // Dev fallback: CWD is src-tauri, fixtures are at ../resources/fixtures
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let dev_path = cwd.join("../resources/fixtures/2511260420");
    if dev_path.exists() {
        return Ok(dev_path);
    }

    Err("Could not find fixtures directory".to_string())
}

pub fn update_artnet_patch(app: &AppHandle, fixtures: Vec<PatchedFixture>) {
    if let Some(artnet) = app.try_state::<crate::artnet::ArtNetManager>() {
        artnet.update_patch(fixtures);
    }
}
