//! What the cargo suites share: the seeded library (`gpui_agent::fixture`,
//! which the script runner seeds with too), the navigation helpers, and a few
//! readers of what a test left behind.

// Each test binary uses the parts of this it needs, and cargo compiles the
// whole module into each — the standard shape of a shared test fixture.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use luma_ui::runtime::Runtime;
use serde_json::{json, Value};

// Each binary uses a different subset.
#[allow(unused_imports)]
pub use gpui_agent::fixture::{
    config_dir, seeded_prompt, wav, Clip, Fixture, MOVER_PATH, SKEW_DEPTH_M, TRACK, TRACK_NAME,
    VENUE, VENUE_NAME,
};

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

/// A whole score document from clips spelled by the caller.
#[must_use]
pub fn score(clips: Value) -> Value {
    json!({ "clips": clips })
}

/// The score the fixture seeded, read back from its `scores` and `clips` rows
/// as the `luma_patterns::Score` they load into.
pub async fn stored_score(dir: &Path) -> luma_patterns::Score {
    let pool = sqlx::SqlitePool::connect(&format!(
        "sqlite:{}",
        gpui_agent::fixture::library_db(dir).display()
    ))
    .await
    .expect("open the fixture library");
    let id: String = sqlx::query_scalar("SELECT score_id FROM clips LIMIT 1")
        .fetch_one(&pool)
        .await
        .expect("the fixture seeded a score with something in it");
    let mut connection = pool.acquire().await.expect("a connection");
    let score = luma_lib::database::local::scores::rows::load_score(&mut connection, &id)
        .await
        .expect("load the seeded score");
    drop(connection);
    pool.close().await;
    score
}

#[cfg(feature = "pixel")]
pub mod image;

/// Reading `app.timings()`.
///
/// These are the CPU half of a frame (scene build), never the GPU: see
/// `app.timings()` in `api.d.ts`.
pub mod cost {
    use gpui_agent::percentile;
    use serde_json::Value;

    /// Print what the frames in `(from, to]` cost, and hand back the median.
    ///
    /// Half-open on purpose: callers mark a stretch with the frame number a
    /// command returned, and that frame belongs to the stretch before it.
    pub fn summarize(frames: &[Value], from: u64, to: u64, label: &str) -> f64 {
        let mut draw: Vec<f64> = Vec::new();
        let mut parked: Vec<f64> = Vec::new();
        for frame in frames {
            let number = frame["frame"].as_u64().unwrap();
            if number > from && number <= to {
                draw.push(frame["drawMs"].as_f64().unwrap());
                parked.push(frame["parkedMs"].as_f64().unwrap());
            }
        }
        let count = draw.len();
        let mean = draw.iter().sum::<f64>() / count.max(1) as f64;
        let p50 = percentile(&draw, 0.50);
        let p95 = percentile(&draw, 0.95);
        println!(
            "{label:<26} n={count:<4} drawMs mean={mean:6.2} p50={p50:6.2} p95={p95:6.2}  \
             parkedMs p50={:5.2}",
            percentile(&parked, 0.50)
        );
        p50
    }
}
