use sha2::{Digest, Sha256};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use tauri::{AppHandle, Emitter, Manager};

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

// ---------------------------------------------------------------------------
// CUDA wheel selection table
// ---------------------------------------------------------------------------
//
// Each row is a PyTorch CUDA wheel tier.  Rows are ordered highest-first;
// the first row whose requirements are met by the machine wins.
//
// To support a new PyTorch CUDA tier (e.g. cu130), add one row here.
// New GPU architectures (e.g. sm_130) work automatically as long as
// their sm value exceeds the min_sm of an existing tier.

struct CudaTier {
    /// PyTorch index tag, e.g. "cu128".
    index_tag: &'static str,
    /// torch + torchaudio version to install from this tier.
    torch_version: &'static str,
    /// Minimum CUDA driver version reported by nvidia-smi.
    min_cuda: (u32, u32),
    /// Minimum compute capability (major*10 + minor).
    /// Wheels from this tier ship kernels for this SM and above.
    min_sm: u32,
}

const CUDA_TIERS: &[CudaTier] = &[
    CudaTier {
        index_tag: "cu128",
        torch_version: "2.8.0",
        min_cuda: (12, 8),
        min_sm: 75,
    },
    CudaTier {
        index_tag: "cu126",
        torch_version: "2.8.0",
        min_cuda: (12, 6),
        min_sm: 50,
    },
    CudaTier {
        index_tag: "cu124",
        torch_version: "2.6.0",
        min_cuda: (12, 4),
        min_sm: 50,
    },
    CudaTier {
        index_tag: "cu118",
        torch_version: "2.6.0",
        min_cuda: (11, 8),
        min_sm: 35,
    },
];

/// Default torch version for macOS (MPS) and CPU-only installs.
const TORCH_DEFAULT_VERSION: &'static str = "2.6.0";

/// Resolved PyTorch install plan for this machine.
struct TorchInstall {
    packages: Vec<String>,
    index_url: Option<String>,
    /// Stable label for the requirements hash.
    label: String,
}

impl TorchInstall {
    fn default_pypi() -> Self {
        Self {
            packages: vec![
                format!("torch=={TORCH_DEFAULT_VERSION}"),
                format!("torchaudio=={TORCH_DEFAULT_VERSION}"),
            ],
            index_url: None,
            label: format!("default-{TORCH_DEFAULT_VERSION}"),
        }
    }

    fn cuda(tier: &CudaTier) -> Self {
        let v = tier.torch_version;
        Self {
            packages: vec![format!("torch=={v}"), format!("torchaudio=={v}")],
            index_url: Some(format!(
                "https://download.pytorch.org/whl/{}",
                tier.index_tag
            )),
            label: format!("{}-{v}", tier.index_tag),
        }
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

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

/// Kick off Python environment setup on a background thread at app startup.
/// Emits `python-env-progress` events so the frontend can show a toast.
pub fn setup_python_env_background(app_handle: AppHandle) {
    std::thread::spawn(move || match ensure_python_env(&app_handle) {
        Ok(_) => {}
        Err(err) => {
            eprintln!("[python-env] Background setup failed: {}", err);
            let _ = app_handle.emit("python-env-progress", ("error", &err));
        }
    });
}

pub fn ensure_python_env(app: &AppHandle) -> Result<PathBuf, String> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let guard = LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
    let result = ensure_python_env_inner(app);
    drop(guard);
    result
}

// ---------------------------------------------------------------------------
// Internal implementation
// ---------------------------------------------------------------------------

fn ensure_python_env_inner(app: &AppHandle) -> Result<PathBuf, String> {
    let cache_dir = app
        .path()
        .app_cache_dir()
        .map_err(|e| format!("Failed to locate cache dir: {}", e))?;
    let env_dir = cache_dir.join("python-env");
    let mut did_work = false;

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
        let _ = app.emit(
            "python-env-progress",
            ("setup", "Creating Python environment\u{2026}"),
        );
        did_work = true;
        create_virtual_env(app, &env_dir)?;
        python_path = find_venv_python(&env_dir);
    }

    let python_path = python_path.ok_or_else(|| {
        format!(
            "Virtual environment at {} missing python binary",
            env_dir.display()
        )
    })?;

    let installed = install_requirements(app, &python_path, &env_dir)?;
    did_work = did_work || installed;

    if did_work {
        let _ = app.emit("python-env-progress", ("ready", "Python environment ready"));
    }

    Ok(python_path)
}

