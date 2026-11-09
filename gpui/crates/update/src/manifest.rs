//! The release manifest, `latest.json`, and the decision whether it is newer.
//!
//! ```json
//! {
//!   "version": "0.6.0",
//!   "notes": "- New Low quality for laptops\n- Clearer sync status",
//!   "changes": [
//!     { "version": "0.6.0", "commit": "0123abc…", "lines": ["New Low quality for laptops", "Clearer sync status"] },
//!     { "version": "0.5.19", "commit": "4567def…", "lines": ["Right-click a score to rename it"] }
//!   ],
//!   "pub_date": "2026-10-23T12:00:00Z",
//!   "commit": "0123abc…",
//!   "platforms": {
//!     "aarch64-apple-darwin":     { "url": "https://…/Luma-aarch64.app.tar.gz", "signature": "…" },
//!     "x86_64-unknown-linux-gnu": { "url": "https://…/luma-linux-x86_64.tar.gz", "signature": "…" }
//!   }
//! }
//! ```
//!
//! A nightly has no version worth comparing, so it is the commit that says
//! whether it moved. Platforms are keyed by target triple, which the retired
//! Tauri build (0.5.19 and older), polling the same URL for `darwin-aarch64`,
//! never finds: the native app cannot open its library.
//!
//! `changes` holds the changelog of the channel's last releases, newest
//! first, so an install that skipped some can say everything it gains
//! ([`Manifest::changes_since`]). `notes` is the newest release's lines as
//! one string, for installs from before `changes`; this app ignores it.
//! `scripts/release/manifest.py` writes both.

use std::collections::HashMap;

use serde::Deserialize;

use crate::Channel;

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub version: String,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub changes: Vec<Change>,
    #[serde(default)]
    pub commit: Option<String>,
    pub platforms: HashMap<String, Asset>,
}

/// One release's changelog lines, as the release workflow collected them.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Change {
    pub version: String,
    #[serde(default)]
    pub commit: Option<String>,
    #[serde(default)]
    pub lines: Vec<String>,
}

impl Change {
    /// The version as a person reads it: a nightly's build metadata (the
    /// `+abc1234` commit) dropped, so `0.6.3-nightly.20261108` remains.
    pub fn label(&self) -> &str {
        let version = self.version.trim_start_matches('v');
        version.split_once('+').map_or(version, |(base, _)| base)
    }
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

    /// The archive for this machine, under its target triple.
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

impl Manifest {
    /// Every release newer than `current` that has something to say, newest
    /// first. A line appears once, under the newest release that has it.
    ///
    /// Stable keeps the releases with a higher version. Nightly keeps the
    /// releases above the one `current` was built from; a build older than
    /// every entry gets them all. A dev build gains nothing.
    pub fn changes_since(&self, current: &Current) -> Vec<Change> {
        let newer: Vec<&Change> = match current.channel {
            Channel::Dev => Vec::new(),
            Channel::Nightly => self
                .changes
                .iter()
                .take_while(|change| current.commit.is_none() || change.commit != current.commit)
                .collect(),
            Channel::Stable => {
                let Ok(running) = parse_version(&current.version) else {
                    return Vec::new();
                };
                self.changes
                    .iter()
                    .filter(|change| {
                        parse_version(&change.version).is_ok_and(|version| version > running)
                    })
                    .collect()
            }
        };
        let mut seen: Vec<&str> = Vec::new();
        let mut releases = Vec::new();
        for change in newer {
            let mut lines = Vec::new();
            for line in &change.lines {
                let line = line.trim();
                if !line.is_empty() && !seen.contains(&line) {
                    seen.push(line);
                    lines.push(line.to_string());
                }
            }
            if !lines.is_empty() {
                releases.push(Change {
                    version: change.version.clone(),
                    commit: change.commit.clone(),
                    lines,
                });
            }
        }
        releases
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
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
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
            "aarch64-apple-darwin": { "url": "https://example.com/Luma-aarch64.app.tar.gz", "signature": "c2ln" },
            "x86_64-unknown-linux-gnu": { "url": "https://example.com/luma-linux-x86_64.tar.gz", "signature": "c2ln" }
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
    fn reads_a_manifest() {
        let manifest = Manifest::parse(MANIFEST).unwrap();
        assert_eq!(manifest.version, "0.6.0");
        assert_eq!(
            manifest.asset("x86_64-unknown-linux-gnu").unwrap().url,
            "https://example.com/luma-linux-x86_64.tar.gz"
        );
        assert!(manifest.asset("x86_64-pc-windows-msvc").is_none());
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

    /// What every release before `changes` published: `notes` a URL.
    #[test]
    fn a_manifest_from_before_changes_still_reads() {
        let manifest = Manifest::parse(
            r#"{
                "version": "0.6.2",
                "notes": "https://github.com/julianallchin/luma/releases/tag/v0.6.2",
                "pub_date": "2026-11-01T12:00:00Z",
                "commit": "bbbb",
                "platforms": { "aarch64-apple-darwin": { "url": "https://example.com/a", "signature": "c2ln" } }
            }"#,
        )
        .unwrap();
        assert!(manifest.changes.is_empty());
        assert!(manifest
            .changes_since(&current(Channel::Stable, "0.6.0", None))
            .is_empty());
    }

    /// Three releases, newest first; the last two share a line.
    fn with_changes() -> Manifest {
        Manifest::parse(
            r#"{
                "version": "0.8.0",
                "notes": "- Clearer sync status",
                "commit": "c8",
                "changes": [
                    { "version": "0.8.0", "commit": "c8", "lines": ["Clearer sync status"] },
                    { "version": "0.7.0", "commit": "c7", "lines": ["Right-click a score to rename it", "New Low quality for laptops"] },
                    { "version": "0.6.0", "commit": "c6", "lines": ["New Low quality for laptops"] }
                ],
                "platforms": {}
            }"#,
        )
        .unwrap()
    }

