//! Bridge to the joint bar classifier python worker.
//!
//! Bundles `bar_window_classifier.pt` via `include_bytes!` (~13 MB) and
//! writes it into the app cache on first use (mirrors the python script via
//! `ensure_worker_script`). Bar boundaries are passed via a temp JSON file.
//!
//! Also bundles `tag_thresholds.json`: F1-optimal per-tag thresholds from the
//! model's training-time LOTO sweep, read through [`bundled_thresholds`].
//!
//! Output: parsed [`BarClassification`] list, one per scored bar — see the
//! python worker's module docstring for the canonical shape. What the
//! database stores is the compact [`StoredBarClassifications`].

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use crate::preprocessing::WorkerEnvironment;
use serde::{Deserialize, Serialize};

const WORKER_SOURCE: &str = include_str!("../python/classifier_worker.py");
const WORKER_SCRIPT_NAME: &str = "classifier_worker.py";

const BUNDLED_WEIGHTS: &[u8] = include_bytes!("../python/classifier/bar_window_classifier.pt");
const WEIGHTS_FILE_NAME: &str = "bar_window_classifier.pt";

const BUNDLED_THRESHOLDS: &str = include_str!("../python/classifier/tag_thresholds.json");

/// Bundled per-tag suggestion thresholds (raw JSON from the training-time
/// LOTO sweep), used in place of a flat 0.5 cutoff.
pub fn bundled_thresholds() -> &'static str {
    BUNDLED_THRESHOLDS
}

#[derive(Debug, Clone, Deserialize)]
pub struct BarClassification {
    pub bar_idx: usize,
    /// `intensity` (continuous, clipped 0..5) plus per-tag sigmoid
    /// probabilities.
    pub predictions: HashMap<String, f64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClassifierAnalysis {
    pub tag_order: Vec<String>,
    pub bars: Vec<BarClassification>,
}

/// `track_bar_classifications.classifications_json`. Index `i` of `intensity`
/// and `scores` is bar `i` of `workers::build_bar_boundaries`, which is also
/// where a bar's start and end come from. A bar the worker could not score is
/// null in both. Values are rounded to 3 decimals: the row syncs, and more
/// digits carry no information.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredBarClassifications {
    /// `[start, end]` of bar 0 of the grid the classifier ran against, so
    /// `list_pending_bar_aligned` can tell when the grid has moved since.
    pub first_bar: (f64, f64),
    /// The continuous intensity head, per bar.
    pub intensity: Vec<Option<f64>>,
    /// Per-tag probabilities, per bar, in the row's `tag_order_json` order.
    pub scores: Vec<Option<Vec<f64>>>,
}

impl StoredBarClassifications {
    pub fn new(analysis: &ClassifierAnalysis, first_bar: (f64, f64)) -> Result<Self, String> {
        let n_bars = analysis.bars.iter().map(|bar| bar.bar_idx + 1).max();
        let n_bars = n_bars.unwrap_or(0);
        let mut stored = Self {
            first_bar,
            intensity: vec![None; n_bars],
            scores: vec![None; n_bars],
        };
        for bar in &analysis.bars {
            let score = |key: &str| {
                bar.predictions.get(key).map(|v| round3(*v)).ok_or_else(|| {
                    format!("classifier output for bar {} lacks '{key}'", bar.bar_idx)
                })
            };
            stored.intensity[bar.bar_idx] = Some(score("intensity")?);
            stored.scores[bar.bar_idx] = Some(
                analysis
                    .tag_order
                    .iter()
                    .map(|tag| score(tag))
                    .collect::<Result<_, _>>()?,
            );
        }
        Ok(stored)
    }
}

fn round3(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

/// Write the bundled bar-classifier weights into the app cache once and
/// return the on-disk path. Refreshes the file if its size differs from the
/// embedded bytes (defensive — covers a stale truncated write).
fn ensure_weights_file(cache_dir: &Path) -> Result<PathBuf, String> {
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
    env: &WorkerEnvironment,
    mert_path: &Path,
    bar_boundaries: &[(f64, f64)],
) -> Result<ClassifierAnalysis, String> {
    let weights_path = ensure_weights_file(env.cache_dir())?;

    if !mert_path.exists() {
        return Err(format!(
            "MERT cache missing at {} — mert preprocessor must run first",
            mert_path.display()
        ));
    }

    let boundaries_json = serde_json::to_vec(bar_boundaries)
        .map_err(|e| format!("Failed to encode bar boundaries: {e}"))?;

    let mut cmd = env.worker_command(WORKER_SCRIPT_NAME, WORKER_SOURCE)?;
    let mut child = cmd
        .arg(mert_path)
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
