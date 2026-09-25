//! What the cargo suites share: the seeded library (now in the library as
//! `gpui_agent::fixture`, so the script runner seeds the same way), the
//! navigation helpers, and a few readers of what a test left behind.

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

/// `until(what, pred)` in a script — see `until.js`.
pub const UNTIL: &str = include_str!("until.js");

/// `nav.*` in a script — the suite's one description of how to reach a view.
///
/// Carries [`UNTIL`] with it, because every step polls: a test that spliced
/// this alone would get a `nav` whose first call died on an undefined `until`.
pub const NAV: &str = concat!(include_str!("until.js"), include_str!("nav.js"));

/// `body`, with the navigation helpers in front of it.
#[must_use]
pub fn script(body: &str) -> String {
    format!("{NAV}\n{body}")
}

/// Held by every test that sets `LUMA_CONFIG_DIR` or `LUMA_CACHE_DIR`.
///
/// The backend reads both from the process environment when a library opens,
/// and one binary runs its tests on parallel threads. Without one lock, a test
/// can open its library on another test's fake Python or config.
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

/// One clip of the shipped preset `form`/`preset` over `start`..`start +
/// duration` beats, with `seed`.
#[must_use]
pub fn preset_clip(form: &str, preset: &str, start: f64, duration: f64, seed: u64) -> Value {
    let mut clip = luma_patterns::presets()
        .preset(form, preset)
        .expect("a shipped preset")
        .clip(start, duration);
    clip.seed = seed;
    serde_json::to_value(clip).expect("a serializable clip")
}

/// A score with one clip of the shipped preset `form`/`preset` at beats 2–6.
#[must_use]
pub fn preset_score(form: &str, preset: &str) -> Value {
    let mut document: luma_patterns::Score =
        serde_json::from_value(score(json!({}))).expect("an empty score");
    let preset = luma_patterns::presets()
        .preset(form, preset)
        .expect("a shipped preset");
    document
        .clips
        .insert("form-clip".into(), preset.clip(2.0, 4.0));
    serde_json::to_value(document).expect("a serializable score")
}

/// The score the fixture seeded, read back from its rows.
///
/// A score is `scores` and `clips`; there is no document
/// column to read any more. Tests assert on a `luma_patterns::Score`, which is
/// what those rows load into — so this is the one place that knows the
/// difference, rather than eighteen copies of a `SELECT`.
pub async fn stored_score(dir: &Path) -> luma_patterns::Score {
    let pool = sqlx::SqlitePool::connect(&format!("sqlite:{}", dir.join("luma.db").display()))
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

/// [`stored_score`] as JSON, for an assertion that reaches into the document
/// by key rather than by field.
pub async fn stored_score_json(dir: &Path) -> Value {
    serde_json::to_value(stored_score(dir).await).expect("a score serializes")
}

#[cfg(feature = "pixel")]
pub mod image;
pub mod session;

/// Reading `app.timings()` — shared so two suites cannot disagree about what a
/// percentile means.
///
/// These are the CPU half of a frame (scene build), never the GPU: see
/// `app.timings()` in `api.d.ts`.
pub mod cost {
    use serde_json::Value;

    /// Nearest-rank percentile. Sorts in place.
    pub fn percentile(sample: &mut Vec<f64>, fraction: f64) -> f64 {
        sample.sort_by(|a, b| a.partial_cmp(b).unwrap());
        if sample.is_empty() {
            return f64::NAN;
        }
        sample[(((sample.len() - 1) as f64) * fraction).round() as usize]
    }

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
        let p50 = percentile(&mut draw.clone(), 0.50);
        let p95 = percentile(&mut draw, 0.95);
        println!(
            "{label:<26} n={count:<4} drawMs mean={mean:6.2} p50={p50:6.2} p95={p95:6.2}  \
             parkedMs p50={:5.2}",
            percentile(&mut parked, 0.50)
        );
        p50
    }
}
