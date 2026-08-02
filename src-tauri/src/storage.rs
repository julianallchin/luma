//! The one place that knows where Luma's on-disk data lives.
//!
//! Every durable artifact (tracks, stems, PCM caches, MERT features, agent
//! workspaces) hangs off a single root — the Tauri **app config dir**. Before
//! this module, that root was reconstructed three different ways: from an
//! `AppHandle` in the app, and from a hardcoded `$HOME/Library/Application
//! Support/com.luma.luma` literal in `eval/context.rs` and the dev binaries.
//! The literal was macOS-only and silently degraded to "no stems, no cache" off
//! macOS.
//!
//! [`StorageRoot`] is a plain `PathBuf` newtype with pure path accessors, so
//! headless callers (golden harness, `dump_venue`, the agent harness) get the
//! exact same layout as the app without needing an `AppHandle`.
//!
//! Path construction only — the single exception is
//! [`StorageRoot::ensure_track_storage`], which replaces
//! `services::tracks::ensure_storage`.

use std::path::{Path, PathBuf};

use tauri::{AppHandle, Manager};

/// Bundle identifier — must match `tauri.conf.json` `identifier`.
const APP_IDENTIFIER: &str = "com.luma.luma";

/// Root of Luma's durable on-disk data (the Tauri app config dir).
///
/// macOS: `~/Library/Application Support/com.luma.luma`
/// Linux: `~/.config/com.luma.luma`
/// Windows: `%APPDATA%\com.luma.luma`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageRoot(PathBuf);

impl StorageRoot {
    /// The app's resolved config dir. This is the only correct constructor when
    /// an `AppHandle` is in hand — it honors whatever Tauri resolved at runtime.
    pub fn from_app(app: &AppHandle) -> Result<Self, String> {
        app.path()
            .app_config_dir()
            .map(Self)
            .map_err(|e| format!("Failed to locate app config dir: {e}"))
    }

    /// Wrap an explicit root. For tests and for harness binaries that already
    /// resolved a path themselves.
    pub fn from_path(p: PathBuf) -> Self {
        Self(p)
    }

    /// Reproduce Tauri's `app_config_dir` without an `AppHandle`, for binaries
    /// that link `luma_lib` but never start the app (golden harness,
    /// `dump_venue`). `dirs::config_dir()` is per-OS exactly what Tauri uses.
    pub fn from_env_default() -> Result<Self, String> {
        let base = dirs::config_dir()
            .ok_or_else(|| "Failed to locate the user config directory".to_string())?;
        Ok(Self(base.join(APP_IDENTIFIER)))
    }

    /// The root itself.
    pub fn path(&self) -> &Path {
        &self.0
    }

    /// The global library database.
    pub fn luma_db_path(&self) -> PathBuf {
        self.0.join("luma.db")
    }

    // -- tracks ---------------------------------------------------------------

    /// `<root>/tracks` — imported audio files live directly here as `<hash>.<ext>`.
    pub fn tracks_dir(&self) -> PathBuf {
        self.0.join("tracks")
    }

    /// `<root>/tracks/art` — extracted album art.
    pub fn art_dir(&self) -> PathBuf {
        self.tracks_dir().join("art")
    }

    /// `<root>/tracks/stems` — parent of the per-track stem directories.
    pub fn stems_root(&self) -> PathBuf {
        self.tracks_dir().join("stems")
    }

    /// `<root>/tracks/mert` — MERT-95M layer-7 feature `.npy` cache.
    pub fn mert_dir(&self) -> PathBuf {
        self.tracks_dir().join("mert")
    }

    /// Recoverable staging area for track deletion. A manifest lets startup
    /// roll files back when SQLite still contains the track, or finish removal
    /// after the catalog transaction committed.
    pub fn track_deletion_trash_dir(&self) -> PathBuf {
        self.tracks_dir().join(".deleting")
    }

    // -- PCM caches -----------------------------------------------------------

    /// `<root>/tracks/cache/<hash>.pcm` — the full stereo-interleaved decode at
    /// [`crate::services::tracks::TARGET_SAMPLE_RATE`].
    ///
    /// Note this is also what [`crate::audio::cache`] derives file-relatively
    /// from a track under `tracks/` — see that module's `cache_dir_for_track`.
    pub fn mix_pcm_path(&self, track_hash: &str) -> PathBuf {
        self.tracks_dir()
            .join("cache")
            .join(format!("{track_hash}.pcm"))
    }

