use std::path::PathBuf;
use std::sync::OnceLock;

static FFMPEG_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Initialize the ffmpeg path from the Tauri resource directory.
/// Call once during app setup.
pub fn init(app: &tauri::AppHandle) {
    use tauri::Manager;

    FFMPEG_PATH.get_or_init(|| {
        let mut search_dirs = Vec::new();

        // Production: Tauri resource directory
        if let Ok(resource_dir) = app.path().resource_dir() {
            search_dirs.push(resource_dir.join("ffmpeg-runtime"));
        }

        // Development: relative to the source directory (src-tauri/ffmpeg-runtime)
        if let Ok(exe) = std::env::current_exe() {
            if let Some(exe_dir) = exe.parent() {
                if exe_dir.ends_with("debug") || exe_dir.ends_with("release") {
                    if let Some(target) = exe_dir.parent() {
                        if let Some(src_tauri) = target.parent() {
                            search_dirs.push(src_tauri.join("ffmpeg-runtime"));
                        }
                    }
                }
            }
        }

        let binary_name = if cfg!(windows) {
            "ffmpeg.exe"
        } else {
            "ffmpeg"
        };

        for dir in &search_dirs {
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
