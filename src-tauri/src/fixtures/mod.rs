pub mod models;
mod parser;

use tauri::{AppHandle, Manager, State, command};
use std::path::PathBuf;
use std::sync::Mutex;
use self::models::{FixtureDefinition, FixtureEntry};
use self::parser::FixtureIndex;

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
        std::env::current_dir().map_err(|e| e.to_string())?.join("resources/fixtures/2511260420")
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
pub fn search_fixtures(query: String, state: State<'_, FixtureState>) -> Result<Vec<FixtureEntry>, String> {
    let state_guard = state.0.lock().unwrap();
    
    let index = state_guard.as_ref().ok_or("Fixtures not initialized. Call initialize_fixtures first.")?;
    
    let query = query.to_lowercase();
    if query.is_empty() {
         // Return first 50 if empty
         return Ok(index.entries.iter().take(50).cloned().collect());
    }

    let results: Vec<FixtureEntry> = index.entries.iter()
        .filter(|f| f.manufacturer.to_lowercase().contains(&query) || f.model.to_lowercase().contains(&query))
        .take(50)
        .cloned()
        .collect();
    
    Ok(results)
}

#[command]
pub fn get_fixture_definition(app: AppHandle, path: String) -> Result<FixtureDefinition, String> {
    let resource_path = app.path().resource_dir()
        .map(|p| p.join("resources/fixtures/2511260420"))
        .unwrap_or_else(|_| PathBuf::from("resources/fixtures/2511260420"));

     let final_path = if resource_path.exists() {
        resource_path
    } else {
        std::env::current_dir().map_err(|e| e.to_string())?.join("resources/fixtures/2511260420")
    };

    let full_path = final_path.join(path);
        
    parser::parse_definition(&full_path).map_err(|e| e.to_string())
}
