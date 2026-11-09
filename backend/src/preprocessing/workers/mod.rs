//! Concrete preprocessor implementations.
//!
//! Each module is one [`Preprocessor`]: it owns the DB persistence and the
//! scheduler wiring, and its `process` submodule runs the Python worker.
//!
//! Bar-indexed preprocessors (`classifier`, `genre`) additionally share
//! [`track_bar_boundaries`], so they agree on what "bar N" means, and each
//! declares where its row records bar 0 ([`Preprocessor::first_bar`]), so
//! both run again when the beat grid moves underneath them.
//!
//! [`Preprocessor`]: crate::preprocessing::preprocessor::Preprocessor
//! [`Preprocessor::first_bar`]: crate::preprocessing::preprocessor::Preprocessor::first_bar

pub mod beat_grid;
pub mod classifier;
pub mod genre;
pub mod mert;
pub mod n2n;
pub mod roots;
pub mod stems;

use serde::Deserialize;
use sqlx::SqlitePool;

use crate::database::local::tracks as tracks_db;

/// The error of a worker process that exited unsuccessfully: one line naming
/// the worker and what it last said, then everything it wrote to stderr.
///
/// What it last said is the message of the `{"error": …}` line a worker
/// prints as it gives up, or the exception a worker let reach Python. The
/// traceback and whatever the libraries logged on the way come after it. The
/// failure record keeps all of it for whoever has to find where the worker
/// broke, and the import list and the agent show only the first line (see
/// [`failures::headline`](crate::preprocessing::failures::headline)).
pub(crate) fn failed(worker: &str, stderr: &str) -> String {
    #[derive(Deserialize)]
    struct Said {
        error: String,
    }

    let stderr = stderr.trim();
    // A progress bar redraws its line with carriage returns, so only the text
    // after the last one was still on screen.
    let Some(last) = stderr
        .lines()
        .rev()
        .map(|line| line.rsplit('\r').next().unwrap_or(line).trim())
        .find(|line| !line.is_empty())
    else {
        return format!("{worker} exited unsuccessfully");
    };
    let said =
        serde_json::from_str::<Said>(last).map_or_else(|_| last.to_owned(), |said| said.error);
    if last == stderr {
        format!("{worker} failed: {said}")
    } else {
        format!("{worker} failed: {said}\n\n{stderr}")
    }
}

/// Build `[(start, end), ...]` bar boundaries from downbeat times, falling
/// back to a synthetic final bar of `60/bpm * beats_per_bar` seconds.
///
/// This is *the* definition of Luma's bar axis: every bar-indexed artifact
/// derives its `bar_idx` from this list, which is what lets the agent index
/// `features.bars` and `features.genres` with the same integer.
///
/// Returns an empty Vec when the grid has fewer than two downbeats.
fn build_bar_boundaries(
    downbeats: &[f64],
    bpm: Option<f64>,
    beats_per_bar: Option<i64>,
) -> Vec<(f64, f64)> {
    if downbeats.len() < 2 {
        return Vec::new();
    }
    let mut out: Vec<(f64, f64)> = downbeats.windows(2).map(|w| (w[0], w[1])).collect();
    let bpm = bpm.unwrap_or(0.0);
    let bpb = beats_per_bar.unwrap_or(4) as f64;
    if bpm > 0.0 && bpb > 0.0 {
        let bar_secs = (60.0 / bpm) * bpb;
        let last = *downbeats.last().unwrap();
        out.push((last, last + bar_secs));
    }
    out
}

/// The bar boundaries of a track's current beat grid (see
/// [`build_bar_boundaries`]). Empty when the track has no grid or fewer than
/// two downbeats.
pub async fn track_bar_boundaries(
    pool: &SqlitePool,
    track_id: &str,
) -> Result<Vec<(f64, f64)>, String> {
    let Some(beats) = tracks_db::get_track_beats_raw(pool, track_id).await? else {
        return Ok(Vec::new());
    };
    let downbeats: Vec<f64> = serde_json::from_str(&beats.downbeats_json)
        .map_err(|e| format!("Failed to parse downbeats_json: {e}"))?;
    Ok(build_bar_boundaries(
        &downbeats,
        beats.bpm,
        beats.beats_per_bar,
    ))
}

#[cfg(test)]
mod tests {
    use super::{build_bar_boundaries, failed};
    use crate::preprocessing::failures::headline;

    #[test]
    fn a_failed_worker_leads_with_its_last_word_and_keeps_its_traceback() {
        let stderr = "WARNING: feature_extractor_cqt requires the libray 'nnAudio'\n\
                      Loading weights:   0%|          | 0/211\rLoading weights: 100%|##| 211/211\n\
                      Traceback (most recent call last):\n  \
                        File \"mert_worker.py\", line 152, in main\n\
                      TypeError: 'NoneType' object is not subscriptable\n\
                      {\"error\": \"'NoneType' object is not subscriptable\"}\n";
        let error = failed("MERT worker", stderr);
        assert_eq!(
            headline(&error),
            "MERT worker failed: 'NoneType' object is not subscriptable"
        );
        assert!(
            error.contains("File \"mert_worker.py\", line 152, in main"),
            "the traceback is lost: {error}"
        );
    }

    #[test]
    fn a_worker_that_let_its_exception_through_leads_with_the_exception() {
        let stderr = "Traceback (most recent call last):\n  \
                        File \"genre_worker.py\", line 530, in main\n\
                      RuntimeError: no ONNX model found\n";
        assert_eq!(
            headline(&failed("Genre worker", stderr)),
            "Genre worker failed: RuntimeError: no ONNX model found"
        );
    }

    #[test]
    fn a_worker_that_said_one_line_or_nothing_is_reported_in_one_line() {
        assert_eq!(
            failed(
                "n2n worker",
                "{\"error\": \"MERT cache does not exist: /c.npy\"}\n"
            ),
            "n2n worker failed: MERT cache does not exist: /c.npy"
        );
        assert_eq!(
            failed("n2n worker", "\n"),
            "n2n worker exited unsuccessfully"
        );
    }

    #[test]
    fn build_bar_boundaries_pairs_consecutive_downbeats_plus_synth_tail() {
        let db = vec![0.0, 1.0, 2.0, 3.0];
        let out = build_bar_boundaries(&db, Some(120.0), Some(4));
        // 3 real bars (0-1, 1-2, 2-3) + 1 synthetic tail (3, 5).
        assert_eq!(out.len(), 4);
        assert_eq!(out[0], (0.0, 1.0));
        assert_eq!(out[2], (2.0, 3.0));
        assert!((out[3].0 - 3.0).abs() < 1e-9);
        assert!((out[3].1 - 5.0).abs() < 1e-9); // 3 + (60/120)*4 = 3 + 2 = 5
    }

    #[test]
    fn build_bar_boundaries_returns_empty_for_too_few_downbeats() {
        assert!(build_bar_boundaries(&[], Some(120.0), Some(4)).is_empty());
        assert!(build_bar_boundaries(&[1.0], Some(120.0), Some(4)).is_empty());
    }
}
