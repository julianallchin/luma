use crate::models::tracks::ChordSection;
use crate::preprocessing::WorkerEnvironment;
use serde::Deserialize;
use std::path::PathBuf;

const WORKER_SOURCE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/python/ace_chord_sections_worker.py"
));
const WORKER_SCRIPT_NAME: &str = "ace_chord_sections_worker.py";

#[derive(Debug, Clone)]
pub struct RootAnalysis {
    pub sections: Vec<ChordSection>,
    pub logits_path: Option<String>,
}

#[derive(Deserialize)]
struct WorkerResponse {
    sections: Vec<WorkerSection>,
    logits_path: Option<String>,
}

#[derive(Deserialize)]
struct WorkerSection {
    start: f64,
    end: f64,
    label: String,
}

pub fn compute_roots(
    env: &WorkerEnvironment,
    audio_paths: &[PathBuf],
) -> Result<RootAnalysis, String> {
    // Copy bundled consonance-ACE repo alongside the worker so imports like `ACE.*` resolve.
    let resource_dir = env.deploy_resource("consonance-ACE")?;
    if !resource_dir.exists() {
        return Err(format!(
            "consonance-ACE resource dir missing after copy at {}",
            resource_dir.display()
        ));
    }

    let mut cmd = env.worker_command(WORKER_SCRIPT_NAME, WORKER_SOURCE)?;
    cmd.args(audio_paths).arg("--save-logits");

    let output = cmd
        .output()
        .map_err(|e| format!("Failed to launch python worker: {}", e))?;

    if !output.status.success() {
        return Err(crate::preprocessing::workers::failed(
            "Python worker",
            &String::from_utf8_lossy(&output.stderr),
        ));
    }

    let stdout = String::from_utf8(output.stdout)
        .map_err(|e| format!("Worker output was not valid UTF-8: {}", e))?;
    let payload: WorkerResponse = serde_json::from_str(stdout.trim())
        .map_err(|e| format!("Failed to parse worker response '{}': {}", stdout.trim(), e))?;

    let mut sections = Vec::new();
    for sec in payload.sections {
        sections.push(ChordSection {
            start_s: sec.start as f32,
            end_s: sec.end as f32,
            root_pitch_class: parse_root_from_label(&sec.label),
            label: Some(sec.label),
        });
    }

    Ok(RootAnalysis {
        sections,
        logits_path: payload.logits_path,
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