/// Returns `Ok(true)` when packages were installed, `Ok(false)` when already up-to-date.
fn install_requirements(
    app: &AppHandle,
    python_path: &Path,
    env_dir: &Path,
) -> Result<bool, String> {
    let torch_plan = resolve_torch_install();
    let hash = requirements_hash(&torch_plan);
    let marker = env_dir.join(".requirements.hash");
    if let Ok(existing) = fs::read_to_string(&marker) {
        if existing.trim() == hash {
            if python_path.exists() {
                return Ok(false);
            }
        }
    }

    let _ = app.emit(
        "python-env-progress",
        ("setup", "Installing Python packages\u{2026}"),
    );

    if let Some(parent) = marker.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to prepare env dir {}: {}", parent.display(), e))?;
    }

    // Write requirements files to the env directory, stripping any
    // torch/torchaudio pins so pip won't overwrite the GPU-specific
    // wheels we install in step 1.  (We can't edit the upstream
    // consonance-ACE requirements.txt, so we filter at write time.)
    let mut requirement_paths = Vec::new();
    for (relative_path, contents) in REQUIREMENT_FILES {
        let path = env_dir.join(relative_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to prepare env dir {}: {}", parent.display(), e))?;
        }
        let filtered: String = contents
            .lines()
            .filter(|line| {
                let t = line.trim().to_lowercase();
                !t.starts_with("torch==") && !t.starts_with("torchaudio==")
            })
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&path, filtered).map_err(|e| {
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

    // Step 1: Install torch + torchaudio from the resolved wheel index.
    // These are deliberately absent from the requirements files so pip never
    // overwrites a GPU build with a CPU-only wheel from PyPI.
    {
        let pkg_args: Vec<&str> = torch_plan.packages.iter().map(|s| s.as_str()).collect();

        if let Some(index_url) = &torch_plan.index_url {
            eprintln!("[python-env] Installing PyTorch ({})…", torch_plan.label);
            let _ = app.emit(
                "python-env-progress",
                (
                    "setup",
                    &format!("Installing PyTorch ({})…", torch_plan.label),
                ),
            );

            let result = run_command(
                Command::new(python_path)
                    .args(["-m", "pip", "install"])
                    .args(&pkg_args)
                    .args(["--index-url", index_url]),
                "install PyTorch with CUDA support",
            );

            if let Err(err) = result {
                eprintln!(
                    "[python-env] CUDA install failed, falling back to CPU: {}",
                    err
                );
                run_command(
                    Command::new(python_path)
                        .args(["-m", "pip", "install"])
                        .args(&pkg_args),
                    "install PyTorch (CPU fallback)",
                )?;
            }
        } else {
            run_command(
                Command::new(python_path)
                    .args(["-m", "pip", "install"])
                    .args(&pkg_args),
                "install PyTorch",
            )?;
        }
    }

    // Step 2: Install everything else from the requirements files.
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
    Ok(true)
}

fn requirements_hash(torch: &TorchInstall) -> String {
    let mut hasher = Sha256::new();
    hasher.update(torch.label.as_bytes());
    for (_, contents) in REQUIREMENT_FILES {
        hasher.update(contents.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

// ---------------------------------------------------------------------------
// GPU / torch backend detection
// ---------------------------------------------------------------------------

/// Build a [`TorchInstall`] for this machine.
///
/// Detection strategy (like chaiNNer):
/// 1. macOS → default PyPI wheel (includes MPS on Apple Silicon).
/// 2. Run `nvidia-smi` to get the driver CUDA version **and** the GPU
///    compute capability.
/// 3. Walk [`CUDA_TIERS`] highest-first.  The first tier whose
///    `min_cuda` ≤ driver CUDA **and** `min_sm` ≤ GPU sm wins.
/// 4. No match → default PyPI (CPU-only on Windows/Linux).
///
/// New GPU architectures work automatically as long as their sm value
/// exceeds the `min_sm` of an existing tier.  Adding a new PyTorch CUDA
/// tier is a one-line addition to [`CUDA_TIERS`].
fn resolve_torch_install() -> TorchInstall {
    if cfg!(target_os = "macos") {
        return TorchInstall::default_pypi();
    }

    let (driver_cuda, gpu_sm) = match query_nvidia_gpu_info() {
        Some(info) => info,
        None => return TorchInstall::default_pypi(),
    };

    for tier in CUDA_TIERS {
        if driver_cuda >= tier.min_cuda && gpu_sm >= tier.min_sm {
            eprintln!(
                "[python-env] GPU sm_{} + CUDA driver {}.{} → {} (torch {})",
                gpu_sm, driver_cuda.0, driver_cuda.1, tier.index_tag, tier.torch_version
            );
            return TorchInstall::cuda(tier);
        }
    }

    eprintln!(
        "[python-env] GPU sm_{} + CUDA {}.{} did not match any tier → CPU",
        gpu_sm, driver_cuda.0, driver_cuda.1
    );
    TorchInstall::default_pypi()
}

/// Query nvidia-smi for:
///   - Driver CUDA version (from the header table)
///   - GPU compute capability (via `--query-gpu`)
///
/// Returns `None` if nvidia-smi is absent or fails.
fn query_nvidia_gpu_info() -> Option<((u32, u32), u32)> {
    // 1. Driver CUDA version from the standard nvidia-smi header
    let header = Command::new("nvidia-smi").output().ok()?;
    if !header.status.success() {
        return None;
    }
    let header_text = String::from_utf8_lossy(&header.stdout);
    let driver_cuda = parse_nvidia_smi_cuda_version(&header_text)?;

    // 2. Compute capability via the structured query interface
    let query = Command::new("nvidia-smi")
        .args(["--query-gpu=compute_cap", "--format=csv,noheader"])
        .output()
        .ok()?;
    if !query.status.success() {
        return None;
    }
    let cc_text = String::from_utf8_lossy(&query.stdout);
    let sm = parse_compute_capability(cc_text.trim())?;

    Some((driver_cuda, sm))
}

/// Parse "CUDA Version: X.Y" from the nvidia-smi header.
fn parse_nvidia_smi_cuda_version(text: &str) -> Option<(u32, u32)> {
    let rest = text.split("CUDA Version:").nth(1)?;
    let version_str = rest.trim().split_whitespace().next()?;
    let mut parts = version_str.split('.');
    let major = parts.next()?.parse::<u32>().ok()?;
    let minor = parts.next()?.parse::<u32>().ok()?;
    Some((major, minor))
}

/// Parse a compute capability like "12.0" into a flat sm value: 120.
fn parse_compute_capability(text: &str) -> Option<u32> {
    let mut parts = text.split('.');
    let major: u32 = parts.next()?.parse().ok()?;
    let minor: u32 = parts.next()?.parse().ok()?;
    Some(major * 10 + minor)
}

// ---------------------------------------------------------------------------
// Venv helpers
// ---------------------------------------------------------------------------

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
    let mut search_dirs = Vec::new();

    if let Ok(resource_dir) = app.path().resource_dir() {
        search_dirs.push(resource_dir.join("python-runtime"));
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
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
