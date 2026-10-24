//! Luma's self-updater: check, download, verify, swap, and say so.
//!
//! Modelled on Zed's `auto_update` crate. A build knows its channel from the
//! environment it was compiled in; the updater polls that channel's manifest
//! on a GitHub release, downloads the archive for this machine, checks its
//! minisign signature, swaps the installed tree for it, and reports
//! [`Status::Updated`] with the path to relaunch. The app shows "Restart to
//! update" and calls gpui's `restart` with that path.
//!
//! Everything blocking runs on a thread of its own; the app watches
//! [`Updater::subscribe`]. A failed *automatic* check is logged and goes back
//! to idle, as in Zed — only a check someone asked for reports an error.

pub mod install;
pub mod manifest;
pub mod verify;

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use install::Install;
use manifest::{Current, Manifest};

/// How often a running app looks for a release.
pub const POLL_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// Which stream of releases a build follows, fixed when it is compiled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    /// Built from source: never updates.
    Dev,
    /// The rolling `nightly` prerelease, built from `dev`.
    Nightly,
    /// Tagged `v*` releases.
    Stable,
}

impl Channel {
    /// The channel the release workflow compiled in through
    /// `LUMA_RELEASE_CHANNEL`. Anything else is a source build.
    pub const CURRENT: Self = match option_env!("LUMA_RELEASE_CHANNEL") {
        Some(channel) if const_eq(channel, "stable") => Self::Stable,
        Some(channel) if const_eq(channel, "nightly") => Self::Nightly,
        _ => Self::Dev,
    };

    pub fn label(self) -> &'static str {
        match self {
            Self::Dev => "dev",
            Self::Nightly => "nightly",
            Self::Stable => "stable",
        }
    }

    /// Where this channel's `latest.json` is published.
    fn manifest_url(self) -> Option<&'static str> {
        match self {
            Self::Dev => None,
            Self::Nightly => {
                Some("https://github.com/julianallchin/luma/releases/download/nightly/latest.json")
            }
            // `latest` skips prereleases, so the nightly never reaches here.
            Self::Stable => {
                Some("https://github.com/julianallchin/luma/releases/latest/download/latest.json")
            }
        }
    }
}

const fn const_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// The commit this build was made from, when the release workflow said.
pub const COMMIT: Option<&str> = option_env!("LUMA_COMMIT_SHA");

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Idle,
    Checking,
    Downloading {
        version: String,
    },
    Installing {
        version: String,
    },
    /// Installed on disk; relaunching `restart` runs it.
    Updated {
        version: String,
        restart: PathBuf,
    },
    /// A check someone asked for found nothing newer.
    UpToDate,
    /// A check someone asked for failed.
    Errored(String),
}

impl Status {
    pub fn busy(&self) -> bool {
        matches!(
            self,
            Self::Checking | Self::Downloading { .. } | Self::Installing { .. }
        )
    }
}

/// Where releases come from, who must have signed them, and where they go.
#[derive(Clone)]
struct Target {
    manifest_url: String,
    public_key: String,
    install: Install,
}

pub struct Updater {
    current: Current,
    /// Where updates come from and go, or why there is nowhere.
    target: Result<Target, String>,
    status: tokio::sync::watch::Sender<Status>,
    /// One check at a time. The status says the same thing, but a flag set
    /// with a compare-exchange is what stops two threads starting at once.
    running: AtomicBool,
}

impl Updater {
    /// The updater for this process, running `version`.
    pub fn new(version: &str) -> Arc<Self> {
        let current = Current {
            channel: Channel::CURRENT,
            version: version.to_string(),
            commit: COMMIT.map(str::to_string),
        };
        let target = Self::target(current.channel);
        if let Err(reason) = &target {
            log::info!("automatic updates are off: {reason}");
        }
        Self::with(current, target)
    }

    fn with(current: Current, target: Result<Target, String>) -> Arc<Self> {
        Arc::new(Self {
            current,
            target,
            status: tokio::sync::watch::channel(Status::Idle).0,
            running: AtomicBool::new(false),
        })
    }