    /// `<root>/tracks/cache/<hash>_eval_mono.pcm` — mono-at-analysis-rate audio
    /// for the eval engine (skips decode + downmix on subsequent sessions).
    pub fn eval_mono_pcm_path(&self, track_hash: &str) -> PathBuf {
        self.tracks_dir()
            .join("cache")
            .join(format!("{track_hash}_eval_mono.pcm"))
    }

    /// `<root>/tracks/stems/<hash>` — one track's separated stems.
    pub fn stems_dir(&self, track_hash: &str) -> PathBuf {
        self.stems_root().join(track_hash)
    }

    /// `<root>/tracks/stems/<hash>/cache/<hash>_stem_<stem>.pcm` — the decoded
    /// stem PCM. The doubled hash is not redundant by accident: the stem cache is
    /// written by [`crate::audio::cache`] keyed on the synthetic tag
    /// `"{hash}_stem_{name}"` relative to the stem file's own directory.
    pub fn stem_pcm_path(&self, track_hash: &str, stem_name: &str) -> PathBuf {
        self.stems_dir(track_hash)
            .join("cache")
            .join(format!("{track_hash}_stem_{stem_name}.pcm"))
    }

    /// The compressed stem source on disk, probing `.ogg` → `.flac` → `.wav`
    /// (older runs wrote the latter two). `None` when the stem isn't separated.
    pub fn stem_source_path(&self, track_hash: &str, stem_name: &str) -> Option<PathBuf> {
        find_stem_file(&self.stems_dir(track_hash), stem_name)
    }

    /// `<root>/tracks/mert/<hash>.fullmix.npy`
    pub fn mert_fullmix_path(&self, track_hash: &str) -> PathBuf {
        self.mert_dir().join(format!("{track_hash}.fullmix.npy"))
    }

    /// `<root>/tracks/mert/<hash>.drum.npy`
    pub fn mert_drum_path(&self, track_hash: &str) -> PathBuf {
        self.mert_dir().join(format!("{track_hash}.drum.npy"))
    }

    // -- agent execution ------------------------------------------------------

    /// `<root>/agent-workspaces` — parent of the per-thread python workspaces
    /// (`<thread-id>/{inputs,scratch,outputs}`).
    pub fn agent_workspaces_dir(&self) -> PathBuf {
        self.0.join("agent-workspaces")
    }

    /// One agent thread's workspace.
    pub fn agent_workspace_dir(&self, thread_id: &str) -> PathBuf {
        self.agent_workspaces_dir().join(thread_id)
    }

    // -- authored-state repositories ----------------------------------------

    /// `<root>/authored-state` — durable Git repositories plus disposable
    /// linked worktrees used for isolated authored-document edits.
    pub fn authored_state_dir(&self) -> PathBuf {
        self.0.join("authored-state")
    }

    /// `<root>/authored-state/repos` — bare Git repositories, one per bounded
    /// authored document.
    pub fn authored_repositories_dir(&self) -> PathBuf {
        self.authored_state_dir().join("repos")
    }

    /// `<root>/authored-state/repos/<repo-id>.git`.
    pub fn authored_repository_dir(&self, repository_id: &str) -> PathBuf {
        self.authored_repositories_dir()
            .join(format!("{repository_id}.git"))
    }

    /// `<root>/authored-state/worktrees` — parent of every linked worktree.
    pub fn authored_worktrees_dir(&self) -> PathBuf {
        self.authored_state_dir().join("worktrees")
    }

    /// `<root>/authored-state/worktrees/<repo-id>`.
    pub fn authored_repository_worktrees_dir(&self, repository_id: &str) -> PathBuf {
        self.authored_worktrees_dir().join(repository_id)
    }

    /// `<root>/authored-state/worktrees/<repo-id>/<worktree-id>`.
    pub fn authored_worktree_dir(&self, repository_id: &str, worktree_id: &str) -> PathBuf {
        self.authored_repository_worktrees_dir(repository_id)
            .join(worktree_id)
    }

    // -- creation -------------------------------------------------------------

