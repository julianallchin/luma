use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use tauri::{AppHandle, Manager};

const WORKER_SOURCE: &str = include_str!("../python/beat_worker.py");
const REQUIREMENTS_TEXT: &str = include_str!("../python/requirements.txt");
const PY_MIN_VERSION: (u32, u32) = (3, 10);
const PY_MAX_VERSION_EXCLUSIVE: (u32, u32) = (3, 14);

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
    let python_path = ensure_python_env(app)?;
    let script_path = ensure_worker_script(app)?;
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

fn ensure_worker_script(app: &AppHandle) -> Result<PathBuf, String> {
    let cache_dir = app
        .path()
        .app_cache_dir()
        .map_err(|e| format!("Failed to locate cache dir: {}", e))?;
    std::fs::create_dir_all(&cache_dir)
        .map_err(|e| format!("Failed to create cache dir {}: {}", cache_dir.display(), e))?;

    let script_path = cache_dir.join("beat_worker.py");
    std::fs::write(&script_path, WORKER_SOURCE).map_err(|e| {
        format!(
            "Failed to write python worker {}: {}",
            script_path.display(),
            e
        )
    })?;
    Ok(script_path)
}

fn ensure_python_env(app: &AppHandle) -> Result<PathBuf, String> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let guard = LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
    let result = ensure_python_env_inner(app);
    drop(guard);
    result
}

fn ensure_python_env_inner(app: &AppHandle) -> Result<PathBuf, String> {
    let cache_dir = app
        .path()
        .app_cache_dir()
        .map_err(|e| format!("Failed to locate cache dir: {}", e))?;
    let env_dir = cache_dir.join("python-env");

    let mut python_path = find_venv_python(&env_dir);

    if let Some(path) = &python_path {
        let supported = match interpreter_supported(path) {
            Ok(value) => value,
            Err(err) => {
                eprintln!(
                    "[python-worker] failed to inspect env python {}: {}",
                    path.display(),
                    err
                );
                false
            }
        };
        if !supported {
            if let Err(err) = fs::remove_dir_all(&env_dir) {
                eprintln!(
                    "[python-worker] failed to remove incompatible env {}: {}",
                    env_dir.display(),
                    err
                );
            }
            python_path = None;
        }
    }

    if python_path.is_none() {
        create_virtual_env(&env_dir)?;
        python_path = find_venv_python(&env_dir);
    }

    let python_path = python_path.ok_or_else(|| {
        format!(
            "Virtual environment at {} missing python binary",
            env_dir.display()
        )
    })?;

    install_requirements(&python_path, &env_dir)?;
    Ok(python_path)
}

fn find_venv_python(env_dir: &Path) -> Option<PathBuf> {
    #[cfg(windows)]
    let candidates = [
        env_dir.join("Scripts").join("python.exe"),
        env_dir.join("Scripts").join("python"),
    ];
    #[cfg(not(windows))]
    let candidates = [
        env_dir.join("bin").join("python3"),
        env_dir.join("bin").join("python"),
    ];

    candidates.into_iter().find(|path| path.exists())
}

fn create_virtual_env(env_dir: &Path) -> Result<(), String> {
    if let Some(parent) = env_dir.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            format!(
                "Failed to create python env parent {}: {}",
                parent.display(),
                e
            )
        })?;
    }

    let system_python = resolve_system_python()
        .ok_or_else(|| "Unable to locate supported python interpreter".to_string())?;

    let output = Command::new(&system_python)
        .args(["-m", "venv"])
        .arg(env_dir)
        .output()
        .map_err(|e| format!("Failed to create virtualenv: {}", e))?;

    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "python -m venv failed:\nstdout: {}\nstderr: {}",
            stdout, stderr
        ));
    }

    Ok(())
}

fn install_requirements(python_path: &Path, env_dir: &Path) -> Result<(), String> {
    let hash = requirements_hash();
    let marker = env_dir.join(".requirements.hash");
    if let Ok(existing) = fs::read_to_string(&marker) {
        if existing.trim() == hash {
            if python_path.exists() {
                return Ok(());
            }
        }
    }

    if let Some(parent) = marker.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to prepare env dir {}: {}", parent.display(), e))?;
    }

    let requirements_path = env_dir.join("requirements.txt");
    fs::write(&requirements_path, REQUIREMENTS_TEXT)
        .map_err(|e| format!("Failed to write requirements: {}", e))?;

    run_command(
        Command::new(python_path).args(["-m", "pip", "install", "--upgrade", "pip"]),
        "upgrade pip",
    )?;

    run_command(
        Command::new(python_path)
            .args(["-m", "pip", "install", "-r"])
            .arg(&requirements_path),
        "install python requirements",
    )?;

    fs::write(&marker, hash).map_err(|e| format!("Failed to write requirements marker: {}", e))?;
    Ok(())
}

fn run_command(cmd: &mut Command, action: &str) -> Result<(), String> {
    let output = cmd
        .output()
        .map_err(|e| format!("Failed to {}: {}", action, e))?;
    if output.status.success() {
        return Ok(());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(format!(
        "Command to {} failed.\nstdout: {}\nstderr: {}",
        action, stdout, stderr
    ))
}

fn resolve_system_python() -> Option<PathBuf> {
    let candidates = if cfg!(windows) {
        vec![
            "python3.13",
            "python3.12",
            "python3.11",
            "python3.10",
            "python3",
            "python",
            "py",
        ]
    } else {
        vec![
            "python3.13",
            "python3.12",
            "python3.11",
            "python3.10",
            "python3",
            "python",
        ]
    };

    for candidate in candidates {
        if let Some(version) = interpreter_version(candidate) {
            if version_in_supported_range(version) {
                return Some(PathBuf::from(candidate));
            }
        }
    }
    None
}

fn requirements_hash() -> String {
    let mut hasher = Sha256::new();
    hasher.update(REQUIREMENTS_TEXT.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn interpreter_supported(path: &Path) -> Result<bool, String> {
    match interpreter_version_path(path) {
        Ok(version) => Ok(version_in_supported_range(version)),
        Err(err) => Err(err),
    }
}

fn interpreter_version(candidate: &str) -> Option<(u32, u32)> {
    let output = Command::new(candidate).arg("--version").output().ok()?;
    parse_python_version(output)
}

fn interpreter_version_path(path: &Path) -> Result<(u32, u32), String> {
    let output = Command::new(path).arg("--version").output().map_err(|e| {
        format!(
            "Failed to query python version at {}: {}",
            path.display(),
            e
        )
    })?;
    parse_python_version(output).ok_or_else(|| {
        format!(
            "Could not parse python version output for {}",
            path.display()
        )
    })
}

fn parse_python_version(output: std::process::Output) -> Option<(u32, u32)> {
    let text = if output.stdout.is_empty() {
        String::from_utf8(output.stderr).ok()?
    } else {
        String::from_utf8(output.stdout).ok()?
    };
    let trimmed = text.trim();
    let version_str = trimmed.strip_prefix("Python ")?;
    let mut parts = version_str.split('.');
    let major = parts.next()?.parse::<u32>().ok()?;
    let minor = parts.next()?.parse::<u32>().ok()?;
    Some((major, minor))
}

fn version_in_supported_range(version: (u32, u32)) -> bool {
    version >= PY_MIN_VERSION && version < PY_MAX_VERSION_EXCLUSIVE
}
