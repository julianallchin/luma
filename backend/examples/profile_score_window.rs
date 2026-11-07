//! Profiling instrument, not a gate: samples a real installed score across a
//! time window and prints per-frame eval cost, so a "it lags right *here*"
//! report can be attributed to the score rather than guessed at.
//!
//! Points at a COPY of a library, opened read-only.
//!
//! ```text
//! LUMA_PROF_DB=/path/luma.db LUMA_PROF_TRACK=<id> LUMA_PROF_VENUE=<id> \
//! LUMA_PROF_FIXTURES=/path/resources/fixtures LUMA_PROF_FROM=45 LUMA_PROF_TO=52 \
//! cargo run --manifest-path backend/Cargo.toml --release --example profile_score_window
//! ```

use std::path::{Path, PathBuf};
use std::time::Instant;

use luma_lib::eval::{Arena, Scope};
use luma_lib::storage::StorageRoot;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let env = |key: &str| std::env::var(key).unwrap_or_else(|_| panic!("{key} must be set"));
    let db = PathBuf::from(env("LUMA_PROF_DB"));
    let track_id = env("LUMA_PROF_TRACK");
    let venue_id = env("LUMA_PROF_VENUE");
    let from: f32 = env("LUMA_PROF_FROM").parse().expect("LUMA_PROF_FROM");
    let to: f32 = env("LUMA_PROF_TO").parse().expect("LUMA_PROF_TO");
    let resource_root = PathBuf::from(env("LUMA_PROF_FIXTURES"));

    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect_with(SqliteConnectOptions::new().filename(&db).read_only(true))
        .await
        .expect("open the library copy");
    let score_id: String =
        sqlx::query_scalar("SELECT id FROM scores WHERE track_id = ? AND venue_id = ?")
            .bind(&track_id)
            .bind(&venue_id)
            .fetch_one(&pool)
            .await
            .expect("find the score");

    let built = Instant::now();
    let scene = luma_lib::build_score_scene(
        &pool,
        &StorageRoot::from_path(db.parent().unwrap_or(Path::new(".")).to_path_buf()),
        &resource_root,
        &score_id,
        None,
    )
    .await
    .expect("build the scene");
    println!(
        "build_score_scene: {:.1} ms",
        built.elapsed().as_secs_f64() * 1e3
    );

    // One frame at a time, exactly as the live path samples it — a batched
    // `times` slice would amortise per-call costs the renderer never gets to.
    let mut scratch = Arena::default();
    let mut worst: Vec<(f32, f64)> = Vec::new();
    let step = 1.0 / 60.0;
    let mut t = from;
    while t <= to {
        // Warm the frame once so the number is steady-state, then measure.
        scene
            .try_render(&[t], Scope::Composite, &mut scratch)
            .expect("evaluate the frame");
        let at = Instant::now();
        let out = scene
            .try_render(&[t], Scope::Composite, &mut scratch)
            .expect("evaluate the frame");
        let ms = at.elapsed().as_secs_f64() * 1e3;
        let frame = out.first();
        let total = frame.map_or(0, |f| f.primitives.len());
        let lit = frame.map_or(0, |f| {
            f.primitives.values().filter(|p| p.dimmer > 0.001).count()
        });
        let strobing = frame.map_or(0, |f| {
            f.primitives.values().filter(|p| p.strobe > 0.001).count()
        });
        let energy: f32 = frame.map_or(0.0, |f| f.primitives.values().map(|p| p.dimmer).sum());
        println!(
            "  t={t:6.3}  {ms:6.3} ms  primitives={total}  lit={lit}  strobing={strobing}  dimmer_sum={energy:.2}"
        );
        worst.push((t, ms));
        t += step;
    }
    worst.sort_by(|a, b| b.1.total_cmp(&a.1));
    println!("--- worst 10 frames ---");
    for (t, ms) in worst.iter().take(10) {
        println!("  t={t:6.3}  {ms:7.3} ms");
    }
    let mean: f64 = worst.iter().map(|(_, ms)| ms).sum::<f64>() / worst.len() as f64;
    println!("frames={} mean={:.3} ms", worst.len(), mean);
}
