use sha2::{Digest, Sha256};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use tauri::{AppHandle, Manager};

const PY_MIN_VERSION: (u32, u32) = (3, 12);
const PY_MAX_VERSION_EXCLUSIVE: (u32, u32) = (3, 13);
const REQUIREMENT_FILES: &[(&str, &str)] = &[
    (
        "requirements.txt",
        include_str!("../python/requirements.txt"),
    ),
    (
        "consonance-ACE/requirements.txt",
        include_str!("../python/consonance-ACE/requirements.txt"),
    ),
];

pub fn ensure_worker_script(
    app: &AppHandle,
    script_name: &str,
    source: &str,
) -> Result<PathBuf, String> {
    let cache_dir = app
        .path()
        .app_cache_dir()
        .map_err(|e| format!("Failed to locate cache dir: {}", e))?;
    fs::create_dir_all(&cache_dir)
        .map_err(|e| format!("Failed to create cache dir {}: {}", cache_dir.display(), e))?;

    let script_path = cache_dir.join(script_name);
    fs::write(&script_path, source).map_err(|e| {
        format!(
            "Failed to write python worker {}: {}",
            script_path.display(),
            e
        )
    })?;
    Ok(script_path)
}

pub fn ensure_python_resource_dir(app: &AppHandle, relative: &str) -> Result<PathBuf, String> {
    let cache_dir = app
        .path()
        .app_cache_dir()
        .map_err(|e| format!("Failed to locate cache dir: {}", e))?;
    let dest_root = cache_dir.join(relative);
    if dest_root.exists() {
        return Ok(dest_root);
    }

    let source_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("python")
        .join(relative);
    if !source_root.exists() {
        return Err(format!(
            "Missing bundled python resource at {}",
            source_root.display()
        ));
    }

    copy_dir_recursive(&source_root, &dest_root).map_err(|e| format!("{}", e))?;
    Ok(dest_root)
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> io::Result<()> {
    if !dst.exists() {
        fs::create_dir_all(dst)?;
    }

    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let dest_path = dst.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_dir_recursive(&path, &dest_path)?;
        } else if file_type.is_file() {
            if let Some(parent) = dest_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&path, &dest_path)?;
        }
    }
    Ok(())
}

pub fn ensure_python_env(app: &AppHandle) -> Result<PathBuf, String> {
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
        if !interpreter_supported(path).unwrap_or(false) {
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
        create_virtual_env(app, &env_dir)?;
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

fn create_virtual_env(app: &AppHandle, env_dir: &Path) -> Result<(), String> {
    if let Some(parent) = env_dir.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            format!(
                "Failed to create python env parent {}: {}",
                parent.display(),
                e
            )
        })?;
    }

    let system_python = resolve_bundled_or_system_python(app)
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

    let mut requirement_paths = Vec::new();
    for (relative_path, contents) in REQUIREMENT_FILES {
        let path = env_dir.join(relative_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to prepare env dir {}: {}", parent.display(), e))?;
        }
        fs::write(&path, contents).map_err(|e| {
            format!(
                "Failed to write requirements file {}: {}",
                path.display(),
                e
            )
        })?;
        requirement_paths.push(path);
    }

    run_command(
        Command::new(python_path).args(["-m", "pip", "install", "--upgrade", "pip"]),
        "upgrade pip",
    )?;

    for requirements_path in &requirement_paths {
        run_command(
            Command::new(python_path)
                .args(["-m", "pip", "install", "-r"])
                .arg(requirements_path),
            &format!(
                "install python requirements from {}",
                requirements_path.display()
            ),
        )?;
    }

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

fn resolve_bundled_or_system_python(app: &AppHandle) -> Option<PathBuf> {
    // Check multiple locations for bundled Python (dev vs production)
    let mut search_dirs = Vec::new();

    // Production: Tauri resource directory
    if let Ok(resource_dir) = app.path().resource_dir() {
        search_dirs.push(resource_dir.join("python-runtime"));
    }

    // Development: relative to the source directory (src-tauri/python-runtime)
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            // In dev: target/debug/binary -> ../../python-runtime
            if exe_dir.ends_with("debug") || exe_dir.ends_with("release") {
                if let Some(target) = exe_dir.parent() {
                    if let Some(src_tauri) = target.parent() {
                        search_dirs.push(src_tauri.join("python-runtime"));
                    }
                }
            }
        }
    }

    for python_runtime_dir in search_dirs {
        eprintln!(
            "[python-env] Looking for bundled Python in: {}",
            python_runtime_dir.display()
        );

        let python_dir = python_runtime_dir.join("python");

        let candidates = if cfg!(windows) {
            vec![
                python_dir.join("python.exe"),
                python_dir.join("Scripts").join("python.exe"),
            ]
        } else {
            vec![
                python_dir.join("bin").join("python3"),
                python_dir.join("bin").join("python"),
            ]
        };

        for candidate in &candidates {
            if candidate.exists() {
                if let Ok(version) = interpreter_version_path(candidate) {
                    if version_in_supported_range(version) {
                        eprintln!(
                            "[python-env] Found bundled Python {}.{} at: {}",
                            version.0,
                            version.1,
                            candidate.display()
                        );
                        return Some(candidate.clone());
                    }
                }
            }
        }
    }

    eprintln!("[python-env] ERROR: Bundled Python not found!");
    None
}

fn requirements_hash() -> String {
    let mut hasher = Sha256::new();
    for (_, contents) in REQUIREMENT_FILES {
        hasher.update(contents.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

fn interpreter_supported(path: &Path) -> Result<bool, String> {
    match interpreter_version_path(path) {
        Ok(version) => Ok(version_in_supported_range(version)),
        Err(err) => Err(err),
    }
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
