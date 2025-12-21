use std::collections::HashMap;
use std::sync::Mutex;

// Temporary in-memory storage until SQLite integration is ready
pub struct AuthState(pub Mutex<HashMap<String, String>>);

impl Default for AuthState {
    fn default() -> Self {
        Self(Mutex::new(HashMap::new()))
    }
}

#[tauri::command]
pub fn get_session_item(key: String, state: tauri::State<AuthState>) -> Option<String> {
    let store = state.0.lock().unwrap();
    store.get(&key).cloned()
}

#[tauri::command]
pub fn set_session_item(key: String, value: String, state: tauri::State<AuthState>) {
    let mut store = state.0.lock().unwrap();
    store.insert(key, value);
}

#[tauri::command]
pub fn remove_session_item(key: String, state: tauri::State<AuthState>) {
    let mut store = state.0.lock().unwrap();
    store.remove(&key);
}
