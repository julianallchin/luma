# Releasing Luma

`.github/workflows/release.yml` builds, signs and publishes the native app.
Installs update themselves from what it publishes (`gpui/crates/update`).

| Platform | Download | Update archive |
|---|---|---|
| macOS, Apple silicon | `Luma-aarch64.dmg` (signed, notarized) | `Luma-aarch64.app.tar.gz` |
| Linux x86_64 | `luma-linux-x86_64.tar.gz` via `install.sh` | the same tarball |

There is no Intel Mac build: PyTorch publishes no macOS x86_64 wheels past
2.2, so the managed Python environment cannot be created there. There is no
Windows build yet.

## Cutting a stable release

1. Set the version in `backend/Cargo.toml`. That is `luma_lib::VERSION`, which
   the About screen shows and the updater compares against.
2. Commit, then push a matching tag: `git tag v0.6.0 && git push origin v0.6.0`.
   The workflow fails if the tag and the crate version differ.
3. The workflow creates a **draft** release with the DMG, both archives, their
   `.sig` files, `latest.json` and `install.sh`. Download the DMG and the
   tarball and check that they launch.
4. Publish the draft. Publishing moves
   `releases/latest/download/latest.json`, which every stable install polls
   every hour. Until then no install sees the release.

## Nightly

Run the workflow by hand (Actions → Release → Run workflow) on any branch. It
replaces the `nightly` prerelease with a build of that commit. Nightly builds
poll `releases/download/nightly/latest.json` and update whenever its commit
differs from their own. Install one on Linux with
`LUMA_CHANNEL=nightly sh install.sh`.

## Secrets and variables

Set these under Settings → Secrets and variables → Actions.

| Name | Kind | Used for |
|---|---|---|
| `APPLE_CERTIFICATE` | secret | base64 of the Developer ID Application `.p12` |
| `APPLE_CERTIFICATE_PASSWORD` | secret | the `.p12` password |
| `APPLE_ID` | secret | notarytool Apple ID |
| `APPLE_PASSWORD` | secret | an app-specific password for that Apple ID |
| `APPLE_TEAM_ID` | secret | the team ID the certificate belongs to |
| `APPLE_SIGNING_IDENTITY` | variable, optional | codesign identity; defaults to the one in `release.yml` |
| `TAURI_SIGNING_PRIVATE_KEY` | secret | minisign key that signs update archives (the name is the one the signer, `@tauri-apps/cli signer`, reads) |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | secret | its password (may be empty) |

Without the Apple secrets the DMG is ad-hoc signed and not notarized:
Gatekeeper blocks it on other Macs. Without the update key the publish job
fails, because installs refuse unsigned archives.

### The update key

Installs verify each archive against the public key compiled into
`gpui/crates/update/src/verify.rs` (minisign key ID `D0F3AAE2A309160A`);
`TAURI_SIGNING_PRIVATE_KEY` is its private half. To rotate it:

```sh
npx @tauri-apps/cli@2.12.4 signer generate -w luma-update.key
```

Store the private key and password as the two secrets. Put the base64 public
key (`luma-update.key.pub`) in `verify.rs`, or build with
`LUMA_UPDATE_PUBKEY` set. Installs built with the old key reject archives
signed with the new one. Ship one release signed with the old key that
carries the new public key before switching the secret.

## Tauri installs

The retired Tauri build (0.5.19 and older) polls the same stable `latest.json`
but looks for `darwin-aarch64`, and the manifest keys platforms by target
triple, so those installs keep their version. The native app cannot open a
library that old (`backend/migrations/20261024000000_baseline.sql`).

## Build switches

| Variable | When | Effect |
|---|---|---|
| `LUMA_RELEASE_CHANNEL` | compile time | `stable` or `nightly`; anything else is a dev build that never updates |
| `LUMA_COMMIT_SHA` | compile time | the commit a nightly compares |
| `LUMA_UPDATE_PUBKEY` | compile time | a different update public key |
| `LUMA_UPDATE_EXPLANATION` | compile time | turns updates off and shows this text, for distribution packages |
| `LUMA_UPDATE_MANIFEST` | run time | poll another manifest URL, for testing an update end to end |
| `LUMA_RESOURCE_DIR` | run time | use another resource directory (`backend/src/resources.rs`) |

## Building locally

`scripts/release/bundle-linux` and `scripts/release/bundle-mac` produce the
same files in `dist/` without any secrets. They need Git LFS files and
submodules, as in `gpui/BUILD.md`.
