use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Command;
use tauri::AppHandle;

use crate::python_env;

const WORKER_SOURCE: &str = include_str!("../python/ace_chord_sections_worker.py");
const WORKER_SCRIPT_NAME: &str = "ace_chord_sections_worker.py";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChordSection {
    pub start: f32,
    pub end: f32,
    pub root: Option<u8>,
    pub label: String,
}

#[derive(Debug, Clone)]
pub struct RootAnalysis {
    pub frame_hop_seconds: f32,
    pub sections: Vec<ChordSection>,
}

#[derive(Deserialize)]
struct WorkerResponse {
    frame_hop_seconds: Option<f64>,
    sections: Vec<WorkerSection>,
}

#[derive(Deserialize)]
struct WorkerSection {
    start: f64,
    end: f64,
    label: String,
}

pub fn compute_roots(app: &AppHandle, audio_path: &Path) -> Result<RootAnalysis, String> {
    let python_path = python_env::ensure_python_env(app)?;
    let script_path = python_env::ensure_worker_script(app, WORKER_SCRIPT_NAME, WORKER_SOURCE)?;
    // Copy bundled consonance-ACE repo alongside the worker so imports like `ACE.*` resolve.
    let resource_dir = python_env::ensure_python_resource_dir(app, "consonance-ACE")?;
    let workdir = script_path
        .parent()
        .ok_or_else(|| "Worker script missing parent directory".to_string())?;
    if !resource_dir.exists() {
        return Err(format!(
            "consonance-ACE resource dir missing after copy at {}",
            resource_dir.display()
        ));
    }

    let mut cmd = Command::new(&python_path);
    cmd.env("PYTHONUNBUFFERED", "1")
        .arg(&script_path)
        .arg(audio_path)
        .current_dir(workdir);

    let output = cmd
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

    let frame_hop_seconds = payload.frame_hop_seconds.map(|v| v as f32).unwrap_or(0.0);

    let mut sections = Vec::new();
    for sec in payload.sections {
        sections.push(ChordSection {
            start: sec.start as f32,
            end: sec.end as f32,
            root: parse_root_from_label(&sec.label),
            label: sec.label,
        });
    }

    Ok(RootAnalysis {
        frame_hop_seconds,
        sections,
    })
}

fn parse_root_from_label(label: &str) -> Option<u8> {
    // Expect labels like "C:maj", "G:min", "N".
    let root_str = label.split(':').next().unwrap_or("").trim();
    match root_str {
        "C" => Some(0),
        "C#" | "Db" => Some(1),
        "D" => Some(2),
        "D#" | "Eb" => Some(3),
        "E" => Some(4),
        "F" => Some(5),
        "F#" | "Gb" => Some(6),
        "G" => Some(7),
        "G#" | "Ab" => Some(8),
        "A" => Some(9),
        "A#" | "Bb" => Some(10),
        "B" => Some(11),
        "N" | "" => None,
        _ => None,
    }
}
