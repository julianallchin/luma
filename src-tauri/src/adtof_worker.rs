//! Bridge to the ADTOF-pytorch python worker.
//!
//! Shells out to `adtof_worker.py` against a single drum-stem file and
//! returns the per-class onset timestamps. Weights ship inside the
//! `adtof-pytorch` pip package — no separate download step.

use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use tauri::AppHandle;

use crate::python_env;

const WORKER_SOURCE: &str = include_str!("../python/adtof_worker.py");
const WORKER_SCRIPT_NAME: &str = "adtof_worker.py";

/// Per-class onset timestamps, keyed by ADTOF MIDI note number
/// (35 kick / 38 snare / 47 tom / 42 hi-hat / 49 cymbal).
#[derive(Debug, Clone)]
pub struct DrumOnsets {
    pub onsets: HashMap<String, Vec<f32>>,
}

#[derive(Deserialize)]
struct WorkerResponse {
    onsets: HashMap<String, Vec<f64>>,
}

pub fn compute_drum_onsets(app: &AppHandle, drum_stem_path: &Path) -> Result<DrumOnsets, String> {
    let python_path = python_env::ensure_python_env(app)?;
    let script_path = python_env::ensure_worker_script(app, WORKER_SCRIPT_NAME, WORKER_SOURCE)?;

    let mut cmd = Command::new(&python_path);
    crate::cmd_util::no_window(&mut cmd);
    let output = cmd
        .env("PYTHONUNBUFFERED", "1")
        .arg(&script_path)
        .arg(drum_stem_path)
        .output()
        .map_err(|e| format!("Failed to launch ADTOF worker: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            "ADTOF worker exited unsuccessfully".to_string()
        } else {
            format!("ADTOF worker failed: {stderr}")
        });
    }

    let stdout = String::from_utf8(output.stdout)
        .map_err(|e| format!("ADTOF worker output was not valid UTF-8: {e}"))?;
    let payload: WorkerResponse = serde_json::from_str(stdout.trim())
        .map_err(|e| format!("Failed to parse ADTOF response '{}': {e}", stdout.trim()))?;

    let onsets = payload
        .onsets
        .into_iter()
        .map(|(k, v)| (k, v.into_iter().map(|t| t as f32).collect()))
        .collect();
    Ok(DrumOnsets { onsets })
}
