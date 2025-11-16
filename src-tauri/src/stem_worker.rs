use serde::Deserialize;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use tauri::AppHandle;

use crate::python_env;

const WORKER_SOURCE: &str = include_str!("../python/audio_preprocessor.py");
const WORKER_SCRIPT_NAME: &str = "audio_preprocessor.py";
const DEMUCS_MODEL: &str = "htdemucs";

#[derive(Debug, Clone)]
pub struct StemFile {
    pub name: String,
    pub path: PathBuf,
}

#[derive(Deserialize)]
struct StemEntry {
    name: String,
    path: String,
}

#[derive(Deserialize)]
struct WorkerResponse {
    stems: Vec<StemEntry>,
}

pub fn separate_stems(
    app: &AppHandle,
    audio_path: &Path,
    target_dir: &Path,
) -> Result<Vec<StemFile>, String> {
    let python_path = python_env::ensure_python_env(app)?;
    let script_path = python_env::ensure_worker_script(app, WORKER_SCRIPT_NAME, WORKER_SOURCE)?;

    let mut child = Command::new(&python_path)
        .env("PYTHONUNBUFFERED", "1")
        .arg(&script_path)
        .arg(audio_path)
        .arg(target_dir)
        .arg("--model")
        .arg(DEMUCS_MODEL)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to launch stem worker: {}", e))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Stem worker stdout not captured".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "Stem worker stderr not captured".to_string())?;

    let stderr_lines = Arc::new(Mutex::new(Vec::new()));
    let stderr_lines_thread = Arc::clone(&stderr_lines);
    let stderr_handle = std::thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines().flatten() {
            log_stem_line(&line);
            stderr_lines_thread.lock().unwrap().push(line);
        }
    });

    let mut stdout_buf = String::new();
    {
        let mut reader = BufReader::new(stdout);
        reader
            .read_to_string(&mut stdout_buf)
            .map_err(|e| format!("Failed to read stem worker stdout: {}", e))?;
    }

    let status = child
        .wait()
        .map_err(|e| format!("Failed to wait for stem worker: {}", e))?;
    stderr_handle
        .join()
        .map_err(|_| "Failed to join stem stderr reader".to_string())?;

    let stderr_vec = match Arc::try_unwrap(stderr_lines) {
        Ok(mutex) => mutex.into_inner().unwrap_or_default(),
        Err(arc) => {
            let guard = arc
                .lock()
                .map_err(|_| "Failed to lock stderr buffer".to_string())?;
            guard.clone()
        }
    };
    let stderr_text = stderr_vec.join("\n");

    if !status.success() {
        return Err(format!(
            "Stem worker failed (code {}): {}",
            status, stderr_text
        ));
    }

    if !stderr_text.is_empty() {
        log_stem_line(&format!("stem worker stderr: {}", stderr_text));
    }

    let stdout = stdout_buf.trim();
    let response: WorkerResponse = serde_json::from_str(stdout)
        .map_err(|e| format!("Failed to parse stem worker response '{}': {}", stdout, e))?;

    if response.stems.is_empty() {
        return Err("Stem worker reported no stems".to_string());
    }

    Ok(response
        .stems
        .into_iter()
        .map(|entry| StemFile {
            name: entry.name,
            path: PathBuf::from(entry.path),
        })
        .collect())
}

fn log_stem_line(message: &str) {
    eprintln!("[stem_worker] {}", message);
}
