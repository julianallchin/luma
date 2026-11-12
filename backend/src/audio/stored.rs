//! The one format Luma keeps a song in: Ogg Opus.
//!
//! Import converts to it, sync uploads the stored bytes, and every reader —
//! the decode cache, the analysis workers, another member's device — gets the
//! same audio whatever the song arrived as.

use std::path::Path;

/// The extension of a stored song.
pub const EXTENSION: &str = "ogg";

/// Convert `source`, whatever audio it holds and whatever it is named, into
/// the stored format at `dest`. ffmpeg goes by content, so an extension that
/// lies is not a problem. A failure leaves nothing at `dest`.
///
/// `max_seconds` stops the conversion there: a caller with a length limit
/// passes a little more than it, so audio over the limit costs no more than
/// audio at it and still measures as over.
pub fn store(source: &Path, dest: &Path, max_seconds: Option<f64>) -> Result<(), String> {
    let ffmpeg = crate::runtime::ffmpeg::ffmpeg_path();
    let mut command = crate::runtime::command::command(&ffmpeg);
    command
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(source);
    if let Some(seconds) = max_seconds {
        command.args(["-t", &seconds.to_string()]);
    }
    let output = command
        .args(["-vn", "-c:a", "libopus", "-b:a", "96k", "-f", "ogg", "-y"])
        .arg(dest)
        .output()
        .map_err(|error| format!("Failed to run ffmpeg: {error}"))?;
    if output.status.success() {
        return Ok(());
    }
    let _ = std::fs::remove_file(dest);
    Err(format!(
        "ffmpeg could not convert {}: {}",
        source.display(),
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}
