//! Finding the library track a deck has loaded.

use std::collections::HashSet;

use sqlx::SqlitePool;

use super::LoadedTrack;
use crate::database::local::tracks;

/// The library track `loaded` is, if one is close enough.
///
/// A source that names the file (StageLinQ) is matched by filename. Any other
/// is matched by metadata, in an ordered cascade where each stage narrows the
/// last:
///   1. duration within ±5 s, when the source reports one (rkbx_link does not),
///   2. BPM within 5%, allowing the ×2 / ÷2 harmonic analyzers disagree on,
///   3. bigram similarity of the normalized `title artist`, best above 0.25.
pub(crate) async fn find_track(
    pool: &SqlitePool,
    loaded: &LoadedTrack,
) -> Result<Option<String>, String> {
    if let Some(path) = &loaded.network_path {
        let Some(filename) = stagelinq::extract_filename_from_network_path(path) else {
            return Ok(None);
        };
        // No tie-break: duplicate filenames resolve to an arbitrary row.
        let found = tracks::get_tracks_by_source_filename(pool, filename).await?;
        return Ok(found.into_iter().next().map(|track| track.id));
    }
    if loaded.title.is_empty() && loaded.artist.is_empty() {
        return Ok(None);
    }

    let candidates =
        tracks::get_match_candidates(pool, loaded.duration.map(|seconds| (seconds, 5.0))).await?;
    let query = normalize(&format!("{} {}", loaded.title, loaded.artist));
    let best = candidates
        .iter()
        .filter(|track| bpm_matches(track.bpm.unwrap_or(0.0), loaded.bpm.unwrap_or(0.0)))
        .map(|track| {
            let library = normalize(&format!(
                "{} {}",
                track.title.as_deref().unwrap_or(""),
                track.artist.as_deref().unwrap_or("")
            ));
            (track, bigram_similarity(&query, &library))
        })
        .filter(|(_, score)| *score >= 0.25)
        .max_by(|a, b| a.1.total_cmp(&b.1));
    Ok(best.map(|(track, _)| track.id.clone()))
}

/// Whether `lib_bpm` is within 5% of `src_bpm` at any of the ×1 / ×2 / ÷2
/// harmonics analyzers disagree on. Missing BPM on either side passes through
/// rather than filtering the candidate out.
fn bpm_matches(lib_bpm: f64, src_bpm: f64) -> bool {
    if lib_bpm <= 0.0 || src_bpm <= 0.0 {
        return true;
    }
    let tolerance = 0.05;
    [1.0f64, 2.0, 0.5]
        .iter()
        .any(|ratio| (lib_bpm * ratio - src_bpm).abs() / src_bpm <= tolerance)
}

/// Lowercase, drop everything non-alphanumeric, collapse whitespace.
fn normalize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_lowercase().next().unwrap_or(c)
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Character-bigram Jaccard similarity in [0, 1].
fn bigram_similarity(a: &str, b: &str) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    if a == b {
        return 1.0;
    }
    let bigrams_a: HashSet<(char, char)> = a.chars().zip(a.chars().skip(1)).collect();
    let bigrams_b: HashSet<(char, char)> = b.chars().zip(b.chars().skip(1)).collect();
    if bigrams_a.is_empty() || bigrams_b.is_empty() {
        return 0.0;
    }
    let intersection = bigrams_a.intersection(&bigrams_b).count();
    let union = bigrams_a.len() + bigrams_b.len() - intersection;
    if union == 0 {
        0.0
    } else {
        intersection as f64 / union as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn harmonic_bpm_disagreements_still_match() {
        assert!(bpm_matches(64.0, 128.0));
        assert!(bpm_matches(256.0, 128.0));
        assert!(!bpm_matches(100.0, 128.0));
        assert!(
            bpm_matches(0.0, 128.0),
            "an unanalyzed track is not ruled out"
        );
    }

    #[test]
    fn punctuation_and_case_do_not_change_similarity() {
        let a = normalize("Strobe (Club Edit) - deadmau5");
        let b = normalize("strobe club edit deadmau5");
        assert_eq!(bigram_similarity(&a, &b), 1.0);
        assert!(bigram_similarity("strobe deadmau5", "ghosts n stuff deadmau5") < 1.0);
    }
}
