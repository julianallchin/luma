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

use std::path::{Component, Path, PathBuf};

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

/// Where the stage GLBs live.
///
/// `LUMA_MESHES_ROOT` wins, so a test or a fixture can supply its own tree.
/// Otherwise the meshes are found *through* the fixtures root rather than by a
/// second search: a fixtures root is always `<resources>/fixtures/<version>`,
/// so its grandparent's `meshes/` is the sibling tree, in the repo and in a
/// bundle alike: `resources/fixtures` and `resources/meshes` are siblings.
/// A caller with no fixtures root (the desktop viewport) gets the installed
/// bundle's copy, and the repo's own copy is the last resort.
#[must_use]
pub fn meshes_root(fixtures_root: Option<&Path>) -> PathBuf {
    if let Some(path) = std::env::var_os("LUMA_MESHES_ROOT") {
        return PathBuf::from(path);
    }
    let sibling = fixtures_root
        .and_then(Path::parent)
        .and_then(Path::parent)
        .map(|resources| resources.join("meshes"));
    let bundled = resource_dir().map(|root| root.join("resources/meshes"));
    match [sibling, bundled]
        .into_iter()
        .flatten()
        .find(|path| path.is_dir())
    {
        Some(path) => path,
        None => Path::new(env!("CARGO_MANIFEST_DIR")).parent().map_or_else(
            || PathBuf::from("resources/meshes"),
            |repo| repo.join("resources/meshes"),
        ),
    }
}

/// A fixture path that stays inside the fixtures root, or nothing.
#[must_use]
pub fn confined_path(path: &str) -> Option<&Path> {
    let relative = Path::new(path);
    let escapes = relative.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    });
    (!escapes).then_some(relative)
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
