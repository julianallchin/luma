//! Filesystem capabilities shared by every Python-backed analysis worker.
//!
//! A worker needs an interpreter, a writable deployment directory, and the
//! bundled Python resources. None of those are UI capabilities. Keeping the
//! resolved paths here lets desktop and headless hosts run the same scheduler
//! without carrying an `AppHandle` through background work.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::preprocessing::AnalysisGuard;
use crate::runtime::python::{self as python_env, BackgroundSetup};

#[derive(Clone, Debug)]
pub struct WorkerEnvironment {
    cache_dir: PathBuf,
    resource_dir: Option<PathBuf>,
    /// The host's background Python setup, when it runs one.
    setup: Option<BackgroundSetup>,
    background: bool,
}

impl WorkerEnvironment {
    #[must_use]
    pub fn new(cache_dir: PathBuf, resource_dir: Option<PathBuf>) -> Self {
        Self {
            cache_dir,
            resource_dir,
            setup: None,
            background: false,
        }
    }

    /// Launch every worker below the UI
    /// ([`crate::runtime::command::background_command`]), for analysis nobody
    /// is waiting on, like the startup catch-up: the stage keeps its frames,
    /// and the batch takes longer.
    #[must_use]
    pub fn in_background(mut self) -> Self {
        self.background = true;
        self
    }

    /// When the Python environment the workers run in was last installed.
    pub fn environment_installed_at(&self) -> Option<std::time::SystemTime> {
        python_env::installed_at(&self.cache_dir)
    }

    /// Set this environment up in the background
    /// ([`python_env::setup_python_env_background`]) and hold every worker
    /// until that is ready; see [`Self::ready`]. Returns the setup, for
    /// anything else that runs Python in this environment to wait on.
    pub fn start_setup(&mut self) -> BackgroundSetup {
        let setup = python_env::setup_python_env_background(
            self.cache_dir.clone(),
            self.resource_dir.clone(),
        );
        self.setup = Some(setup.clone());
        setup
    }

    /// The host's background setup of this environment, when it runs one.
    pub fn setup(&self) -> Option<&BackgroundSetup> {
        self.setup.as_ref()
    }

    /// Wait for this environment's setup, if it has one, to settle either way.
    pub async fn setup_settled(&self) {
        if let Some(setup) = &self.setup {
            let _ = setup.settled().await;
        }
    }

    /// Wait until workers can start: at once when there is no setup to wait
    /// for, otherwise once it is ready ([`BackgroundSetup::ready`]). Setup
    /// takes minutes on a first launch, and an update that changes the
    /// requirements reinstalls them, so an import can arrive first. An
    /// identity transition ends the wait.
    pub async fn ready(&self, analysis: &AnalysisGuard) -> Result<(), String> {
        let Some(setup) = &self.setup else {
            return Ok(());
        };
        tokio::select! {
            ready = setup.ready() => ready,
            () = analysis.cancelled() => analysis.checkpoint(),
        }
    }

    pub fn from_env_default() -> Result<Self, String> {
        Ok(Self::new(
            crate::storage::cache_dir()?,
            crate::resources::resource_dir(),
        ))
    }

    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    pub fn python(&self) -> Result<PathBuf, String> {
        python_env::find_existing_venv_python(&self.cache_dir).ok_or_else(|| {
            format!(
                "no managed python environment under {} — run Luma once to create it",
                self.cache_dir.display()
            )
        })
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

    /// The launch line every python-backed worker shares: managed interpreter,
    /// the deployed copy of `name` as argv[1], and a working directory of the
    /// deploy dir so sibling resources unpacked by [`Self::deploy_resource`]
    /// (`consonance-ACE/`, `n2n/`) resolve as plain imports.
    ///
    /// Callers append their own arguments, stdio and any worker-specific
    /// environment. Every path a worker receives on argv is absolute, so the
    /// shared working directory never changes how an argument resolves.
    pub fn worker_command(&self, name: &str, source: &str) -> Result<Command, String> {
        let python = self.python()?;
        let script = self.deploy_script(name, source)?;
        let workdir = script
            .parent()
            .ok_or_else(|| format!("worker script {} has no parent directory", script.display()))?
            .to_path_buf();

        let mut cmd = if self.background {
            crate::runtime::command::background_command(python)
        } else {
            crate::runtime::command::command(python)
        };
        cmd.env("PYTHONUNBUFFERED", "1")
            .current_dir(workdir)
            .arg(script);
        Ok(cmd)
    }
}

#[cfg(test)]
mod tests {
    use std::task::Poll;

    use futures_util::poll;
    use tokio::sync::watch;

