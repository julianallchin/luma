//! Bootstrapping a windowless Luma host.
//!
//! Three binaries serve Luma's backend without a renderer — the JSON-RPC
//! `luma-cli rpc`, the MCP `luma-cli mcp` server, and the `luma-cli record` encoder —
//! and none has an `AppHandle`, a WebView, or an NSApplication. What they share
//! is everything up to the first request: the same flags and the same
//! [`crate::boot::open`] the app opens its library with. Only the wire
//! protocol differs, so only the wire protocol lives in the bins.
//!
//! Deliberately absent: DMX output, audio devices, replication and the loops.
//! Events go to stderr, because these hosts put their protocol on stdout.

use std::path::PathBuf;

use crate::boot::{Boot, Identity};
use crate::dispatch::{AppServices, Event, EventSink, Events};
use crate::storage::StorageRoot;

/// Emitted events land on stderr, alongside the host's other diagnostics.
struct StderrEvents;

impl EventSink for StderrEvents {
    fn emit(&self, event: Event) {
        match serde_json::to_string(&event) {
            Ok(json) => eprintln!("[event] {json}"),
            Err(error) => eprintln!("[event] {event:?} (not serializable: {error})"),
        }
    }
}

/// Where a headless host reads its library, fixtures and managed venv from.
///
/// Every field is an override: `None` means "resolve the way the app would".
#[derive(Default, Debug, Clone)]
pub struct HostConfig {
    config_dir: Option<PathBuf>,
    fixtures_root: Option<PathBuf>,
    cache_dir: Option<PathBuf>,
    fixture_principal: Option<String>,
}

impl HostConfig {
    /// Parse the shared headless flags: `--config-dir`, `--fixtures-root`,
    /// `--cache-dir`, `--fixture-principal`.
    ///
    /// `--fixture-principal` is a host-only trusted-identity seam. It arms the
    /// app-database write gate without a verified Supabase session, so it is
    /// only legible against a disposable config dir the caller nominated — hence
    /// the `--config-dir` requirement.
    ///
    /// # Errors
    ///
    /// On an unknown flag, a flag missing its value, or a principal given
    /// without an explicit config dir.
    pub fn parse_args(args: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut config = Self::default();
        let mut args = args;
        while let Some(flag) = args.next() {
            let mut value = |flag: &str| {
                args.next()
                    .ok_or_else(|| format!("{flag} requires a value"))
            };
            match flag.as_str() {
                "--config-dir" => config.config_dir = Some(PathBuf::from(value("--config-dir")?)),
                "--fixtures-root" => {
                    config.fixtures_root = Some(PathBuf::from(value("--fixtures-root")?));
                }
                "--cache-dir" => config.cache_dir = Some(PathBuf::from(value("--cache-dir")?)),
                "--fixture-principal" => {
                    let principal = value("--fixture-principal")?;
                    if principal.trim().is_empty() || principal.chars().any(char::is_control) {
                        return Err("--fixture-principal requires a non-empty printable id".into());
                    }
                    config.fixture_principal = Some(principal);
                }
                other => return Err(format!("unknown flag `{other}`")),
            }
        }
        if config.fixture_principal.is_some() && config.config_dir.is_none() {
            return Err("--fixture-principal requires an explicit --config-dir".into());
        }
        Ok(config)
    }

    /// The library directory this host will open.
    fn storage(&self) -> Result<StorageRoot, String> {
        match &self.config_dir {
            Some(path) => Ok(StorageRoot::from_path(path.clone())),
            None => StorageRoot::from_env_default(),
        }
    }

    fn cache_dir(&self) -> Result<PathBuf, String> {
        match &self.cache_dir {
            Some(path) => Ok(path.clone()),
            None => crate::storage::cache_dir(),
        }
    }

    fn fixtures_root(&self) -> Result<PathBuf, String> {
        match &self.fixtures_root {
            Some(path) => Ok(path.clone()),
            None => crate::resources::fixtures_root(),
        }
    }
}

/// Open the library this host was pointed at, through [`crate::boot::open`].
///
/// Migrations run against whatever config dir this is given, exactly as the app
/// does — pointing a host at an empty directory produces a fresh, fully
/// migrated `luma.db` + `state.db`. No replication, no devices: the persisted
/// audio setting never opens the audio device here.
///
/// # Errors
///
/// If a path cannot be resolved, or the boot fails.
pub async fn boot(config: &HostConfig) -> Result<AppServices, String> {
    let booted = crate::boot::open(Boot {
        storage: config.storage()?,
        fixtures_root: config.fixtures_root()?,
        cache_dir: Some(config.cache_dir()?),
        events: Events::new(StderrEvents),
        identity: match &config.fixture_principal {
            Some(principal) => Identity::Fixture(principal.clone()),
            None => Identity::Stored,
        },
        sync: false,
        output: false,
        audio: false,
    })
    .await?;
    Ok(booted.services)
}
