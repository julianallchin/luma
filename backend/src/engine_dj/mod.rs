pub mod db;
pub mod types;

use std::path::{Path, PathBuf};

/// Resolve an Engine DJ relative path to an absolute path.
/// Engine DJ stores paths like `../../Music Library/song.mp3` relative to Database2/.
/// We resolve relative to the Engine Library root.
pub fn resolve_engine_path(library_path: &str, relative_path: &str) -> PathBuf {
    let base = Path::new(library_path);
    let combined = base.join(relative_path);
    // Try to canonicalize (resolves ..) — fall back to the joined path
    combined.canonicalize().unwrap_or(combined)
}

/// Default Engine DJ library path.
pub fn default_library_path() -> PathBuf {
    dirs::audio_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join("Music"))
        .join("Engine Library")
}
