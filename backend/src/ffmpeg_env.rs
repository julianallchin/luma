//! Where the bundled ffmpeg binary is.
//!
//! A one-shot search resolves host resources and the developer runtime. Native
//! and headless hosts share the same fallback to system PATH.

use std::path::PathBuf;
use std::sync::OnceLock;

static FFMPEG_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

pub fn init_headless() {
    let mut dirs = Vec::new();
    if let Some(root) = std::env::var_os("LUMA_RESOURCE_DIR") {
        dirs.push(PathBuf::from(root).join("ffmpeg-runtime"));
    }
    dirs.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ffmpeg-runtime"));
    dirs.extend(dev_runtime_dir());
    init_from(&dirs);
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

/// `backend/ffmpeg-runtime`, found by walking up from the executable. This is
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
