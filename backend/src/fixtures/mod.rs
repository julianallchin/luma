pub mod engine;
pub mod layout;
pub mod library;
pub mod parser;

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

use once_cell::sync::Lazy;

use crate::models::fixtures::FixtureDefinition;

/// The definition at `fixture_path` under `root`, parsed at most once per file
/// per process.
///
/// A `lib:<id>` path names a fixture of the library beside `root`
/// ([`crate::resources::fixture_library_root`]): its spec is read, built and
/// lowered ([`library::load`]) under the same memo. Anything else is a
/// QLC+ `.qxf` relative to `root`.
///
/// `fixture_path` comes from synced rows and from callers on the seam, and is
/// joined onto a directory, so one that would escape `root` is refused rather
/// than read.
///
/// A venue's fixtures share a handful of definitions, and re-reading and
/// re-parsing a `.qxf` per fixture was most of the cost of deriving a group
/// tree. The modification time is stored *beside* the entry rather than in its
/// key, so a definition replaced on disk is re-read and the superseded parse is
/// dropped: keying on `(path, mtime)` bounds the memo by how many times the
/// library was ever edited, which is not a bound.
///
/// # Errors
/// The path escapes `root`, or the file cannot be read or parsed.
pub fn definition(root: &Path, fixture_path: &str) -> Result<Arc<FixtureDefinition>, String> {
    /// One entry per file: when it was last read, and what it parsed to.
    type Parsed = HashMap<PathBuf, (Option<SystemTime>, Arc<FixtureDefinition>)>;
    static CACHE: Lazy<Mutex<Parsed>> = Lazy::new(Default::default);

    let escapes = || format!("fixture path escapes the fixtures root: {fixture_path}");
    let path = match library::id(fixture_path) {
        Some(id) => {
            let library = crate::resources::fixture_library_root(Some(root))
                .ok_or_else(|| format!("{fixture_path}: no fixture library is installed"))?;
            confined(id).ok_or_else(escapes)?;
            library::spec_path(&library, id)
        }
        None => root.join(confined(fixture_path).ok_or_else(escapes)?),
    };
    let changed = std::fs::metadata(&path)
        .map_err(|error| format!("{fixture_path}: {error}"))?
        .modified()
        .ok();
    let cache = || CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((_, hit)) = cache().get(&path).filter(|(seen, _)| *seen == changed) {
        return Ok(hit.clone());
    }
    let parsed = Arc::new(if library::id(fixture_path).is_some() {
        library::load(&path).map_err(|error| format!("{fixture_path}: {error}"))?
    } else {
        parser::parse_definition(&path)?
    });
    cache().insert(path, (changed, parsed.clone()));
    Ok(parsed)
}

/// `path`, if it stays inside the directory it is joined onto.
fn confined(path: &str) -> Option<&Path> {
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

    #[test]
    fn a_path_that_escapes_the_root_is_refused() {
        let root = tempfile::tempdir().unwrap();
        for path in ["../../etc/passwd", "a/../../b.qxf", "/etc/passwd"] {
            let error = definition(root.path(), path).expect_err(path);
            assert!(error.contains("escapes"), "{path}: {error}");
        }
        assert!(confined("chauvet/rogue-r2.qxf").is_some());
    }

    #[test]
    fn a_definition_replaced_on_disk_is_read_again() {
        let xml = |model: &str| {
            format!(
                "<FixtureDefinition><Manufacturer>Test</Manufacturer>\
                 <Model>{model}</Model><Type>LED</Type><Mode Name=\"one\"/></FixtureDefinition>"
            )
        };
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("fixture.qxf");
        std::fs::write(&file, xml("First")).unwrap();
        let first = definition(root.path(), "fixture.qxf").unwrap();
        assert!(Arc::ptr_eq(
            &first,
            &definition(root.path(), "fixture.qxf").unwrap()
        ));

        std::fs::write(&file, xml("Second")).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(SystemTime::now() + std::time::Duration::from_secs(60))
            .unwrap();
        assert_eq!(
            definition(root.path(), "fixture.qxf").unwrap().model,
            "Second"
        );
    }
}