    /// Each release kept, as its label and its lines.
    fn grouped(releases: Vec<Change>) -> Vec<(String, Vec<String>)> {
        releases
            .into_iter()
            .map(|change| (change.label().to_string(), change.lines))
            .collect()
    }

    fn group(label: &str, lines: &[&str]) -> (String, Vec<String>) {
        (
            label.into(),
            lines.iter().map(|line| line.to_string()).collect(),
        )
    }

    #[test]
    fn stable_shows_every_release_it_skipped() {
        let manifest = with_changes();
        let since =
            |version| grouped(manifest.changes_since(&current(Channel::Stable, version, None)));
        // 0.6.0's one line is 0.7.0's too, so it shows once, under 0.7.0,
        // and 0.6.0 has nothing left to say.
        assert_eq!(
            since("0.5.0"),
            [
                group("0.8.0", &["Clearer sync status"]),
                group(
                    "0.7.0",
                    &[
                        "Right-click a score to rename it",
                        "New Low quality for laptops"
                    ]
                ),
            ]
        );
        assert_eq!(
            since("0.7.0"),
            [group("0.8.0", &["Clearer sync status"])],
            "nothing it already runs"
        );
        assert!(since("0.8.0").is_empty());
    }

    #[test]
    fn nightly_shows_the_releases_above_its_commit() {
        let manifest = with_changes();
        let since =
            |commit| grouped(manifest.changes_since(&current(Channel::Nightly, "0.0.0", commit)));
        assert_eq!(
            since(Some("c7")),
            [group("0.8.0", &["Clearer sync status"])]
        );
        assert!(since(Some("c8")).is_empty());
        // Older than every entry the manifest keeps: all of them.
        assert_eq!(since(Some("c1")).len(), 2);
    }

    #[test]
    fn a_nightly_label_drops_the_commit() {
        let change = |version: &str| Change {
            version: version.into(),
            commit: None,
            lines: Vec::new(),
        };
        assert_eq!(
            change("0.6.3-nightly.20261108+abc1234").label(),
            "0.6.3-nightly.20261108"
        );
        assert_eq!(change("v0.6.3").label(), "0.6.3");
    }

    #[test]
    fn dev_gains_no_changes() {
        assert!(with_changes()
            .changes_since(&current(Channel::Dev, "0.0.1", None))
            .is_empty());
    }

    #[test]
    fn platform_keys_are_target_triples() {
        assert_eq!(
            platform_key("macos", "aarch64"),
            Some("aarch64-apple-darwin")
        );
        assert_eq!(
            platform_key("linux", "x86_64"),
            Some("x86_64-unknown-linux-gnu")
        );
        assert_eq!(platform_key("freebsd", "x86_64"), None);
    }
}
