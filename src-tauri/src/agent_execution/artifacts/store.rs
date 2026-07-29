//! The artifact store: one workspace directory, one identity system, one
//! lifecycle.
//!
//! Layout (design §9.5 / decision D6):
//!
//! ```text
//! agent-workspaces/<thread-id>/
//!   inputs/    read-only inside the sandbox — host-placed data for the worker
//!   scratch/   writable inside the sandbox — the worker's cwd
//!   outputs/   host-registered generated artifacts (figures, exports)
//! ```
//!
//! Everything the worker can see is workspace-relative; original music-library
//! paths never cross the boundary. Imports resolve symlinks first and land as
//! hard links when the filesystem allows it, falling back to a copy.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::agent_execution::artifacts::codecs;
use crate::agent_execution::bindings::manifest::{ArtifactId, BindingRevision};
use crate::agent_execution::error::{err, Result};

pub const INPUTS_DIR: &str = "inputs";
pub const SCRATCH_DIR: &str = "scratch";
pub const OUTPUTS_DIR: &str = "outputs";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    Tensor,
    Figure,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactEncoding {
    RawLe,
    Npy,
    PcmF32,
    Png,
    Utf8,
}

impl ArtifactEncoding {
    pub fn as_str(self) -> &'static str {
        match self {
            ArtifactEncoding::RawLe => "raw_le",
            ArtifactEncoding::Npy => "npy",
            ArtifactEncoding::PcmF32 => "pcm_f32",
            ArtifactEncoding::Png => "png",
            ArtifactEncoding::Utf8 => "utf8",
        }
    }

    fn extension(self) -> &'static str {
        match self {
            ArtifactEncoding::RawLe => "bin",
            ArtifactEncoding::Npy => "npy",
            ArtifactEncoding::PcmF32 => "pcm",
            ArtifactEncoding::Png => "png",
            ArtifactEncoding::Utf8 => "txt",
        }
    }
}

/// The manifest-facing view of an artifact. `id` is the map key on the wire, so
/// it is not serialized inside the value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtifactDescriptor {
    #[serde(skip)]
    pub id: ArtifactId,
    pub kind: ArtifactKind,
    pub encoding: ArtifactEncoding,
    /// Always workspace-relative, always forward-slashed.
    pub rel_path: String,
    pub byte_len: u64,
    pub content_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_rate_hz: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channels: Option<u16>,
}

/// Who owns the bytes. Only imported inputs are garbage-collected; generated
/// outputs are results the agent may still be referring to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactOwnership {
    /// Placed under `inputs/` by the host for one or more revisions.
    Imported,
    /// Produced under `outputs/` by the worker and registered afterwards.
    Generated,
}

#[derive(Debug, Clone)]
struct Entry {
    descriptor: ArtifactDescriptor,
    ownership: ArtifactOwnership,
}

/// What to import and how to describe it.
pub struct ImportRequest<'a> {
    pub source: &'a Path,
    pub kind: ArtifactKind,
    pub encoding: ArtifactEncoding,
    /// Compute a `sha256:` content hash. Off by default: hashing a 200 MB PCM
    /// cache on every revision is not free.
    pub hash: bool,
}

impl<'a> ImportRequest<'a> {
    pub fn new(source: &'a Path, kind: ArtifactKind, encoding: ArtifactEncoding) -> Self {
        Self {
            source,
            kind,
            encoding,
            hash: false,
        }
    }

    pub fn hashed(mut self) -> Self {
        self.hash = true;
        self
    }
}

/// How the bytes got into `inputs/`. Reported so callers can see whether a
/// filesystem boundary forced a copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlacementMethod {
    HardLink,
    Copy,
}

pub struct ArtifactStore {
    root: PathBuf,
    entries: BTreeMap<ArtifactId, Entry>,
    leases: BTreeMap<BindingRevision, BTreeSet<ArtifactId>>,
    last_placement: Option<PlacementMethod>,
}

