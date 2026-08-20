//! Render telemetry: an append-only JSON-lines log under the storage root.

use std::fs::{self, OpenOptions};
use std::io::Write;

use chrono::Utc;
use serde_json::{json, Value};

use crate::dispatch::{AppServices, CommandError};

const RENDER_TELEMETRY_LOG: &str = "render-telemetry.log";
const ROTATED_RENDER_TELEMETRY_LOG: &str = "render-telemetry.log.1";
const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

/// Append one schema-free entry as `{ ts, entry }`. One rotation generation is
/// kept; the log is capped at [`MAX_LOG_BYTES`].
pub async fn append_render_telemetry(
    services: &AppServices,
    entry: Value,
) -> Result<(), CommandError> {
    let root = services.storage.path();
    fs::create_dir_all(root).map_err(|e| format!("create app config dir: {e}"))?;

    let log_path = root.join(RENDER_TELEMETRY_LOG);
    if let Ok(meta) = fs::metadata(&log_path) {
        if meta.len() > MAX_LOG_BYTES {
            let rotated_path = root.join(ROTATED_RENDER_TELEMETRY_LOG);
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