    use super::*;
    use crate::preprocessing::AnalysisTaskGroup;
    use crate::runtime::python::PythonSetup;

    fn lease(group: &AnalysisTaskGroup) -> crate::preprocessing::task_group::AnalysisLease {
        group
            .lease(group.current_epoch().expect("epoch"))
            .expect("lease")
    }

    fn workers(cache: &Path, state: watch::Receiver<PythonSetup>) -> WorkerEnvironment {
        let mut workers = WorkerEnvironment::new(cache.to_path_buf(), None);
        workers.setup = Some(BackgroundSetup::new(cache.to_path_buf(), state));
        workers
    }

    /// A cache holding a venv's interpreter, as setup leaves it before it has
    /// installed any package.
    fn bare_venv() -> tempfile::TempDir {
        let cache = tempfile::tempdir().expect("temp dir");
        let (bin, exe) = if cfg!(windows) {
            ("Scripts", "python.exe")
        } else {
            ("bin", "python3")
        };
        let bin = python_env::venv_dir(cache.path()).join(bin);
        std::fs::create_dir_all(&bin).expect("venv bin dir");
        std::fs::write(bin.join(exe), "").expect("interpreter stand-in");
        cache
    }

    /// A cache holding an environment an earlier setup finished installing.
    fn installed_environment() -> tempfile::TempDir {
        let cache = bare_venv();
        let marker = python_env::venv_dir(cache.path()).join(python_env::REQUIREMENTS_MARKER);
        std::fs::write(marker, "an earlier build's hash").expect("marker");
        cache
    }

    #[tokio::test]
    async fn a_worker_waits_for_running_setup_and_starts_once_it_is_ready() {
        // An installed environment, as during an update's reinstall.
        let cache = installed_environment();
        let (setup, state) = watch::channel(PythonSetup::Running);
        let workers = workers(cache.path(), state);
        let group = AnalysisTaskGroup::new();
        let stage = lease(&group);

        let analysis = stage.guard();
        let ready = workers.ready(&analysis);
        tokio::pin!(ready);
        assert!(
            poll!(&mut ready).is_pending(),
            "a worker started while setup was still running"
        );
        setup.send_replace(PythonSetup::Ready);
        assert_eq!(poll!(&mut ready), Poll::Ready(Ok(())));
    }

    #[tokio::test]
    async fn an_identity_switch_completes_while_a_stage_waits_for_setup() {
        let cache = tempfile::tempdir().expect("temp dir");
        let (_setup, state) = watch::channel(PythonSetup::Running);
        let workers = workers(cache.path(), state);
        let group = AnalysisTaskGroup::new();
        // The waiting stage holds its lease, which the switch waits for.
        let stage = lease(&group);
        let analysis = stage.guard();
        let ready = workers.ready(&analysis);
        tokio::pin!(ready);
        assert!(poll!(&mut ready).is_pending());

        let switch = group.suspend_for_identity_switch();
        tokio::pin!(switch);
        assert!(
            poll!(&mut switch).is_pending(),
            "the switch should wait for the stage's lease"
        );
        assert!(
            matches!(poll!(&mut ready), Poll::Ready(Err(_))),
            "the stage kept waiting for setup through the switch"
        );
        drop(stage);
        assert!(
            matches!(poll!(&mut switch), Poll::Ready(Ok(_))),
            "the switch did not complete once the stage let go"
        );
    }

    #[tokio::test]
    async fn a_failed_first_setup_fails_the_worker_with_the_step_not_the_pip_log() {
        // Setup creates the venv before it installs anything, so a failed
        // first setup leaves an interpreter with no packages.
        let cache = bare_venv();
        let error = "Command to install PyTorch failed.\nstdout: Collecting torch==2.6.0";
        let (_setup, state) = watch::channel(PythonSetup::failed(error));
        let group = AnalysisTaskGroup::new();
        let stage = lease(&group);

        let failure = workers(cache.path(), state)
            .ready(&stage.guard())
            .await
            .expect_err("there is no installed environment to run on");
        assert!(
            failure.contains("Command to install PyTorch failed."),
            "{failure}"
        );
        assert!(!failure.contains("Collecting"), "{failure}");
    }

    #[tokio::test]
    async fn a_failed_setup_leaves_workers_the_environment_an_earlier_one_installed() {
        // An offline launch after an update: the reinstall fails, and the old
        // environment is still installed.
        let cache = installed_environment();
        let (_setup, state) = watch::channel(PythonSetup::failed("offline"));
        let group = AnalysisTaskGroup::new();
        let stage = lease(&group);

        assert_eq!(
            workers(cache.path(), state).ready(&stage.guard()).await,
            Ok(())
        );
    }
}
