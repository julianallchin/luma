//! What the cargo suites share: the seeded library (`gpui_agent::fixture`,
//! which the script runner seeds with too), the navigation helpers, and a few
//! readers of what a test left behind.

// Each test binary uses the parts of this it needs, and cargo compiles the
// whole module into each — the standard shape of a shared test fixture.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use luma_ui::runtime::Runtime;

// Each binary uses a different subset.
#[allow(unused_imports)]
pub use gpui_agent::fixture::{config_dir, wav, Fixture, TRACK_NAME, VENUE_NAME};

#[allow(unused_imports)] // each binary uses a different subset
pub use gpui_agent::{NAV, UNTIL};

/// `body`, with the navigation helpers in front of it.
#[must_use]
pub fn script(body: &str) -> String {
    format!("{NAV}\n{body}")
}

/// Held by every test that sets `LUMA_CACHE_DIR`.
///
/// The app reads it from the process environment when a library opens (a
/// library's config directory comes from its thread's `Runtime`, but its cache
/// directory has no such seam yet), and one binary runs its tests on parallel
/// threads. Without one lock, a test can open its library on another test's
/// fake Python.
pub fn environment_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Put a stand-in genre model in the library at `storage_root`.
///
/// Tests that fake the Python workers still reach the genre stage, and that
/// stage downloads an 18 MB model before it starts its worker. The fake worker
/// fails without reading the model, so an empty file is enough, and the test
/// does not depend on the network.
pub fn stub_genre_model(storage_root: &Path) {
    let models = storage_root.join("models");
    std::fs::create_dir_all(&models).unwrap();
    std::fs::write(models.join(luma_lib::GENRE_MODEL_FILE_NAME), b"").unwrap();
}

/// `frames` of silent 16-bit mono PCM at 8 kHz, as a WAV file at `path`: the
/// smallest file an import decodes, for tests that only need a song to exist.
pub fn silent_wav(path: &Path, frames: u32) {
    let data_len = frames * 2;
    let mut bytes = Vec::with_capacity((44 + data_len) as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&8_000_u32.to_le_bytes());
    bytes.extend_from_slice(&16_000_u32.to_le_bytes());
    bytes.extend_from_slice(&2_u16.to_le_bytes());
    bytes.extend_from_slice(&16_u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    bytes.resize((44 + data_len) as usize, 0);
    std::fs::write(path, bytes).unwrap();
}

/// A fake Python at `cache` (what `LUMA_CACHE_DIR` points the worker
/// environment at) that sleeps `seconds` per call, answers the beat worker
/// with a 120 BPM grid, and fails every other worker.
///
/// What an import test wants from analysis: slow enough to watch, with one
/// stage that succeeds and the rest that fail fast once their sleep is over.
#[cfg(unix)]
pub fn install_fake_analysis(cache: &Path, seconds: f32) {
    use std::os::unix::fs::PermissionsExt;
    let bin = luma_lib::runtime::python::venv_dir(cache).join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let python = bin.join("python3");
    std::fs::write(
        &python,
        format!(
            "#!/bin/sh\nsleep {seconds}\ncase \"$1\" in\n  *beat_worker.py) printf '%s\\n' '{{\"beats\":[0.0,0.5],\"downbeats\":[0.0],\"bpm\":120.0,\"downbeat_offset\":0.0,\"beats_per_bar\":4}}' ;;\n  *) exit 1 ;;\nesac\n"
        ),
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&python).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(python, permissions).unwrap();
}

/// A runtime pointed at `config_dir` with motion snapped — what every harness
/// in this suite wants that does not go through [`Fixture::open`].
///
/// Carried in [`Config`] rather than set in the environment, so that two of
/// these may be live at once in one test binary. Everything else still falls
/// back to the environment, which keeps the escape hatches working for a human
/// running a single test by hand.
#[must_use]
pub fn runtime(config_dir: impl Into<PathBuf>) -> Runtime {
    Runtime {
        config_dir: Some(config_dir.into()),
        reduced_motion: true,
        ..Runtime::default()
    }
}

#[cfg(feature = "pixel")]
pub mod image;