    /// Create the durable track storage tree (`tracks`, `tracks/art`,
    /// `tracks/stems`, `tracks/mert`).
    pub fn ensure_track_storage(&self) -> Result<(), String> {
        for dir in [
            self.tracks_dir(),
            self.art_dir(),
            self.stems_root(),
            self.mert_dir(),
        ] {
            std::fs::create_dir_all(&dir)
                .map_err(|e| format!("Failed to create {}: {e}", dir.display()))?;
        }
        Ok(())
    }
}

/// Find a stem file by name under `stems_dir`, checking `.ogg` first then
/// `.flac` / `.wav` for backwards compatibility with older runs.
pub fn find_stem_file(stems_dir: &Path, stem_name: &str) -> Option<PathBuf> {
    ["ogg", "flac", "wav"]
        .iter()
        .map(|ext| stems_dir.join(format!("{stem_name}.{ext}")))
        .find(|p| p.exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> StorageRoot {
        StorageRoot::from_path(PathBuf::from("/fake/com.luma.luma"))
    }

    /// These literals are the exact strings the pre-centralization call sites
    /// built by hand. Changing one silently orphans every existing cache file.
    #[test]
    fn paths_match_the_legacy_hardcoded_layout() {
        let r = root();
        let h = "abc123";
        assert_eq!(r.tracks_dir(), PathBuf::from("/fake/com.luma.luma/tracks"));
        assert_eq!(r.art_dir(), PathBuf::from("/fake/com.luma.luma/tracks/art"));
        assert_eq!(
            r.stems_root(),
            PathBuf::from("/fake/com.luma.luma/tracks/stems")
        );
        assert_eq!(
            r.mix_pcm_path(h),
            PathBuf::from("/fake/com.luma.luma/tracks/cache/abc123.pcm")
        );
        assert_eq!(
            r.eval_mono_pcm_path(h),
            PathBuf::from("/fake/com.luma.luma/tracks/cache/abc123_eval_mono.pcm")
        );
        assert_eq!(
            r.stems_dir(h),
            PathBuf::from("/fake/com.luma.luma/tracks/stems/abc123")
        );
        assert_eq!(
            r.stem_pcm_path(h, "drums"),
            PathBuf::from("/fake/com.luma.luma/tracks/stems/abc123/cache/abc123_stem_drums.pcm")
        );
        assert_eq!(
            r.mert_fullmix_path(h),
            PathBuf::from("/fake/com.luma.luma/tracks/mert/abc123.fullmix.npy")
        );
        assert_eq!(
            r.mert_drum_path(h),
            PathBuf::from("/fake/com.luma.luma/tracks/mert/abc123.drum.npy")
        );
        assert_eq!(
            r.agent_workspaces_dir(),
            PathBuf::from("/fake/com.luma.luma/agent-workspaces")
        );
        assert_eq!(
            r.agent_workspace_dir("t-1"),
            PathBuf::from("/fake/com.luma.luma/agent-workspaces/t-1")
        );
        assert_eq!(
            r.authored_repository_dir("r-abc"),
            PathBuf::from("/fake/com.luma.luma/authored-state/repos/r-abc.git")
        );
        assert_eq!(
            r.authored_worktree_dir("r-abc", "w-1"),
            PathBuf::from("/fake/com.luma.luma/authored-state/worktrees/r-abc/w-1")
        );
        assert_eq!(
            r.luma_db_path(),
            PathBuf::from("/fake/com.luma.luma/luma.db")
        );
    }

    #[test]
    fn stem_source_probes_ogg_then_flac_then_wav() {
        let tmp = std::env::temp_dir().join(format!("luma-storage-{}", std::process::id()));
        let r = StorageRoot::from_path(tmp.clone());
        let stems = r.stems_dir("h");
        std::fs::create_dir_all(&stems).unwrap();

        assert_eq!(r.stem_source_path("h", "drums"), None);

        std::fs::write(stems.join("drums.wav"), b"x").unwrap();
        assert_eq!(
            r.stem_source_path("h", "drums"),
            Some(stems.join("drums.wav"))
        );

        std::fs::write(stems.join("drums.ogg"), b"x").unwrap();
        assert_eq!(
            r.stem_source_path("h", "drums"),
            Some(stems.join("drums.ogg"))
        );

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn from_env_default_ends_in_the_bundle_identifier() {
        let r = StorageRoot::from_env_default().unwrap();
        assert_eq!(
            r.path().file_name().and_then(|s| s.to_str()),
            Some("com.luma.luma")
        );
    }
}
