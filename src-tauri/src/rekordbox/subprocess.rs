use std::path::PathBuf;
use std::process::Command;

use super::types::{RekordboxLibraryInfo, RekordboxPlaylist, RekordboxTrack};

/// Locate the rekordbox_read binary.
/// In both dev and production it sits next to the main app binary
/// (same workspace target dir in dev, bundled sidecar in production).
fn binary_path() -> Result<PathBuf, String> {
    let bin_name = if cfg!(windows) {
        "rekordbox_read.exe"
    } else {
        "rekordbox_read"
    };

    // Same directory as the running executable (works for dev + production)
    if let Ok(exe) = std::env::current_exe() {
        let sidecar = exe.parent().unwrap_or(exe.as_ref()).join(bin_name);
        if sidecar.exists() {
            return Ok(sidecar);
        }
    }

    // Fallback: assume it's on PATH
    Ok(PathBuf::from(bin_name))
}

fn run_command(args: &[&str]) -> Result<String, String> {
    let bin = binary_path()?;
    let output = Command::new(&bin).args(args).output().map_err(|e| {
        format!(
            "Failed to run rekordbox_read: {} (path: {})",
            e,
            bin.display()
        )
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("rekordbox_read failed: {}", stderr.trim()));
    }

    String::from_utf8(output.stdout)
        .map_err(|e| format!("Invalid UTF-8 from rekordbox_read: {}", e))
}

pub fn get_library_info() -> Result<RekordboxLibraryInfo, String> {
    let json = run_command(&["library-info"])?;
    serde_json::from_str(&json).map_err(|e| format!("Failed to parse library info: {}", e))
}

pub fn list_tracks() -> Result<Vec<RekordboxTrack>, String> {
    let json = run_command(&["list-tracks"])?;
    serde_json::from_str(&json).map_err(|e| format!("Failed to parse tracks: {}", e))
}

pub fn list_playlists() -> Result<Vec<RekordboxPlaylist>, String> {
    let json = run_command(&["list-playlists"])?;
    serde_json::from_str(&json).map_err(|e| format!("Failed to parse playlists: {}", e))
}

pub fn get_playlist_tracks(playlist_id: &str) -> Result<Vec<RekordboxTrack>, String> {
    let json = run_command(&["playlist-tracks", playlist_id])?;
    serde_json::from_str(&json).map_err(|e| format!("Failed to parse playlist tracks: {}", e))
}

pub fn search_tracks(query: &str) -> Result<Vec<RekordboxTrack>, String> {
    let json = run_command(&["search", query])?;
    serde_json::from_str(&json).map_err(|e| format!("Failed to parse search results: {}", e))
}
