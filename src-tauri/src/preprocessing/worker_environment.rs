//! Filesystem capabilities shared by every Python-backed analysis worker.
//!
//! A worker needs an interpreter, a writable deployment directory, and the
//! bundled Python resources. None of those are UI capabilities. Keeping the
//! resolved paths here lets Tauri and headless hosts run the same scheduler
//! without carrying an `AppHandle` through background work.

use std::path::{Path, PathBuf};

use crate::python_env;

#[derive(Clone, Debug)]
pub struct WorkerEnvironment {
    cache_dir: PathBuf,
    resource_dir: Option<PathBuf>,
    wait_for_setup: bool,
}

impl WorkerEnvironment {
    #[must_use]
    pub fn new(cache_dir: PathBuf, resource_dir: Option<PathBuf>) -> Self {
        Self {
            cache_dir,
            resource_dir,
            wait_for_setup: false,
        }
    }

    /// Wait for the desktop host's concurrent environment bootstrap instead
    /// of treating its not-yet-created interpreter as a permanent failure.
    #[must_use]
    pub fn wait_for_setup(mut self) -> Self {
        self.wait_for_setup = true;
        self
    }

    pub fn from_env_default() -> Result<Self, String> {
        let cache_dir = if let Some(path) = std::env::var_os("LUMA_CACHE_DIR") {
            PathBuf::from(path)
        } else {
            dirs::cache_dir()
                .map(|path| path.join("com.luma.luma"))
                .ok_or_else(|| "could not locate a cache directory".to_string())?
        };
        let resource_dir = std::env::var_os("LUMA_RESOURCE_DIR").map(PathBuf::from);
        Ok(Self::new(cache_dir, resource_dir))
    }

    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    pub fn python(&self) -> Result<PathBuf, String> {
        let resolve = || {
            let path = python_env::find_existing_venv_python(&self.cache_dir)?;
            if self.wait_for_setup
                && !self
                    .cache_dir
                    .join("python-env/.requirements.hash")
                    .exists()
            {
                return None;
            }
            Some(path)
        };
        if let Some(path) = resolve() {
            return Ok(path);
        }
        if self.wait_for_setup {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15 * 60);
            while std::time::Instant::now() < deadline {
                if let Ok(error) = std::fs::read_to_string(self.cache_dir.join(".python-env-error"))
                {
                    return Err(format!("managed Python environment setup failed: {error}"));
                }
                std::thread::sleep(std::time::Duration::from_millis(250));
                if let Some(path) = resolve() {
                    return Ok(path);
                }
            }
        }
        Err(format!(
            "no managed python environment under {} — run Luma once to create it",
            self.cache_dir.display()
        ))
    }

    pub fn deploy_script(&self, name: &str, source: &str) -> Result<PathBuf, String> {
        python_env::ensure_worker_script_at(&self.cache_dir, name, source)
    }

    pub fn deploy_resource(&self, relative: &str) -> Result<PathBuf, String> {
        python_env::ensure_python_resource_dir_at(
            &self.cache_dir,
            self.resource_dir.as_deref(),
            relative,
        )
    }
}
