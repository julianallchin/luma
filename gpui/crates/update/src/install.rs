//! Where Luma is installed, and swapping a verified release in for it.
//!
//! Both shipped layouts are one directory the update replaces whole:
//!
//! ```text
//! macOS   …/Luma.app/Contents/MacOS/luma      root = Luma.app
//! Linux   …/luma.app/bin/luma                 root = luma.app
//! ```
//!
//! Zed rsyncs the new tree over the running one. A rename is better here: the
//! old tree stays whole until the new one is in place, a failure puts it back,
//! and nothing beyond the standard library is needed. Renames need one file
//! system, so the archive is unpacked into a staging directory beside the
//! root rather than into the system temp directory.

use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Install {
    /// The directory an update replaces.
    pub root: PathBuf,
    layout: Layout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layout {
    MacBundle,
    LinuxTarball,
}

impl Install {
    /// The install `exe` runs from, or why it cannot update itself.
    pub fn locate(exe: &Path) -> Result<Self, String> {
        let (root, layout) = if cfg!(target_os = "macos") {
            (exe.ancestors().nth(3), Layout::MacBundle)
        } else if cfg!(target_os = "linux") {
            (exe.ancestors().nth(2), Layout::LinuxTarball)
        } else {
            return Err("Automatic updates are not available on this platform yet.".into());
        };
        Self::at(root, layout)
            .ok_or_else(|| "Luma is not running from an installed release.".to_string())
            .and_then(Self::writable)
    }

    fn at(root: Option<&Path>, layout: Layout) -> Option<Self> {
        let root = root?;
        let bundle = root.extension().is_some_and(|ext| ext == "app") && root.is_dir();
        bundle.then(|| Self {
            root: root.to_path_buf(),
            layout,
        })
    }

    /// Refuse a root this user cannot replace — a system-wide install, a
    /// read-only image — before anything is downloaded for it.
    fn writable(self) -> Result<Self, String> {
        let parent = self.parent()?;
        if let Err(error) = tempfile::Builder::new()
            .prefix(".luma-update-")
            .tempdir_in(parent)
        {
            return Err(format!(
                "Luma cannot update itself because {} is not writable ({error}).",
                parent.display()
            ));
        }
        Ok(self)
    }

    fn parent(&self) -> Result<&Path, String> {
        self.root
            .parent()
            .ok_or_else(|| format!("{} has no parent directory", self.root.display()))
    }

    /// What to launch after the swap: the bundle on macOS (gpui `open`s it),
    /// the binary inside the new tree on Linux.
    pub fn restart_path(&self) -> PathBuf {
        match self.layout {
            Layout::MacBundle => self.root.clone(),
            Layout::LinuxTarball => self.root.join("bin/luma"),
        }
    }

    /// Replace the install with the one tree inside `archive` (a `.tar.gz`
    /// whose single top-level entry is `*.app`), keeping the root's own name.
    ///
    /// The previous tree goes into the staging directory, which is removed
    /// on return. The running process keeps its open files either way.
    pub fn replace(&self, archive: &Path) -> Result<PathBuf, String> {
        let staging = tempfile::Builder::new()
            .prefix(".luma-update-")
            .tempdir_in(self.parent()?)
            .map_err(|error| format!("could not stage the update: {error}"))?;
        let unpacked = staging.path().join("new");
        unpack(archive, &unpacked)?;
        let incoming = single_app(&unpacked)?;
        let previous = staging.path().join("previous");
        fs::rename(&self.root, &previous)
            .map_err(|error| format!("could not move the current install aside: {error}"))?;
        if let Err(error) = fs::rename(&incoming, &self.root) {
            // Put the old tree back; a half-done swap must not leave nothing.
            return Err(match fs::rename(&previous, &self.root) {
                Ok(()) => format!("could not install the update: {error}"),
                Err(restore) => {
                    // The staging directory now holds the only copy: keep it.
                    let _ = staging.keep();
                    format!(
                        "could not install the update ({error}) or restore {} ({restore}); the previous install is at {}",
                        self.root.display(),
                        previous.display()
                    )
                }
            });
        }
        // Dropping `staging` deletes the previous tree.
        Ok(self.restart_path())
    }
}

fn unpack(archive: &Path, into: &Path) -> Result<(), String> {
    let file = fs::File::open(archive)
        .map_err(|error| format!("could not open {}: {error}", archive.display()))?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
    tar.set_preserve_permissions(true);
    tar.set_overwrite(false);
    tar.unpack(into)
        .map_err(|error| format!("could not unpack the update: {error}"))
}

/// The one `*.app` directory an archive holds. Anything else is not a Luma
/// release and is refused rather than guessed at.
fn single_app(dir: &Path) -> Result<PathBuf, String> {
    let entries: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(|error| format!("could not read the unpacked update: {error}"))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    match entries.as_slice() {
        [only] if only.is_dir() && only.extension().is_some_and(|ext| ext == "app") => {
            Ok(only.clone())
        }
        _ => Err("the update archive does not hold exactly one .app directory".into()),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A `.tar.gz` holding `name/bin/luma` with `contents`.
    pub(crate) fn archive(dir: &Path, name: &str, contents: &str) -> PathBuf {
        let path = dir.join("release.tar.gz");
        let file = fs::File::create(&path).unwrap();
        let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(
            file,
            flate2::Compression::fast(),
        ));
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        tar.append_data(&mut header, format!("{name}/bin/luma"), contents.as_bytes())
            .unwrap();
        tar.into_inner().unwrap().finish().unwrap();
        path
    }

    pub(crate) fn installed(dir: &Path, contents: &str) -> Install {
        let root = dir.join("luma.app");
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::write(root.join("bin/luma"), contents).unwrap();
        fs::write(root.join("stale"), "only in the old tree").unwrap();
        Install::at(Some(&root), Layout::LinuxTarball).unwrap()
    }

    #[test]
    fn replaces_the_tree_and_keeps_its_name() {
        let dir = tempfile::tempdir().unwrap();
        let install = installed(dir.path(), "old");
        // The archive's own name does not matter: an install renamed by its
        // owner keeps the name it was given.
        let archive = archive(dir.path(), "Luma.app", "new");
        let restart = install.replace(&archive).unwrap();
        assert_eq!(restart, install.root.join("bin/luma"));
        assert_eq!(fs::read_to_string(&restart).unwrap(), "new");
        assert!(!install.root.join("stale").exists());
        // Only the install and the archive remain: no staging left behind.
        let left: Vec<_> = fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(left.len(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn keeps_the_executable_bit() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let install = installed(dir.path(), "old");
        let restart = install
            .replace(&archive(dir.path(), "luma.app", "new"))
            .unwrap();
        assert_ne!(
            fs::metadata(restart).unwrap().permissions().mode() & 0o111,
            0
        );
    }

    #[test]
    fn a_foreign_archive_leaves_the_install_alone() {
        let dir = tempfile::tempdir().unwrap();
        let install = installed(dir.path(), "old");
        let archive = archive(dir.path(), "not-an-app", "new");
        assert!(install.replace(&archive).is_err());
        assert_eq!(
            fs::read_to_string(install.root.join("bin/luma")).unwrap(),
            "old"
        );
    }

    #[test]
    fn only_a_bundle_directory_is_an_install() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("target/release");
        fs::create_dir_all(&bin).unwrap();
        assert!(Install::at(bin.parent(), Layout::LinuxTarball).is_none());
    }
}
