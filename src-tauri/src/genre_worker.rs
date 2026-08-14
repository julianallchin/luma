//! Bridge to the per-bar genre python worker (Discogs-EffNet via ONNX).
//!
//! ⚠ The model weights are **CC BY-NC-ND 4.0** (Essentia / MTG-UPF model zoo)
//! and are deliberately *not* bundled, downloaded, or committed: no
//! `include_bytes!` here, unlike [`crate::classifier_worker`]. The user places
//! `discogs-effnet-bsdynamic-1.onnx` in `<app config>/models/` themselves. See
//! `docs/genre-model.md` and the worker script's header for the licensing
//! situation before shipping this in a commercial build.
//!
//! Bar boundaries go in on stdin as `[[start, end], ...]` — same shape and
//! reason as [`crate::classifier_worker`] (argv can't carry a few hundred bars
//! on every platform). The worker answers with the payload below on stdout.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};
use tauri::AppHandle;

use crate::python_env;
use crate::storage::StorageRoot;

const WORKER_SOURCE: &str = include_str!("../python/genre_worker.py");
const WORKER_SCRIPT_NAME: &str = "genre_worker.py";

/// File the user must provide under [`StorageRoot::models_dir`].
pub const MODEL_FILE_NAME: &str = "discogs-effnet-bsdynamic-1.onnx";

/// One `(label_index, probability)` pair. `label_index` indexes into
/// [`GenreAnalysis::labels`], **not** the 400-entry Discogs taxonomy — the
/// worker compacts the taxonomy down to the styles a track actually uses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenreScore(pub usize, pub f64);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BarGenres {
    pub bar_idx: u32,
    pub start: f64,
    pub end: f64,
    /// Sparse confidence-descending top-K for this bar.
    pub top: Vec<GenreScore>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenreAnalysis {
    /// Style names present anywhere in this track, in taxonomy order.
    pub labels: Vec<String>,
    pub bars: Vec<BarGenres>,
    /// Whole-track top-10 from the unsmoothed patch mean.
    pub track_top: Vec<GenreScore>,
}

pub fn analyze_genres(
    app: &AppHandle,
    storage: &StorageRoot,
    audio_path: &Path,
    bar_boundaries: &[(f64, f64)],
) -> Result<GenreAnalysis, String> {
    let models_dir = storage.models_dir();
    let model_path = models_dir.join(MODEL_FILE_NAME);
    if !model_path.exists() {
        // Fail here rather than letting python discover it, so the message the
        // user sees in `preprocessing_failures` names the exact file and place.
        return Err(format!(
            "Genre model not installed. Download “Discogs-Effnet” (ONNX, dynamic \
             batch — the file is named {MODEL_FILE_NAME}, ~18 MB) from \
             https://essentia.upf.edu/models.html and place it at {}. Note the \
             Essentia model zoo is licensed CC BY-NC-ND 4.0 (non-commercial, \
             no derivatives).",
            model_path.display()
        ));
    }

    let python_path = python_env::ensure_python_env(app)?;
    let script_path = python_env::ensure_worker_script(app, WORKER_SCRIPT_NAME, WORKER_SOURCE)?;
    let boundaries_json = serde_json::to_vec(bar_boundaries)
        .map_err(|e| format!("Failed to encode bar boundaries: {e}"))?;

    let mut cmd = Command::new(&python_path);
    crate::cmd_util::no_window(&mut cmd);
    let mut child = cmd
        .env("PYTHONUNBUFFERED", "1")
        .env("LUMA_MODELS_DIR", &models_dir)
        .env("LUMA_FFMPEG", crate::ffmpeg_env::ffmpeg_path())
        .arg(&script_path)
        .arg(audio_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to launch genre worker: {e}"))?;
    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| "Failed to open genre worker stdin".to_string())?;
        stdin
            .write_all(&boundaries_json)
            .map_err(|e| format!("Failed to write bar boundaries to worker stdin: {e}"))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|e| format!("Failed to wait on genre worker: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            "Genre worker exited unsuccessfully".to_string()
        } else {
            format!("Genre worker failed: {stderr}")
        });
    }

    let stdout = String::from_utf8(output.stdout)
        .map_err(|e| format!("Genre worker output was not valid UTF-8: {e}"))?;
    serde_json::from_str(stdout.trim()).map_err(|e| {
        format!(
            "Failed to parse genre worker output '{}': {e}",
            stdout.trim()
        )
    })
}
