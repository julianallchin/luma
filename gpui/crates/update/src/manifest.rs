//! The release manifest, `latest.json`, and the decision whether it is newer.
//!
//! The format is Tauri's static updater manifest, unchanged, so the file the
//! release workflow publishes is also the one the retired Tauri build polls
//! (see `RELEASING.md`):
//!
//! ```json
//! {
//!   "version": "0.6.0",
//!   "notes": "…",
//!   "pub_date": "2026-10-23T12:00:00Z",
//!   "commit": "0123abc…",
//!   "platforms": {
//!     "darwin-aarch64": { "url": "https://…/Luma-aarch64.app.tar.gz", "signature": "…" },
//!     "linux-x86_64":   { "url": "https://…/luma-linux-x86_64.tar.gz", "signature": "…" }
//!   }
//! }
//! ```
//!
//! `commit` is Luma's own addition; Tauri ignores it. A nightly has no version
//! worth comparing, so it is the commit that says whether it moved.

use std::collections::HashMap;

use serde::Deserialize;

use crate::Channel;

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub version: String,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub commit: Option<String>,
    pub platforms: HashMap<String, Asset>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Asset {
    pub url: String,
    /// Base64 of the whole minisign `.sig` file — see [`crate::verify`].
    pub signature: String,
}

/// What this build is, for comparing against a manifest.
#[derive(Debug, Clone)]
pub struct Current {
    pub channel: Channel,
    pub version: String,
    pub commit: Option<String>,
}

impl Manifest {
    pub fn parse(json: &str) -> Result<Self, String> {
        serde_json::from_str(json).map_err(|error| format!("unreadable release manifest: {error}"))
    }

    /// The archive for this machine, under Tauri's `<os>-<arch>` key.
    pub fn asset(&self, platform: &str) -> Option<&Asset> {
        self.platforms.get(platform)
    }

    /// Whether installing this release would move `current` forward.
    ///
    /// Stable compares versions, so a manifest that lags (a CDN serving the
    /// previous file) never downgrades anyone. Nightly compares commits: any
    /// other commit is the newer one, because the rolling release only ever
    /// moves forward. A dev build never updates.
    pub fn is_newer_than(&self, current: &Current) -> Result<bool, String> {
        match current.channel {
            Channel::Dev => Ok(false),
            Channel::Nightly => Ok(match (&self.commit, &current.commit) {
                (Some(published), Some(running)) => published != running,
                // A nightly manifest without a commit is malformed; a nightly
                // build without one cannot tell, and must not loop on updates.
                _ => false,
            }),
            Channel::Stable => {
                let published = parse_version(&self.version)?;
                let running = parse_version(&current.version)?;
                Ok(published > running)
            }
        }
    }
}

/// A semver, with the `v` a tag carries tolerated.
fn parse_version(text: &str) -> Result<semver::Version, String> {
    semver::Version::parse(text.trim_start_matches('v'))
        .map_err(|error| format!("`{text}` is not a version: {error}"))
}

/// This machine's manifest key, or `None` where Luma publishes no build.
pub fn platform() -> Option<&'static str> {
    platform_key(std::env::consts::OS, std::env::consts::ARCH)
}

fn platform_key(os: &str, arch: &str) -> Option<&'static str> {
    Some(match (os, arch) {
        ("macos", "aarch64") => "darwin-aarch64",
        ("macos", "x86_64") => "darwin-x86_64",
        ("linux", "x86_64") => "linux-x86_64",
        ("linux", "aarch64") => "linux-aarch64",
        ("windows", "x86_64") => "windows-x86_64",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = r#"{
        "version": "0.6.0",
        "notes": "Native app.",
        "pub_date": "2026-10-23T12:00:00Z",
        "commit": "bbbb",
        "platforms": {
            "darwin-aarch64": { "url": "https://example.com/Luma-aarch64.app.tar.gz", "signature": "c2ln" },
            "linux-x86_64": { "url": "https://example.com/luma-linux-x86_64.tar.gz", "signature": "c2ln" }
        }
    }"#;

    fn current(channel: Channel, version: &str, commit: Option<&str>) -> Current {
        Current {
            channel,
            version: version.into(),
            commit: commit.map(Into::into),
        }
    }

    #[test]
    fn reads_a_tauri_manifest() {
        let manifest = Manifest::parse(MANIFEST).unwrap();
        assert_eq!(manifest.version, "0.6.0");
        assert_eq!(
            manifest.asset("linux-x86_64").unwrap().url,
            "https://example.com/luma-linux-x86_64.tar.gz"
        );
        assert!(manifest.asset("windows-x86_64").is_none());
    }

    #[test]
    fn a_manifest_without_platforms_is_refused() {
        assert!(Manifest::parse(r#"{"version":"0.6.0"}"#).is_err());
    }

    #[test]
    fn stable_moves_only_forward_by_version() {
        let manifest = Manifest::parse(MANIFEST).unwrap();
        let newer = |version| {
            manifest
                .is_newer_than(&current(Channel::Stable, version, None))
                .unwrap()
        };
        assert!(newer("0.5.19"));
        assert!(newer("0.6.0-rc.1"));
        assert!(!newer("0.6.0"));
        assert!(!newer("0.6.1"));
        assert!(!newer("1.0.0"));
    }

    #[test]
    fn stable_tolerates_a_tag_spelling() {
        let mut manifest = Manifest::parse(MANIFEST).unwrap();
        manifest.version = "v0.6.0".into();
        assert!(manifest
            .is_newer_than(&current(Channel::Stable, "0.5.19", None))
            .unwrap());
    }

    #[test]
    fn stable_refuses_a_version_it_cannot_read() {
        let mut manifest = Manifest::parse(MANIFEST).unwrap();
        manifest.version = "latest".into();
        assert!(manifest
            .is_newer_than(&current(Channel::Stable, "0.5.19", None))
            .is_err());
    }

    #[test]
    fn nightly_moves_when_the_commit_does() {
        let manifest = Manifest::parse(MANIFEST).unwrap();
        let newer = |commit| {
            manifest
                .is_newer_than(&current(Channel::Nightly, "0.9.0", commit))
                .unwrap()
        };
        assert!(newer(Some("aaaa")));
        assert!(!newer(Some("bbbb")));
        assert!(!newer(None));
    }

    #[test]
    fn dev_never_updates() {
        let manifest = Manifest::parse(MANIFEST).unwrap();
        assert!(!manifest
            .is_newer_than(&current(Channel::Dev, "0.0.1", Some("aaaa")))
            .unwrap());
    }

    #[test]
    fn platform_keys_match_tauri() {
        assert_eq!(platform_key("macos", "aarch64"), Some("darwin-aarch64"));
        assert_eq!(platform_key("linux", "x86_64"), Some("linux-x86_64"));
        assert_eq!(platform_key("freebsd", "x86_64"), None);
    }
}