    fn target(channel: Channel) -> Result<Target, String> {
        // For a packager whose own tool owns updates — Zed's
        // `ZED_UPDATE_EXPLANATION`. Set when compiling, it is shown verbatim.
        if let Some(explanation) = option_env!("LUMA_UPDATE_EXPLANATION") {
            return Err(explanation.to_string());
        }
        let manifest_url = std::env::var("LUMA_UPDATE_MANIFEST")
            .ok()
            .or_else(|| channel.manifest_url().map(str::to_string))
            .ok_or_else(|| {
                "This is a development build, which does not update itself.".to_string()
            })?;
        let exe = std::env::current_exe()
            .map_err(|error| format!("Could not find the running executable: {error}"))?;
        Ok(Target {
            manifest_url,
            public_key: verify::PUBLIC_KEY.to_string(),
            install: Install::locate(&exe)?,
        })
    }

    /// Why this build cannot update itself, or `None` if it can.
    pub fn unavailable(&self) -> Option<&str> {
        self.target.as_ref().err().map(String::as_str)
    }

    pub fn channel(&self) -> Channel {
        self.current.channel
    }

    pub fn status(&self) -> Status {
        self.status.borrow().clone()
    }

    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<Status> {
        self.status.subscribe()
    }

    /// Look for a release, and install it if there is one, on a thread of
    /// its own. A no-op while a check runs or once an update waits for a
    /// restart: there is nothing newer to do until it is applied.
    pub fn check(self: &Arc<Self>, manual: bool) {
        let Ok(target) = &self.target else {
            return;
        };
        if matches!(*self.status.borrow(), Status::Updated { .. }) {
            return;
        }
        if self
            .running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let (this, target) = (self.clone(), target.clone());
        let spawned = std::thread::Builder::new()
            .name("luma-update".into())
            .spawn(move || {
                let outcome = this.run(&target);
                let status = match outcome {
                    Ok(status) => status,
                    Err(error) => {
                        log::warn!("update check failed: {error}");
                        if manual {
                            Status::Errored(error)
                        } else {
                            Status::Idle
                        }
                    }
                };
                let status = match status {
                    Status::UpToDate if !manual => Status::Idle,
                    status => status,
                };
                this.status.send_replace(status);
                this.running.store(false, Ordering::Release);
            });
        if let Err(error) = spawned {
            log::warn!("could not start the update check: {error}");
            self.running.store(false, Ordering::Release);
        }
    }

