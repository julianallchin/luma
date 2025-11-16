use serde::Deserialize;
use std::path::Path;
use std::process::Command;
use tauri::AppHandle;

use crate::python_env;

const WORKER_SOURCE: &str = include_str!("../python/beat_worker.py");
const WORKER_SCRIPT_NAME: &str = "beat_worker.py";

#[derive(Debug, Clone)]
pub struct BeatAnalysis {
    pub beats: Vec<f32>,
    pub downbeats: Vec<f32>,
}

#[derive(Deserialize)]
struct WorkerResponse {
    beats: Vec<f64>,
    downbeats: Vec<f64>,
}

pub fn compute_beats(app: &AppHandle, audio_path: &Path) -> Result<BeatAnalysis, String> {
    let python_path = python_env::ensure_python_env(app)?;
    let script_path = python_env::ensure_worker_script(app, WORKER_SCRIPT_NAME, WORKER_SOURCE)?;
    let output = Command::new(&python_path)
        .env("PYTHONUNBUFFERED", "1")
        .arg(&script_path)
        .arg(audio_path)
        .output()
        .map_err(|e| format!("Failed to launch python worker: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            "Python worker exited unsuccessfully".to_string()
        } else {
            format!("Python worker failed: {}", stderr)
        });
    }

    let stdout = String::from_utf8(output.stdout)
        .map_err(|e| format!("Worker output was not valid UTF-8: {}", e))?;
    let payload: WorkerResponse = serde_json::from_str(stdout.trim())
        .map_err(|e| format!("Failed to parse worker response '{}': {}", stdout.trim(), e))?;

    Ok(BeatAnalysis {
        beats: payload
            .beats
            .into_iter()
            .map(|value| value as f32)
            .collect(),
        downbeats: payload
            .downbeats
            .into_iter()
            .map(|value| value as f32)
            .collect(),
    })
}
