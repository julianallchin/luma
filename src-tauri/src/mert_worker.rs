//! Bridge to the MERT-95M feature-extraction python worker.
//!
//! Shells out to `mert_worker.py` against a single audio file and returns
//! the path of the cached .npy features. Used by both the bar classifier and
//! the n2n drum-onset preprocessor — running MERT once per track and
//! consuming the same cache from both downstream consumers.

use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Command;
use tauri::AppHandle;

use crate::python_env;

const WORKER_SOURCE: &str = include_str!("../python/mert_worker.py");
const WORKER_SCRIPT_NAME: &str = "mert_worker.py";

/// Result of a successful MERT extraction.
#[derive(Debug, Clone)]
pub struct MertCache {
    pub path: PathBuf,
    #[allow(dead_code)]
    pub n_frames: u64,
}

#[derive(Deserialize)]
struct WorkerResponse {
    path: String,
    n_frames: u64,
    #[allow(dead_code)]
    frames_per_second: u32,
    #[allow(dead_code)]
    layer: u32,
    #[allow(dead_code)]
    model_id: String,
}

pub fn compute_mert_cache(
    app: &AppHandle,
    audio_path: &Path,
    out_path: &Path,
) -> Result<MertCache, String> {
    let python_path = python_env::ensure_python_env(app)?;
    let script_path = python_env::ensure_worker_script(app, WORKER_SCRIPT_NAME, WORKER_SOURCE)?;

    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create MERT cache dir {}: {e}", parent.display()))?;
    }

    let mut cmd = Command::new(&python_path);
    crate::cmd_util::no_window(&mut cmd);
    let output = cmd
        .env("PYTHONUNBUFFERED", "1")
        .arg(&script_path)
        .arg(audio_path)
        .arg("--out")
        .arg(out_path)
        .output()
        .map_err(|e| format!("Failed to launch MERT worker: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            "MERT worker exited unsuccessfully".to_string()
        } else {
            format!("MERT worker failed: {stderr}")
        });
    }

    let stdout = String::from_utf8(output.stdout)
        .map_err(|e| format!("MERT worker output was not valid UTF-8: {e}"))?;
    let payload: WorkerResponse = serde_json::from_str(stdout.trim())
        .map_err(|e| format!("Failed to parse MERT response '{}': {e}", stdout.trim()))?;

    Ok(MertCache {
        path: PathBuf::from(payload.path),
        n_frames: payload.n_frames,
    })
}
