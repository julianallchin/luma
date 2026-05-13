//! Bridge to the MERT-95M feature-extraction python worker.
//!
//! Shells out to `mert_worker.py` against a full-mix audio file *and* the
//! demucs drum stem, producing two cached .npy files in a single Python
//! process. The model is loaded once per track — consumer-laptop friendly
//! — and the resulting caches feed the bar classifier (full mix) and the
//! n2n drum-onset preprocessor (drum stem).

use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Command;
use tauri::AppHandle;

use crate::python_env;

const WORKER_SOURCE: &str = include_str!("../python/mert_worker.py");
const WORKER_SCRIPT_NAME: &str = "mert_worker.py";

/// Result of a successful MERT extraction. Both caches written by one
/// Python invocation against the same loaded MERT-95M model.
#[derive(Debug, Clone)]
pub struct MertCache {
    pub fullmix_path: PathBuf,
    pub drum_path: PathBuf,
    #[allow(dead_code)]
    pub fullmix_frames: u64,
    #[allow(dead_code)]
    pub drum_frames: u64,
}

#[derive(Deserialize)]
struct WorkerResponse {
    fullmix_path: String,
    drum_path: String,
    fullmix_frames: u64,
    drum_frames: u64,
    #[allow(dead_code)]
    frames_per_second: u32,
    #[allow(dead_code)]
    layer: u32,
    #[allow(dead_code)]
    model_id: String,
}

pub fn compute_mert_cache(
    app: &AppHandle,
    fullmix_path: &Path,
    drum_path: &Path,
    out_fullmix: &Path,
    out_drum: &Path,
) -> Result<MertCache, String> {
    let python_path = python_env::ensure_python_env(app)?;
    let script_path = python_env::ensure_worker_script(app, WORKER_SCRIPT_NAME, WORKER_SOURCE)?;
    // The worker imports `n2n.infer.compute_mert_features` so it shares the
    // exact chunking parameters the training pipeline uses; ensure the n2n
    // resource dir is unpacked alongside the script and run with workdir =
    // script's parent so the import resolves.
    let _ = python_env::ensure_python_resource_dir(app, "n2n")?;
    let workdir = script_path
        .parent()
        .ok_or_else(|| "Worker script missing parent directory".to_string())?;

    for out in [out_fullmix, out_drum] {
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                format!("Failed to create MERT cache dir {}: {e}", parent.display())
            })?;
        }
    }

    let mut cmd = Command::new(&python_path);
    crate::cmd_util::no_window(&mut cmd);
    let output = cmd
        .env("PYTHONUNBUFFERED", "1")
        .arg(&script_path)
        .arg("--fullmix")
        .arg(fullmix_path)
        .arg("--drum")
        .arg(drum_path)
        .arg("--out-fullmix")
        .arg(out_fullmix)
        .arg("--out-drum")
        .arg(out_drum)
        .current_dir(workdir)
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
        fullmix_path: PathBuf::from(payload.fullmix_path),
        drum_path: PathBuf::from(payload.drum_path),
        fullmix_frames: payload.fullmix_frames,
        drum_frames: payload.drum_frames,
    })
}
