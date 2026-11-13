# Luma

Lights that know the song. Luma listens to your music and builds the light
show with you, with no lighting experience needed.

<img src="www/public/screenshots/hero.webp" alt="Luma in Arrange: the agent's notes on the score it wrote at the left, the stage lit in red under two white beams, and the score's clips in lanes under the song's waveform." />

A console records DMX for one room's fixtures. Luma records patterns aimed at
named groups (`front_wash`, `back_movers`), and each venue says which of its
fixtures those groups hold. Build the rig, arrange a score on a song's
timeline, and play it live along with a DJ.

Or ask. An agent sits in every window: it listens to the song, writes the
score, and tells you what it did.

- **A 3D visualizer** with haze and beams, to see a show before the rig exists
- **Nearly 900 fixtures** modelled in 3D, driven over Art-Net or USB DMX
- **Lasers**: real projectors through an Ether Dream DAC
- **Follows the DJ** over Pro DJ Link, StageLinQ or Rekordbox
- **Free**, with the source here

## Download

- **Apple silicon Mac**: [Luma-aarch64.dmg](https://github.com/lumashow/luma/releases/latest/download/Luma-aarch64.dmg)
- **Linux x86_64**: `curl -fsSL https://github.com/lumashow/luma/releases/latest/download/install.sh | sh`

An installed app updates itself. Every build is on
[GitHub Releases](https://github.com/lumashow/luma/releases).

## Build from source

You need [rustup](https://rust-lang.org/tools/install/),
[Git LFS](https://git-lfs.com), Python 3, and Xcode on macOS.

```sh
git clone git@github.com:lumashow/luma.git
cd luma
scripts/setup          # toolchain, submodule, LFS files, git hooks
python3 gpui/dev.py    # build, launch, and rebuild on save
```

[gpui/BUILD.md](gpui/BUILD.md) has the rest: Linux libraries, build
profiles, and the test harness.

## Docs

- [luma.show/docs](https://luma.show/docs): using the app, in five short
  pages, and the [node reference](https://luma.show/docs/node-reference)
- [AGENTS.md](AGENTS.md): the conventions for working in this repository
- [docs/README.md](docs/README.md): the architecture, and how each system works
- [RELEASING.md](RELEASING.md): releases and the nightly build

## Tests

Run from the repository root:

| What | Command |
|---|---|
| Native app, compile everything | `cargo check --manifest-path gpui/Cargo.toml --workspace --all-targets` |
| Backend (library, bins, crates) | `cargo test --manifest-path backend/Cargo.toml --workspace` |
| UI script tests (`*.test.js`) | `gpui/test` (see [gpui/BUILD.md](gpui/BUILD.md#script-tests-gpuitest)) |
| One native crate | `cargo test --manifest-path gpui/Cargo.toml -p luma-render` |
| Agent Python executor | `python3 backend/python/luma_exec/tests/run_tests.py` |
| Analysis workers (Python) | `python3 -m unittest discover -s backend/python/tests` |
| Release scripts (Python) | `python3 -m unittest discover -s scripts/release` |
| Sync against a live stack (Docker) | `python3 deploy/test-stack/run.py --test sync::two_device_tests` |

## License

[PolyForm Shield 1.0.0](LICENSE.md).
