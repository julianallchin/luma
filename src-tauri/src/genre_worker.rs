//! Bridge to the per-bar genre python worker (Discogs-EffNet via ONNX).
//!
//! ⚠ The model weights are **CC BY-NC-ND 4.0** (Essentia / MTG-UPF model zoo)
//! and are *not* bundled or committed: no `include_bytes!` here, unlike
//! [`crate::classifier_worker`]. They are downloaded on first use straight from
//! MTG's own server into `<app config>/models/` (checksum-pinned), so Luma
//! never redistributes them — fine while Luma is internal, but a commercially
//! distributed build still needs MTG's proprietary license. See
//! `docs/genre-model.md`.
//!
//! Bar boundaries go in on stdin as `[[start, end], ...]` — same shape and
//! reason as [`crate::classifier_worker`] (argv can't carry a few hundred bars
//! on every platform). The worker answers with the payload below on stdout.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

use sha2::{Digest, Sha256};

use crate::preprocessing::WorkerEnvironment;
use crate::storage::StorageRoot;
use serde::{Deserialize, Serialize};

const WORKER_SOURCE: &str = include_str!("../python/genre_worker.py");
const WORKER_SCRIPT_NAME: &str = "genre_worker.py";

/// Model file under [`StorageRoot::models_dir`], fetched by [`ensure_model`].
pub const MODEL_FILE_NAME: &str = "discogs-effnet-bsdynamic-1.onnx";

/// MTG's own hosting of the ONNX conversion — downloading from the source
/// means Luma never redistributes the CC BY-NC-ND file.
const MODEL_URL: &str =
    "https://essentia.upf.edu/models/music-style-classification/discogs-effnet/discogs-effnet-bsdynamic-1.onnx";

/// SHA-256 of the file at [`MODEL_URL`], pinned 2026-08-13. If MTG ever
/// republishes the checkpoint this fails closed — bump deliberately, since the
/// label order the worker asserts is only known to match *this* file.
const MODEL_SHA256: &str = "a280825b334797cf677939db8cd5762c0392aedd0ca6415dbc1cd083f045e43c";

/// Serializes the download so parallel genre runs don't race on the same file.
static MODEL_DOWNLOAD: Mutex<()> = Mutex::new(());

/// Return the model path, downloading it on first use. Download goes to a
/// `.part` sibling, is checksum-verified, then atomically renamed — a crashed
/// or corrupt download can never be mistaken for the model.
fn ensure_model(models_dir: &Path) -> Result<PathBuf, String> {
    let model_path = models_dir.join(MODEL_FILE_NAME);
    if model_path.exists() {
        return Ok(model_path);
    }
    let _guard = MODEL_DOWNLOAD
        .lock()
        .map_err(|_| "Genre model download lock poisoned by an earlier panic".to_string())?;
    if model_path.exists() {
        return Ok(model_path); // another track got here first
    }

    std::fs::create_dir_all(models_dir)
        .map_err(|e| format!("Failed to create {}: {e}", models_dir.display()))?;

    let context = |e: String| {
        format!(
            "Failed to fetch the genre model from {MODEL_URL}: {e}. \
             You can install it manually at {} (~18 MB). Note the Essentia \
             model zoo is CC BY-NC-ND 4.0 (non-commercial, no derivatives).",
            model_path.display()
        )
    };

    let bytes = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .map_err(|e| context(e.to_string()))?
        .get(MODEL_URL)
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.bytes())
        .map_err(|e| context(e.to_string()))?;

    let digest = format!("{:x}", Sha256::digest(&bytes));
    if digest != MODEL_SHA256 {
        return Err(context(format!(
            "checksum mismatch (got {digest}, expected {MODEL_SHA256}) — \
             MTG may have republished the checkpoint; verify and bump MODEL_SHA256"
        )));
    }

    let part_path = model_path.with_extension("onnx.part");
    std::fs::write(&part_path, &bytes).map_err(|e| context(e.to_string()))?;
    std::fs::rename(&part_path, &model_path).map_err(|e| context(e.to_string()))?;
    Ok(model_path)
}

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
    env: &WorkerEnvironment,
    storage: &StorageRoot,
    audio_path: &Path,
    bar_boundaries: &[(f64, f64)],
) -> Result<GenreAnalysis, String> {
    let models_dir = storage.models_dir();
    ensure_model(&models_dir)?;

    let python_path = env.python()?;
    let script_path = env.deploy_script(WORKER_SCRIPT_NAME, WORKER_SOURCE)?;
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
