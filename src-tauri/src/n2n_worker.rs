//! Bridge to the n2n drum-onset python worker.
//!
//! Shells out to `n2n_worker.py` against a single full-mix audio file plus
//! the shared MERT cache and returns the per-class onset timestamps. The
//! vendored `n2n/` source package + its checkpoint (`weights.pt`) ship
//! inside the app bundle under `src-tauri/python/n2n/` and are copied to
//! the user's cache dir on first run via
//! [`python_env::ensure_python_resource_dir`].

use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use tauri::AppHandle;

use crate::python_env;

const WORKER_SOURCE: &str = include_str!("../python/n2n_worker.py");
const WORKER_SCRIPT_NAME: &str = "n2n_worker.py";

/// Per-class onset timestamps. Keys are the n2n 4-class names:
/// `kick`, `snare`, `hat`, `cymbal` (see `n2n/data/drum_mapping.py`).
#[derive(Debug, Clone)]
pub struct DrumOnsets {
    pub onsets: HashMap<String, Vec<f32>>,
}

#[derive(Deserialize)]
struct WorkerResponse {
    onsets: HashMap<String, Vec<f64>>,
}

pub fn compute_drum_onsets(
    app: &AppHandle,
    audio_path: &Path,
    mert_path: &Path,
) -> Result<DrumOnsets, String> {
    let python_path = python_env::ensure_python_env(app)?;
    let script_path = python_env::ensure_worker_script(app, WORKER_SCRIPT_NAME, WORKER_SOURCE)?;
    // Copy the vendored n2n package + bundled checkpoint into the cache dir
    // so `import n2n.infer` resolves and the checkpoint path is stable.
    let n2n_dir = python_env::ensure_python_resource_dir(app, "n2n")?;
    let ckpt_path = n2n_dir.join("weights.pt");
    if !ckpt_path.exists() {
        return Err(format!(
            "n2n checkpoint missing at {} — bundle resource may not have been copied",
            ckpt_path.display()
        ));
    }
    if !mert_path.exists() {
        return Err(format!(
            "MERT cache missing at {} — mert preprocessor must run first",
            mert_path.display()
        ));
    }
    let workdir = script_path
        .parent()
        .ok_or_else(|| "Worker script missing parent directory".to_string())?;

    let mut cmd = Command::new(&python_path);
    crate::cmd_util::no_window(&mut cmd);
    let output = cmd
        .env("PYTHONUNBUFFERED", "1")
        .arg(&script_path)
        .arg(audio_path)
        .arg("--ckpt")
        .arg(&ckpt_path)
        .arg("--mert")
        .arg(mert_path)
        .current_dir(workdir)
        .output()
        .map_err(|e| format!("Failed to launch n2n worker: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            "n2n worker exited unsuccessfully".to_string()
        } else {
            format!("n2n worker failed: {stderr}")
        });
    }

    let stdout = String::from_utf8(output.stdout)
        .map_err(|e| format!("n2n worker output was not valid UTF-8: {e}"))?;
    let payload: WorkerResponse = serde_json::from_str(stdout.trim())
        .map_err(|e| format!("Failed to parse n2n response '{}': {e}", stdout.trim()))?;

    let onsets = payload
        .onsets
        .into_iter()
        .map(|(k, v)| (k, v.into_iter().map(|t| t as f32).collect()))
        .collect();
    Ok(DrumOnsets { onsets })
}
