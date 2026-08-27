//! Where the bundled ffmpeg binary is.
//!
//! Resolution is a one-shot search over a list of directories, latched in a
//! `OnceLock`. Who supplies that list differs — the desktop app knows its Tauri
//! resource dir, a headless binary only knows where its own executable is — so
//! the *search* lives in [`init_from`] and each host supplies its own dirs.
//! Before that split a headless binary silently fell through to system `PATH`
//! and failed at spawn time with no diagnosis.

use std::path::PathBuf;
use std::sync::OnceLock;

static FFMPEG_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Initialize the ffmpeg path from the Tauri resource directory.
/// Call once during app setup.
pub fn init(app: &tauri::AppHandle) {
    use tauri::Manager;

    let mut dirs = Vec::new();
    if let Ok(resource_dir) = app.path().resource_dir() {
        dirs.push(resource_dir.join("ffmpeg-runtime"));
    }
    dirs.extend(dev_runtime_dir());
    init_from(&dirs);
}

/// Initialize from the executable's own location — the answer for any host with
/// no `AppHandle`. Call once during headless boot.
pub fn init_headless() {
    init_from(&dev_runtime_dir().into_iter().collect::<Vec<_>>());
}

/// Latch the first directory in `dirs` that holds an ffmpeg binary.
///
/// Idempotent: the first call wins, later ones are no-ops, so a host that boots
/// twice in one process does not thrash the path.
pub fn init_from(dirs: &[PathBuf]) {
    FFMPEG_PATH.get_or_init(|| {
        let binary_name = if cfg!(windows) {
            "ffmpeg.exe"
        } else {
            "ffmpeg"
        };
        for dir in dirs {
            let candidate = dir.join(binary_name);
            if candidate.exists() {
                eprintln!(
                    "[ffmpeg-env] Found bundled ffmpeg at: {}",
                    candidate.display()
                );
                return Some(candidate);
            }
        }
        eprintln!("[ffmpeg-env] Bundled ffmpeg not found, will fall back to system PATH");
        None
    });
}

/// `src-tauri/ffmpeg-runtime`, found by walking up from the executable. This is
/// where `build.rs` downloads it, so it is the answer in a dev tree for the app
/// and for every headless binary alike.
///
/// Searched by *presence* rather than by counting `target/<profile>/` levels:
/// the profile directory is named by whichever profile was built (`debug`,
/// `release`, `perf`, …) and a hardcoded pair of names silently misses the rest.
fn dev_runtime_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    exe.ancestors()
        .map(|dir| dir.join("ffmpeg-runtime"))
        .find(|candidate| candidate.is_dir())
}

/// Get the path to the ffmpeg binary.
/// Returns the bundled path if available, otherwise "ffmpeg" (system PATH).
pub fn ffmpeg_path() -> PathBuf {
    FFMPEG_PATH
        .get()
        .and_then(|opt| opt.as_ref().cloned())
        .unwrap_or_else(|| PathBuf::from("ffmpeg"))
}

/// Get the directory containing the bundled ffmpeg binary, if available.
/// Useful for prepending to PATH when spawning subprocesses (e.g. Python workers).
pub fn ffmpeg_dir() -> Option<PathBuf> {
    FFMPEG_PATH
        .get()
        .and_then(|opt| opt.as_ref())
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
}
