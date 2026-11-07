//! The one check every golden test in the repository makes.
//!
//! Here because this is the lowest crate the backend, the scene and the
//! renderer tests all reach. Goldens are check-only: a plain run compares and
//! fails, and only `LUMA_UPDATE_GOLDENS` rewrites, so a failing run leaves the
//! committed file in place and the next run fails the same way.

use std::path::Path;

/// Fails unless the golden at `path` holds exactly `actual`. With
/// `LUMA_UPDATE_GOLDENS` set it writes `actual` there instead, for a human to
/// review and commit.
///
/// # Panics
///
/// When the golden is stale, or the update cannot be written.
#[track_caller]
pub fn assert_golden(path: &Path, actual: &str) {
    if std::env::var_os("LUMA_UPDATE_GOLDENS").is_some() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap_or_else(|e| panic!("{}: {e}", parent.display()));
        }
        std::fs::write(path, actual).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        return;
    }
    let golden = std::fs::read_to_string(path).unwrap_or_default();
    assert!(
        golden == actual,
        "{} is stale at {}\nRegenerate with LUMA_UPDATE_GOLDENS=1, then review and commit the diff.",
        path.display(),
        first_difference(&golden, actual),
    );
}

/// The first line that differs, on both sides: enough to name what moved
/// without printing the whole file.
fn first_difference(golden: &str, actual: &str) -> String {
    let (golden, actual): (Vec<&str>, Vec<&str>) =
        (golden.lines().collect(), actual.lines().collect());
    let at = (0..golden.len().max(actual.len()))
        .find(|i| golden.get(*i) != actual.get(*i))
        .unwrap_or(0);
    let line = |lines: &[&str]| lines.get(at).copied().unwrap_or("<end of file>").to_owned();
    format!(
        "line {}:\n  golden: {}\n  actual: {}",
        at + 1,
        line(&golden),
        line(&actual),
    )
}