impl ArtifactStore {
    /// Open (creating if needed) a workspace at `root` with the D6 layout.
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref();
        fs::create_dir_all(root)?;
        let root = fs::canonicalize(root)?;
        for sub in [INPUTS_DIR, SCRATCH_DIR, OUTPUTS_DIR] {
            fs::create_dir_all(root.join(sub))?;
        }
        Ok(Self {
            root,
            entries: BTreeMap::new(),
            leases: BTreeMap::new(),
            last_placement: None,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn inputs_dir(&self) -> PathBuf {
        self.root.join(INPUTS_DIR)
    }

    pub fn scratch_dir(&self) -> PathBuf {
        self.root.join(SCRATCH_DIR)
    }

    pub fn outputs_dir(&self) -> PathBuf {
        self.root.join(OUTPUTS_DIR)
    }

    /// How the most recent import placed its bytes.
    pub fn last_placement(&self) -> Option<PlacementMethod> {
        self.last_placement
    }

    pub fn descriptor(&self, id: &ArtifactId) -> Option<&ArtifactDescriptor> {
        self.entries.get(id).map(|e| &e.descriptor)
    }

    pub fn ownership(&self, id: &ArtifactId) -> Option<ArtifactOwnership> {
        self.entries.get(id).map(|e| e.ownership)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn ids(&self) -> Vec<ArtifactId> {
        self.entries.keys().cloned().collect()
    }

    /// Resolve a workspace-relative path to an absolute one, rejecting anything
    /// that could escape the workspace.
    pub fn resolve(&self, rel_path: &str) -> Result<PathBuf> {
        let rel = Path::new(rel_path);
        if rel.is_absolute() {
            return err(format!("artifact path '{rel_path}' must be relative"));
        }
        for component in rel.components() {
            match component {
                Component::Normal(_) => {}
                Component::CurDir => {}
                _ => {
                    return err(format!(
                        "artifact path '{rel_path}' must not traverse outside the workspace"
                    ))
                }
            }
        }
        let joined = self.root.join(rel);
        // Canonicalize the existing prefix so a symlinked component cannot point
        // outside the workspace.
        let anchor = joined.parent().unwrap_or(&self.root);
        let anchor = fs::canonicalize(anchor)
            .map_err(|e| dpe(format!("cannot resolve '{rel_path}': {e}")))?;
        if !anchor.starts_with(&self.root) {
            return err(format!(
                "artifact path '{rel_path}' resolves outside the workspace"
            ));
        }
        let name = joined
            .file_name()
            .ok_or_else(|| dpe(format!("artifact path '{rel_path}' has no file name")))?;
        Ok(anchor.join(name))
    }

    // -----------------------------------------------------------------------
    // Import
    // -----------------------------------------------------------------------

    /// Place an existing host file under `inputs/` and register it.
    pub fn import(&mut self, request: ImportRequest<'_>) -> Result<ArtifactDescriptor> {
        let source = fs::canonicalize(request.source)
            .map_err(|e| dpe(format!("cannot import '{}': {e}", request.source.display())))?;
        // canonicalize resolved every symlink; whatever it landed on must be a
        // real file, and must not be inside the workspace itself.
        let meta = fs::symlink_metadata(&source)?;
        if !meta.is_file() {
            return err(format!(
                "cannot import '{}': not a regular file",
                request.source.display()
            ));
        }
        if source.starts_with(&self.root) {
            return err(format!(
                "cannot import '{}': source is inside the workspace",
                request.source.display()
            ));
        }

        let id = ArtifactId::new();
        let file_name = format!("{}.{}", id, request.encoding.extension());
        let dest = self.inputs_dir().join(&file_name);
        let method = place(&source, &dest)?;
        self.last_placement = Some(method);

        let byte_len = fs::metadata(&dest)?.len();
        let (sample_rate_hz, channels) = if request.encoding == ArtifactEncoding::PcmF32 {
            match codecs::read_pcm_header(&dest) {
                Ok(h) => (Some(h.sample_rate), Some(h.channels)),
                Err(e) => {
                    let _ = fs::remove_file(&dest);
                    return Err(e);
                }
            }
        } else {
            (None, None)
        };
        if request.encoding == ArtifactEncoding::Npy {
            if let Err(e) = codecs::read_npy_header(&dest) {
                let _ = fs::remove_file(&dest);
                return Err(e);
            }
        }

        let content_hash = if request.hash {
            Some(hash_file(&dest)?)
        } else {
            None
        };

        let descriptor = ArtifactDescriptor {
            id: id.clone(),
            kind: request.kind,
            encoding: request.encoding,
            rel_path: format!("{INPUTS_DIR}/{file_name}"),
            byte_len,
            content_hash,
            sample_rate_hz,
            channels,
        };
        self.entries.insert(
            id,
            Entry {
                descriptor: descriptor.clone(),
                ownership: ArtifactOwnership::Imported,
            },
        );
        Ok(descriptor)
    }

    /// Materialize a fresh `Vec<f32>` (a graph view, a beat list, …) as a
    /// headerless little-endian input artifact.
    pub fn write_raw_f32(&mut self, data: &[f32]) -> Result<ArtifactDescriptor> {
        self.write_input(ArtifactEncoding::RawLe, ArtifactKind::Tensor, |path| {
            codecs::raw_le::write_f32(path, data)
        })
    }

    pub fn write_raw_f64(&mut self, data: &[f64]) -> Result<ArtifactDescriptor> {
        self.write_input(ArtifactEncoding::RawLe, ArtifactKind::Tensor, |path| {
            codecs::raw_le::write_f64(path, data)
        })
    }

    pub fn write_raw_i64(&mut self, data: &[i64]) -> Result<ArtifactDescriptor> {
        self.write_input(ArtifactEncoding::RawLe, ArtifactKind::Tensor, |path| {
            codecs::raw_le::write_i64(path, data)
        })
    }

    pub fn write_npy_f32(&mut self, data: &[f32], shape: &[usize]) -> Result<ArtifactDescriptor> {
        self.write_input(ArtifactEncoding::Npy, ArtifactKind::Tensor, |path| {
            codecs::write_npy_f32(path, data, shape)
        })
    }

    /// Write a bounded UTF-8 input artifact (catalogs, exported proposals).
    pub fn write_utf8(&mut self, text: &str) -> Result<ArtifactDescriptor> {
        self.write_input(ArtifactEncoding::Utf8, ArtifactKind::Json, |path| {
            fs::write(path, text)?;
            Ok(text.len() as u64)
        })
    }

    fn write_input<F>(
        &mut self,
        encoding: ArtifactEncoding,
        kind: ArtifactKind,
        write: F,
    ) -> Result<ArtifactDescriptor>
    where
        F: FnOnce(&Path) -> Result<u64>,
    {
        let id = ArtifactId::new();
        let file_name = format!("{}.{}", id, encoding.extension());
        let dest = self.inputs_dir().join(&file_name);
        let byte_len = write(&dest)?;
        let descriptor = ArtifactDescriptor {
            id: id.clone(),
            kind,
            encoding,
            rel_path: format!("{INPUTS_DIR}/{file_name}"),
            byte_len,
            content_hash: None,
            sample_rate_hz: None,
            channels: None,
        };
        self.entries.insert(
            id,
            Entry {
                descriptor: descriptor.clone(),
                ownership: ArtifactOwnership::Imported,
            },
        );
        Ok(descriptor)
    }

    /// Register a file the worker produced under `outputs/` (a figure). The
    /// worker reports a workspace-relative path; nothing else is trusted.
    pub fn register_output(
        &mut self,
        rel_path: &str,
        kind: ArtifactKind,
        encoding: ArtifactEncoding,
    ) -> Result<ArtifactDescriptor> {
        let expected_prefix = format!("{OUTPUTS_DIR}/");
        if !rel_path.starts_with(&expected_prefix) {
            return err(format!(
                "generated artifact '{rel_path}' must live under '{OUTPUTS_DIR}/'"
            ));
        }
        let abs = self.resolve(rel_path)?;
        let meta = fs::symlink_metadata(&abs)
            .map_err(|e| dpe(format!("generated artifact '{rel_path}': {e}")))?;
        if meta.file_type().is_symlink() || !meta.is_file() {
            return err(format!(
                "generated artifact '{rel_path}' is not a regular file"
            ));
        }
        let byte_len = if encoding == ArtifactEncoding::Png {
            codecs::read_png_info(&abs)?.byte_len
        } else {
            meta.len()
        };

        let id = ArtifactId::new();
        let descriptor = ArtifactDescriptor {
            id: id.clone(),
            kind,
            encoding,
            rel_path: rel_path.to_string(),
            byte_len,
            content_hash: None,
            sample_rate_hz: None,
            channels: None,
        };
        self.entries.insert(
            id,
            Entry {
                descriptor: descriptor.clone(),
                ownership: ArtifactOwnership::Generated,
            },
        );
        Ok(descriptor)
    }

    // -----------------------------------------------------------------------
    // Leases
    // -----------------------------------------------------------------------

    /// Pin a set of artifacts for the lifetime of one binding revision.
    pub fn lease<I: IntoIterator<Item = ArtifactId>>(
        &mut self,
        revision: &BindingRevision,
        ids: I,
    ) -> Result<()> {
        let ids: BTreeSet<ArtifactId> = ids.into_iter().collect();
        for id in &ids {
            if !self.entries.contains_key(id) {
                return err(format!("cannot lease unknown artifact '{id}'"));
            }
        }
        self.leases.entry(revision.clone()).or_default().extend(ids);
        Ok(())
    }

    pub fn leased_by(&self, revision: &BindingRevision) -> Option<&BTreeSet<ArtifactId>> {
        self.leases.get(revision)
    }

    pub fn is_leased(&self, id: &ArtifactId) -> bool {
        self.leases.values().any(|set| set.contains(id))
    }

    /// Drop a revision's lease and collect anything no other revision still
    /// holds. Returns the collected ids.
    pub fn release(&mut self, revision: &BindingRevision) -> Result<Vec<ArtifactId>> {
        let Some(dropped) = self.leases.remove(revision) else {
            return Ok(Vec::new());
        };
        let mut collected = Vec::new();
        for id in dropped {
            if self.is_leased(&id) {
                continue;
            }
            let Some(entry) = self.entries.get(&id) else {
                continue;
            };
            if entry.ownership != ArtifactOwnership::Imported {
                continue;
            }
            let path = self.resolve(&entry.descriptor.rel_path)?;
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            self.entries.remove(&id);
            collected.push(id);
        }
        Ok(collected)
    }

    /// Collect every imported artifact no revision holds a lease on. Used after
    /// an app restart, where in-memory leases are gone by definition.
    pub fn collect_unleased(&mut self) -> Result<Vec<ArtifactId>> {
        let orphans: Vec<ArtifactId> = self
            .entries
            .iter()
            .filter(|(id, e)| e.ownership == ArtifactOwnership::Imported && !self.is_leased(id))
            .map(|(id, _)| id.clone())
            .collect();
        let mut collected = Vec::new();
        for id in orphans {
            let rel = self.entries[&id].descriptor.rel_path.clone();
            let path = self.resolve(&rel)?;
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            self.entries.remove(&id);
            collected.push(id);
        }
        Ok(collected)
    }

    /// Write a manifest into `inputs/` and return its workspace-relative path.
    pub fn write_manifest(
        &self,
        manifest: &crate::agent_execution::bindings::manifest::BindingManifest,
    ) -> Result<String> {
        let rel = manifest.rel_path();
        let path = self.inputs_dir().join(manifest.file_name());
        fs::write(path, manifest.to_json()?)?;
        Ok(rel)
    }
}

/// Hard link if the filesystem allows it, copy otherwise (cross-device, or a
/// filesystem without links).
fn place(source: &Path, dest: &Path) -> Result<PlacementMethod> {
    match fs::hard_link(source, dest) {
        Ok(()) => Ok(PlacementMethod::HardLink),
        Err(_) => {
            fs::copy(source, dest).map_err(|e| {
                dpe(format!(
                    "failed to copy '{}' into the workspace: {e}",
                    source.display()
                ))
            })?;
            Ok(PlacementMethod::Copy)
        }
    }
}

fn hash_file(path: &Path) -> Result<String> {
    let mut f = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

fn dpe(msg: String) -> crate::agent_execution::error::DataPlaneError {
    crate::agent_execution::error::DataPlaneError::new(msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, ArtifactStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = ArtifactStore::open(dir.path().join("thread-1")).unwrap();
        (dir, store)
    }

    fn source_file(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn open_creates_the_d6_layout() {
        let (_d, s) = store();
        assert!(s.inputs_dir().is_dir());
        assert!(s.scratch_dir().is_dir());
        assert!(s.outputs_dir().is_dir());
        assert!(s.is_empty());
    }

    #[test]
    fn import_hardlinks_within_a_filesystem() {
        let (d, mut s) = store();
        let src = source_file(d.path(), "beats.bin", &[0u8; 16]);
        let desc = s
            .import(ImportRequest::new(
                &src,
                ArtifactKind::Tensor,
                ArtifactEncoding::RawLe,
            ))
            .unwrap();
        assert_eq!(s.last_placement(), Some(PlacementMethod::HardLink));
        assert!(desc.rel_path.starts_with("inputs/"));
        assert_eq!(desc.byte_len, 16);
        assert!(desc.content_hash.is_none());

        // A hard link is the same inode, so the source is not duplicated.
        let abs = s.resolve(&desc.rel_path).unwrap();
        assert!(abs.exists());
        assert_eq!(fs::read(&abs).unwrap(), vec![0u8; 16]);
        assert!(abs.starts_with(s.root()));
    }

    #[test]
    fn import_falls_back_to_copy_when_linking_fails() {
        let (d, s) = store();
        let src = source_file(d.path(), "a.bin", &[1u8; 8]);
        // Pre-create the destination name is not possible (ids are random), so
        // exercise `place` directly against an existing destination, which is
        // what a cross-device link error looks like from the caller's side.
        let dest = s.inputs_dir().join("dest.bin");
        fs::write(&dest, b"placeholder").unwrap();
        assert_eq!(place(&src, &dest).unwrap(), PlacementMethod::Copy);
        assert_eq!(fs::read(&dest).unwrap(), vec![1u8; 8]);
    }

    #[test]
    fn import_can_hash_content() {
        let (d, mut s) = store();
        let src = source_file(d.path(), "a.bin", b"hello");
        let desc = s
            .import(
                ImportRequest::new(&src, ArtifactKind::Tensor, ArtifactEncoding::RawLe).hashed(),
            )
            .unwrap();
        assert_eq!(
            desc.content_hash.unwrap(),
            "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn importing_pcm_fills_sample_rate_and_channels() {
        let (d, mut s) = store();
        let src = d.path().join("mix.pcm");
        codecs::write_pcm(&src, 2, 48000, 2, &[0.0; 8]).unwrap();
        let desc = s
            .import(ImportRequest::new(
                &src,
                ArtifactKind::Tensor,
                ArtifactEncoding::PcmF32,
            ))
            .unwrap();
        assert_eq!(desc.sample_rate_hz, Some(48000));
        assert_eq!(desc.channels, Some(2));
        assert_eq!(desc.byte_len, 18 + 32);
    }

    #[test]
    fn importing_a_corrupt_pcm_fails_and_leaves_nothing_behind() {
        let (d, mut s) = store();
        let src = source_file(d.path(), "bad.pcm", &[0u8; 10]);
        assert!(s
            .import(ImportRequest::new(
                &src,
                ArtifactKind::Tensor,
                ArtifactEncoding::PcmF32
            ))
            .is_err());
        assert!(s.is_empty());
        assert_eq!(fs::read_dir(s.inputs_dir()).unwrap().count(), 0);
    }

    #[test]
    fn importing_a_corrupt_npy_fails() {
        let (d, mut s) = store();
        let src = source_file(d.path(), "bad.npy", b"nope");
        assert!(s
            .import(ImportRequest::new(
                &src,
                ArtifactKind::Tensor,
                ArtifactEncoding::Npy
            ))
            .is_err());
        assert_eq!(fs::read_dir(s.inputs_dir()).unwrap().count(), 0);
    }

    #[test]
    #[cfg(unix)]
    fn import_resolves_symlinks_to_their_target() {
        let (d, mut s) = store();
        let real = source_file(d.path(), "real.bin", &[7u8; 4]);
        let link = d.path().join("link.bin");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let desc = s
            .import(ImportRequest::new(
                &link,
                ArtifactKind::Tensor,
                ArtifactEncoding::RawLe,
            ))
            .unwrap();
        let abs = s.resolve(&desc.rel_path).unwrap();
        assert!(!fs::symlink_metadata(&abs).unwrap().file_type().is_symlink());
        assert_eq!(fs::read(abs).unwrap(), vec![7u8; 4]);
    }

    #[test]
    #[cfg(unix)]
    fn import_rejects_dangling_symlinks_and_directories() {
        let (d, mut s) = store();
        let link = d.path().join("dangling");
        std::os::unix::fs::symlink(d.path().join("nope"), &link).unwrap();
        assert!(s
            .import(ImportRequest::new(
                &link,
                ArtifactKind::Tensor,
                ArtifactEncoding::RawLe
            ))
            .is_err());

        let sub = d.path().join("adir");
        fs::create_dir(&sub).unwrap();
        let e = s
            .import(ImportRequest::new(
                &sub,
                ArtifactKind::Tensor,
                ArtifactEncoding::RawLe,
            ))
            .unwrap_err();
        assert!(e.message().contains("not a regular file"), "{e}");
    }

    #[test]
    fn import_rejects_sources_inside_the_workspace() {
        let (_d, mut s) = store();
        let inside = s.scratch_dir().join("agent-wrote-this.bin");
        fs::write(&inside, b"x").unwrap();
        let e = s
            .import(ImportRequest::new(
                &inside,
                ArtifactKind::Tensor,
                ArtifactEncoding::RawLe,
            ))
            .unwrap_err();
        assert!(e.message().contains("inside the workspace"), "{e}");
    }

    #[test]
    fn resolve_rejects_traversal_and_absolute_paths() {
        let (_d, s) = store();
        for bad in [
            "../outside.bin",
            "inputs/../../outside.bin",
            "inputs/../../../etc/passwd",
        ] {
            let e = s.resolve(bad).unwrap_err();
            assert!(e.message().contains("traverse outside"), "{bad}: {e}");
        }
        assert!(s
            .resolve("/etc/passwd")
            .unwrap_err()
            .message()
            .contains("relative"));
    }

    #[test]
    #[cfg(unix)]
    fn resolve_rejects_paths_through_a_symlinked_directory() {
        let (d, s) = store();
        let outside = d.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("secret.bin"), b"s").unwrap();
        std::os::unix::fs::symlink(&outside, s.inputs_dir().join("escape")).unwrap();
        let e = s.resolve("inputs/escape/secret.bin").unwrap_err();
        assert!(e.message().contains("resolves outside"), "{e}");
    }

    #[test]
    fn register_output_accepts_only_files_under_outputs() {
        let (_d, mut s) = store();
        let fig = s.outputs_dir().join("fig-1.png");
        let mut png = Vec::new();
        png.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&1200u32.to_be_bytes());
        png.extend_from_slice(&400u32.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
        fs::write(&fig, &png).unwrap();

        let desc = s
            .register_output(
                "outputs/fig-1.png",
                ArtifactKind::Figure,
                ArtifactEncoding::Png,
            )
            .unwrap();
        assert_eq!(desc.rel_path, "outputs/fig-1.png");
        assert_eq!(desc.byte_len, png.len() as u64);
        assert_eq!(s.ownership(&desc.id), Some(ArtifactOwnership::Generated));

        let e = s
            .register_output(
                "inputs/fig-1.png",
                ArtifactKind::Figure,
                ArtifactEncoding::Png,
            )
            .unwrap_err();
        assert!(e.message().contains("must live under 'outputs/'"), "{e}");
        assert!(s
            .register_output(
                "outputs/../inputs/fig-1.png",
                ArtifactKind::Figure,
                ArtifactEncoding::Png
            )
            .is_err());
    }

    #[test]
    #[cfg(unix)]
    fn register_output_rejects_symlinked_files() {
        let (d, mut s) = store();
        let real = source_file(d.path(), "real.png", b"whatever");
        std::os::unix::fs::symlink(&real, s.outputs_dir().join("fig.png")).unwrap();
        let e = s
            .register_output(
                "outputs/fig.png",
                ArtifactKind::Figure,
                ArtifactEncoding::Png,
            )
            .unwrap_err();
        assert!(e.message().contains("not a regular file"), "{e}");
    }

    #[test]
    fn fresh_vectors_can_be_materialized_as_inputs() {
        let (_d, mut s) = store();
        let desc = s.write_raw_f32(&[1.0, 2.0, 3.0]).unwrap();
        assert_eq!(desc.byte_len, 12);
        assert_eq!(desc.encoding, ArtifactEncoding::RawLe);
        let abs = s.resolve(&desc.rel_path).unwrap();
        assert_eq!(
            codecs::raw_le::read_f32(&abs, 0, 3).unwrap(),
            [1.0, 2.0, 3.0]
        );

        let npy = s.write_npy_f32(&[1.0, 2.0, 3.0, 4.0], &[2, 2]).unwrap();
        let h = codecs::read_npy_header(&s.resolve(&npy.rel_path).unwrap()).unwrap();
        assert_eq!(h.shape, vec![2, 2]);
    }

    #[test]
    fn leases_keep_inputs_alive_until_every_revision_releases() {
        let (_d, mut s) = store();
        let a = s.write_raw_f32(&[1.0]).unwrap();
        let b = s.write_raw_f32(&[2.0]).unwrap();
        let r1 = BindingRevision::new();
        let r2 = BindingRevision::new();
        s.lease(&r1, [a.id.clone(), b.id.clone()]).unwrap();
        s.lease(&r2, [a.id.clone()]).unwrap();

        let collected = s.release(&r1).unwrap();
        assert_eq!(collected, vec![b.id.clone()]);
        assert!(!s.inputs_dir().join(file_name(&b.rel_path)).exists());
        assert!(s.inputs_dir().join(file_name(&a.rel_path)).exists());
        assert!(s.descriptor(&b.id).is_none());
        assert!(s.descriptor(&a.id).is_some());

        let collected = s.release(&r2).unwrap();
        assert_eq!(collected, vec![a.id.clone()]);
        assert!(!s.inputs_dir().join(file_name(&a.rel_path)).exists());
        assert!(s.is_empty());
    }

    fn file_name(rel: &str) -> String {
        rel.rsplit('/').next().unwrap().to_string()
    }

    #[test]
    fn releasing_an_unknown_revision_is_a_no_op() {
        let (_d, mut s) = store();
        assert!(s.release(&BindingRevision::new()).unwrap().is_empty());
    }

    #[test]
    fn leasing_an_unknown_artifact_is_rejected() {
        let (_d, mut s) = store();
        let e = s
            .lease(&BindingRevision::new(), [ArtifactId::from_string("a-nope")])
            .unwrap_err();
        assert!(e.message().contains("cannot lease unknown artifact"), "{e}");
    }

    #[test]
    fn generated_outputs_survive_garbage_collection() {
        let (_d, mut s) = store();
        let fig = s.outputs_dir().join("fig.png");
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&8u32.to_be_bytes());
        png.extend_from_slice(&8u32.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
        fs::write(&fig, png).unwrap();
        let desc = s
            .register_output(
                "outputs/fig.png",
                ArtifactKind::Figure,
                ArtifactEncoding::Png,
            )
            .unwrap();
        let r = BindingRevision::new();
        s.lease(&r, [desc.id.clone()]).unwrap();
        assert!(s.release(&r).unwrap().is_empty());
        assert!(fig.exists());
        assert!(s.descriptor(&desc.id).is_some());
    }

    #[test]
    fn collect_unleased_removes_orphaned_inputs_only() {
        let (_d, mut s) = store();
        let a = s.write_raw_f32(&[1.0]).unwrap();
        let b = s.write_raw_f32(&[2.0]).unwrap();
        let r = BindingRevision::new();
        s.lease(&r, [a.id.clone()]).unwrap();
        let collected = s.collect_unleased().unwrap();
        assert_eq!(collected, vec![b.id]);
        assert!(s.descriptor(&a.id).is_some());
    }

    #[test]
    fn manifest_is_written_into_inputs() {
        use crate::agent_execution::bindings::assembler::BindingBuilder;
        use crate::agent_execution::bindings::manifest::{AgentKind, AnalysisScope};

        let (_d, mut s) = store();
        let a = s.write_raw_f32(&[1.0, 2.0]).unwrap();
        let mut builder = BindingBuilder::new(AgentKind::TrackCopilot, AnalysisScope::default());
        builder.artifact(a.clone()).unwrap();
        builder
            .tensor(
                "features.beats",
                crate::agent_execution::bindings::manifest::TensorRef::new(
                    a.id.clone(),
                    crate::agent_execution::bindings::manifest::DType::F32,
                    vec![2],
                    vec![crate::agent_execution::bindings::manifest::AxisSpec::index(
                        "event", 2,
                    )],
                    crate::agent_execution::bindings::manifest::Provenance::new("beat_this"),
                ),
            )
            .unwrap();
        let manifest = builder.build().unwrap();
        let rel = s.write_manifest(&manifest).unwrap();
        assert!(rel.starts_with("inputs/manifest-r-"));
        let text = fs::read_to_string(s.resolve(&rel).unwrap()).unwrap();
        assert_eq!(text, manifest.to_json().unwrap());
    }
}
