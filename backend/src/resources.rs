//! Where an installed build keeps what a source tree keeps in the repository.
//!
//! A packaged Luma carries one resource directory, laid out as the release
//! bundler writes it (`script/bundle-mac`, `script/bundle-linux`):
//!
//! ```text
//! python-runtime/   standalone CPython that seeds the managed venv
//! ffmpeg-runtime/   the ffmpeg binary
//! python/           the analysis workers (`backend/python`)
//! resources/        fixtures/, meshes/, skills/
//! ```
//!
//! It sits at `Luma.app/Contents/Resources` on macOS and at
//! `luma.app/share/luma` beside `luma.app/bin/luma` in the Linux tarball.
//! `LUMA_RESOURCE_DIR` overrides both. A source build has none, and every
//! caller falls back to the repository through `CARGO_MANIFEST_DIR`.

use std::path::{Path, PathBuf};

/// The packaged resource directory, if this process has one.
///
/// Not memoized: it is a handful of `stat`s, read at startup and when a worker
/// launches, and a cached answer would let the first reader decide for a test
/// that sets `LUMA_RESOURCE_DIR` afterwards.
#[must_use]
pub fn resource_dir() -> Option<PathBuf> {
    if let Some(root) = std::env::var_os("LUMA_RESOURCE_DIR") {
        return Some(PathBuf::from(root));
    }
    packaged(&std::env::current_exe().ok()?)
}

/// The resource directory a bundle puts beside `exe`, if it is there.
///
/// Recognised by its fixture tree, which every bundle carries and a cargo
/// `target/` directory never does — so a dev binary answers `None` rather than
/// adopting some unrelated `Resources` folder above it.
fn packaged(exe: &Path) -> Option<PathBuf> {
    let dir = exe.parent()?;
    [
        dir.join("../Resources"),
        dir.join("../share/luma"),
        dir.to_path_buf(),
    ]
    .into_iter()
    .find(|candidate| candidate.join("resources/fixtures").is_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle(layout: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let (exe, resources) = match layout {
            "macos" => (
                root.path().join("Luma.app/Contents/MacOS/luma"),
                root.path().join("Luma.app/Contents/Resources"),
            ),
            "linux" => (
                root.path().join("luma.app/bin/luma"),
                root.path().join("luma.app/share/luma"),
            ),
            _ => unreachable!(),
        };
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, b"").unwrap();
        std::fs::create_dir_all(resources.join("resources/fixtures")).unwrap();
        (root, exe, resources)
    }

    #[test]
    fn finds_each_bundle_layout_from_its_executable() {
        for layout in ["macos", "linux"] {
            let (_root, exe, resources) = bundle(layout);
            let found = packaged(&exe).expect(layout);
            assert_eq!(
                found.canonicalize().unwrap(),
                resources.canonicalize().unwrap(),
                "{layout}"
            );
        }
    }

    #[test]
    fn a_bare_executable_has_no_resource_directory() {
        let root = tempfile::tempdir().unwrap();
        let exe = root.path().join("target/release/luma-app");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        // A `Resources` folder without a fixture tree is not a bundle.
        std::fs::create_dir_all(root.path().join("target/Resources")).unwrap();
        assert_eq!(packaged(&exe), None);
    }
}