    fn run(&self, target: &Target) -> Result<Status, String> {
        self.status.send_replace(Status::Checking);
        let client = reqwest::blocking::Client::builder()
            .user_agent(concat!("luma-update/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(30))
            .build()
            .map_err(|error| format!("could not start an HTTP client: {error}"))?;
        let manifest = client
            .get(&target.manifest_url)
            .timeout(Duration::from_secs(30))
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .and_then(reqwest::blocking::Response::text)
            .map_err(|error| format!("could not read the release manifest: {error}"))?;
        let manifest = Manifest::parse(&manifest)?;
        if !manifest.is_newer_than(&self.current)? {
            return Ok(Status::UpToDate);
        }
        let platform = manifest::platform().ok_or("Luma publishes no release for this machine.")?;
        let asset = manifest
            .asset(platform)
            .ok_or_else(|| format!("Release {} has no build for {platform}.", manifest.version))?;
        let version = manifest.version.clone();

        self.status.send_replace(Status::Downloading {
            version: version.clone(),
        });
        let mut archive = tempfile::Builder::new()
            .prefix("luma-update-")
            .suffix(".tar.gz")
            .tempfile()
            .map_err(|error| format!("could not create a download file: {error}"))?;
        // No overall timeout: a release is hundreds of megabytes, and a slow
        // link is not a failure. The connect timeout still bounds a dead one.
        let mut response = client
            .get(&asset.url)
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .map_err(|error| format!("could not download {version}: {error}"))?;
        response
            .copy_to(archive.as_file_mut())
            .map_err(|error| format!("could not download {version}: {error}"))?;
        archive
            .as_file_mut()
            .flush()
            .map_err(|error| format!("could not save {version}: {error}"))?;
        verify::verify_file(archive.path(), &asset.signature, &target.public_key)?;

        self.status.send_replace(Status::Installing {
            version: version.clone(),
        });
        let restart = target.install.replace(archive.path())?;
        log::info!("installed Luma {version}; it runs at the next launch");
        Ok(Status::Updated { version, restart })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A source build — which is what `cargo test` makes — must never reach
    /// the network on its own.
    #[test]
    fn a_source_build_is_not_updatable() {
        assert_eq!(Channel::CURRENT, Channel::Dev);
        let updater = Updater::new("0.0.0");
        assert!(updater.unavailable().is_some());
        updater.check(true);
        assert_eq!(updater.status(), Status::Idle);
    }

    #[test]
    fn channel_names_compare_exactly() {
        assert!(const_eq("stable", "stable"));
        assert!(!const_eq("stable", "stabl"));
        assert!(!const_eq("nightly", "stable"));
    }

    /// Answer each GET with the body routed to its path, until the test ends.
    fn serve(routes: Vec<(&'static str, Vec<u8>)>) -> String {
        use std::io::{BufRead as _, BufReader};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                reader.read_line(&mut request).unwrap();
                // Drain the headers; the body of a GET is empty.
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap() > 2 {
                    line.clear();
                }
                let path = request.split_whitespace().nth(1).unwrap_or("");
                let (status, body) = match routes.iter().find(|(route, _)| *route == path) {
                    Some((_, body)) => ("200 OK", body.as_slice()),
                    None => ("404 Not Found", &b""[..]),
                };
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(body);
            }
        });
        base
    }

    /// A check against a local release: the manifest, the archive and its
    /// signature as the release workflow publishes them.
    fn release(
        signature_of: impl FnOnce(&[u8]) -> (String, String),
    ) -> (tempfile::TempDir, Arc<Updater>, Install) {
        let dir = tempfile::tempdir().unwrap();
        let install = install::tests::installed(dir.path(), "old");
        let archive =
            std::fs::read(install::tests::archive(dir.path(), "luma.app", "new")).unwrap();
        let (key, signature) = signature_of(&archive);
        let manifest = serde_json::json!({
            "version": "1.0.0",
            "platforms": {
                manifest::platform().unwrap(): { "url": "PLACEHOLDER", "signature": signature }
            }
        });
        // The archive's URL is only known once its server is up.
        let base = serve(vec![("/luma.tar.gz", archive)]);
        let manifest = manifest
            .to_string()
            .replace("PLACEHOLDER", &format!("{base}/luma.tar.gz"));
        let manifest_url = format!(
            "{}/latest.json",
            serve(vec![("/latest.json", manifest.into_bytes())])
        );
        let updater = Updater::with(
            Current {
                channel: Channel::Stable,
                version: "0.9.0".into(),
                commit: None,
            },
            Ok(Target {
                manifest_url,
                public_key: key,
                install: install.clone(),
            }),
        );
        (dir, updater, install)
    }

    /// Wait for a check to settle. Bounded by a deadline rather than a sleep
    /// of a guessed length.
    fn settled(updater: &Updater) -> Status {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            let status = updater.status();
            if !status.busy() && status != Status::Idle && !updater.running.load(Ordering::Acquire)
            {
                return status;
            }
            assert!(std::time::Instant::now() < deadline, "stuck at {status:?}");
            std::thread::yield_now();
        }
    }

    #[test]
    fn a_signed_release_installs_and_asks_for_a_restart() {
        let (_dir, updater, install) = release(verify::tests::sign);
        updater.check(true);
        let Status::Updated { version, restart } = settled(&updater) else {
            panic!("expected an installed update, got {:?}", updater.status());
        };
        assert_eq!(version, "1.0.0");
        assert_eq!(restart, install.restart_path());
        assert_eq!(std::fs::read_to_string(restart).unwrap(), "new");
    }

    #[test]
    fn a_release_signed_by_another_key_is_refused() {
        let (_dir, updater, install) = release(|archive| {
            let (key, _) = verify::tests::sign(archive);
            let (_, forged) = verify::tests::sign(archive);
            (key, forged)
        });
        updater.check(true);
        assert!(matches!(settled(&updater), Status::Errored(_)));
        assert_eq!(
            std::fs::read_to_string(install.restart_path()).unwrap(),
            "old"
        );
    }

    #[test]
    fn an_automatic_failure_is_quiet() {
        let (_dir, updater, _install) = release(|archive| {
            let (key, _) = verify::tests::sign(archive);
            (key, "Zm9yZ2Vk".into())
        });
        updater.check(false);
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while updater.running.load(Ordering::Acquire) || updater.status().busy() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert_eq!(updater.status(), Status::Idle);
    }
}
