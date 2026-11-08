# Scripts

Run from the repository root.

| Path | What it is for |
|---|---|
| `setup` | Prepare a fresh clone: toolchain, submodule, LFS files, hooks, and a check of the rest. |
| `headless/` | Drive Luma without the app: one agent turn, the MCP server, the venue-and-show gauntlet. See [headless/README.md](headless/README.md). |
| `library/snapshot.py` | Copy a Luma library (the database and its files) to a backup folder and verify the copy: `python3 scripts/library/snapshot.py SOURCE DESTINATION`. |
| `screenshots/` | The website's and README's screenshots, from a staged showcase library or a copy of a real one. See [screenshots/README.md](screenshots/README.md). |
| `release/` | Build the installable app. `bundle-mac` and `bundle-linux` build and package it, `stage-resources` copies the runtimes, workers and fixtures it ships, `manifest.py` writes the update manifest, and `install.sh` is the Linux installer. `macos/` and `linux/` hold the bundle's metadata and the DMG art. The release workflow runs them; see [RELEASING.md](../RELEASING.md). |

A script that only one task needed does not belong here; see "Where things go"
in [AGENTS.md](../AGENTS.md).
