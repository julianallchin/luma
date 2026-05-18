use std::fs::{self, OpenOptions};
use std::io::Write;

use chrono::Utc;
use serde_json::{json, Value};
use tauri::Manager;

const RENDER_TELEMETRY_LOG: &str = "render-telemetry.log";
const ROTATED_RENDER_TELEMETRY_LOG: &str = "render-telemetry.log.1";
const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

#[tauri::command]
pub fn append_render_telemetry(app: tauri::AppHandle, entry: Value) -> Result<(), String> {
    let app_dir = app
        .path()
        .app_config_dir()
        .map_err(|e| format!("resolve app config dir: {e}"))?;
    fs::create_dir_all(&app_dir).map_err(|e| format!("create app config dir: {e}"))?;

    let log_path = app_dir.join(RENDER_TELEMETRY_LOG);
    if let Ok(meta) = fs::metadata(&log_path) {
        if meta.len() > MAX_LOG_BYTES {
            let rotated_path = app_dir.join(ROTATED_RENDER_TELEMETRY_LOG);
            let _ = fs::remove_file(&rotated_path);
            fs::rename(&log_path, rotated_path)
                .map_err(|e| format!("rotate telemetry log: {e}"))?;
        }
    }

    let line = json!({
        "ts": Utc::now().to_rfc3339(),
        "entry": entry,
    });
    let line = serde_json::to_string(&line).map_err(|e| format!("serialize telemetry: {e}"))?;

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .map_err(|e| format!("open telemetry log: {e}"))?;
    writeln!(file, "{line}").map_err(|e| format!("write telemetry log: {e}"))?;

    Ok(())
}
