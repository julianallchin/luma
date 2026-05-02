//! Bridge to the joint bar classifier python worker.
//!
//! Bundles `bar_classifier.pt` via `include_bytes!` (~1 MB) and writes it
//! into the app cache on first use (mirrors the python script via
//! `ensure_worker_script`). Bar boundaries are passed via a temp JSON file.
//!
//! Output: parsed [`BarClassification`] list, one per scored bar. The
//! schema is intentionally text-LLM-friendly — see the python worker's
//! module docstring for the canonical shape.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::python_env;

const WORKER_SOURCE: &str = include_str!("../python/classifier_worker.py");
const WORKER_SCRIPT_NAME: &str = "classifier_worker.py";

const BUNDLED_WEIGHTS: &[u8] = include_bytes!("../python/classifier/bar_classifier.pt");
const WEIGHTS_FILE_NAME: &str = "bar_classifier.pt";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BarClassification {
    pub bar_idx: u32,
    pub start: f64,
    pub end: f64,
    /// `intensity` (continuous, clipped 0..5) plus per-tag sigmoid
    /// probabilities. BTreeMap → stable JSON ordering downstream.
    pub predictions: BTreeMap<String, f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassifierAnalysis {
    pub tag_order: Vec<String>,
    pub bars: Vec<BarClassification>,
}

/// Write the bundled bar-classifier weights into the app cache once and
/// return the on-disk path. Refreshes the file if its size differs from the
/// embedded bytes (defensive — covers a stale truncated write).
fn ensure_weights_file(app: &AppHandle) -> Result<PathBuf, String> {
    let cache_dir = app
        .path()
        .app_cache_dir()
        .map_err(|e| format!("Failed to locate cache dir: {e}"))?;
    fs::create_dir_all(&cache_dir)
        .map_err(|e| format!("Failed to create cache dir {}: {e}", cache_dir.display()))?;
    let weights_path = cache_dir.join(WEIGHTS_FILE_NAME);

    let needs_write = match fs::metadata(&weights_path) {
        Ok(meta) => meta.len() as usize != BUNDLED_WEIGHTS.len(),
        Err(_) => true,
    };
    if needs_write {
        fs::write(&weights_path, BUNDLED_WEIGHTS).map_err(|e| {
            format!(
                "Failed to write classifier weights to {}: {e}",
                weights_path.display()
            )
        })?;
    }
    Ok(weights_path)
}

pub fn classify_bars(
    app: &AppHandle,
    audio_path: &Path,
    bar_boundaries: &[(f64, f64)],
) -> Result<ClassifierAnalysis, String> {
    let python_path = python_env::ensure_python_env(app)?;
    let script_path = python_env::ensure_worker_script(app, WORKER_SCRIPT_NAME, WORKER_SOURCE)?;
    let weights_path = ensure_weights_file(app)?;

    let boundaries_json = serde_json::to_vec(bar_boundaries)
        .map_err(|e| format!("Failed to encode bar boundaries: {e}"))?;

    let mut cmd = Command::new(&python_path);
    crate::cmd_util::no_window(&mut cmd);
    let mut child = cmd
        .env("PYTHONUNBUFFERED", "1")
        .arg(&script_path)
        .arg(audio_path)
        .arg(&weights_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to launch classifier worker: {e}"))?;
    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| "Failed to open classifier worker stdin".to_string())?;
        stdin
            .write_all(&boundaries_json)
            .map_err(|e| format!("Failed to write bar boundaries to worker stdin: {e}"))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|e| format!("Failed to wait on classifier worker: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            "Classifier worker exited unsuccessfully".to_string()
        } else {
            format!("Classifier worker failed: {stderr}")
        });
    }

    let stdout = String::from_utf8(output.stdout)
        .map_err(|e| format!("Classifier worker output was not valid UTF-8: {e}"))?;
    let analysis: ClassifierAnalysis = serde_json::from_str(stdout.trim())
        .map_err(|e| format!("Failed to parse classifier output '{}': {e}", stdout.trim()))?;
    Ok(analysis)
}

/// SHA-256 of the bundled weight bytes — useful for an integrity assertion
/// in tests and for logging which weights are deployed.
#[cfg(test)]
pub fn bundled_weights_len() -> usize {
    BUNDLED_WEIGHTS.len()
}
