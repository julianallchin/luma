//! Track Compositor
//!
//! Builds a [`Scene`] (one [`CompiledAnnotation`] per score row) for a
//! `(track, venue)` pair and installs it on the render engine. The Scene IS the
//! reusable compiled form — every annotation's pattern is lowered to an eval
//! [`Plan`] once, then `Scene::render` evaluates per-frame (cheap, seek-safe).

use std::path::Path;

use crate::database::local::venue_access::{AuthorizedVenue, Read, VenueAccess, VenueResource};
use crate::eval::Scene;
use crate::models::node_graph::BeatGrid;
use crate::render_engine::RenderEngine;
use crate::storage::StorageRoot;

/// Cancel compositing, clear the render engine's active scene, and unload audio.
/// Called when navigating away from the track editor.
///
/// Takes the *score* id, not the track id: the track is resolved under the
/// score's venue read authorization, so this must run before the score row is
/// deleted.
pub(crate) async fn leave_track(
    pool: &sqlx::SqlitePool,
    render_engine: &RenderEngine,
    host_audio: &crate::host_audio::HostAudioState,
    score_id: &str,
) -> Result<(), String> {
    score_scope(pool, score_id).await?;
    render_engine.set_active_scene(None);
    host_audio.unload();
    Ok(())
}

/// Open a score for reading and say which track it annotates.
///
/// The venue comes back inside the access ([`AuthorizedVenue::venue_id`]), so
/// this is the whole `(track, venue)` scope a score implies — resolved *under*
/// the score's own authorization rather than taken on trust from a caller.
async fn score_scope<'a>(
    pool: &'a sqlx::SqlitePool,
    score_id: &str,
) -> Result<(VenueAccess<'a, Read>, String), String> {
    let mut access = VenueAccess::<Read>::read(pool, VenueResource::Score(score_id)).await?;
    let track_id: String = sqlx::query_scalar("SELECT track_id FROM scores WHERE id = ?")
        .bind(score_id)
        .fetch_one(access.connection())
        .await
        .map_err(|error| format!("Failed to resolve the score's track: {error}"))?;
    Ok((access, track_id))
}

/// Install **one score's** light show as the render engine's active scene.
///
/// The score is the subject, not the `(track, venue)` pair it sits on: a pair
/// carries as many scores as there are people who annotated it, and blending
/// them would light the rig with a document nobody is looking at. Which one is
/// on screen is the caller's fact. Building a candidate never changes another
/// render slot, and installation rejects an update superseded while compiling.
///
/// `graph_score` is the editor's working copy when it has one, and `None` for
/// a caller that is only *watching* the score, which then reads the score's own
/// rows.
pub(crate) async fn install_score_scene(
    pool: &sqlx::SqlitePool,
    storage: &StorageRoot,
    resource_root: &Path,
    render_engine: &RenderEngine,
    score_id: &str,
    graph_score: Option<luma_patterns::Score>,
) -> Result<(), String> {
    let update = render_engine.begin_scene_update(crate::render_engine::SceneTarget::Active);
    let scene = build_score_scene(pool, storage, resource_root, score_id, graph_score).await?;
    render_engine.finish_scene_update(update, scene);
    Ok(())
}

/// Resolve and compile one score, including a caller's optional working copy.
/// Rendering, performance decks and agent previews share this format boundary.
pub async fn build_score_scene(
    pool: &sqlx::SqlitePool,
    storage: &StorageRoot,
    resource_root: &Path,
    score_id: &str,
    graph_score: Option<luma_patterns::Score>,
) -> Result<Scene, String> {
    let (mut access, track_id) = score_scope(pool, score_id).await?;
    let score = match graph_score {
        Some(score) => score,
        None => {
            crate::database::local::scores::rows::load_score(access.connection(), score_id).await?
        }
    };
    crate::services::graph_scores::prepare_scene(
        &mut access,
        resource_root,
        storage,
        &track_id,
        &score,
    )
    .await
}

/// Load beat grid for a track.
pub(crate) async fn load_beat_grid(
    pool: &sqlx::SqlitePool,
    track_id: &str,
) -> Result<Option<BeatGrid>, String> {
    crate::services::tracks::get_track_beats(pool, track_id)
        .await
        .map_err(|e| format!("Failed to load beat grid: {}", e))
}

#[cfg(test)]
mod tests {
    use super::score_scope;
    use crate::database::local::venue_access::AuthorizedVenue;

    /// Profiling instrument, not a gate: samples a real installed score across a
    /// time window and prints per-frame eval cost, so a "it lags right *here*"
    /// report can be attributed to the score rather than guessed at.
    ///
    /// Points at a COPY of a library; never open the live one, whose migrations
    /// would write to it.
    ///
    ///   LUMA_PROF_DB=/path/luma.db LUMA_PROF_TRACK=<id> LUMA_PROF_VENUE=<id> \
    ///   LUMA_PROF_FROM=45 LUMA_PROF_TO=52 \
    ///   cargo test -p luma --lib compositor::tests::profile_ -- --ignored --nocapture
    #[tokio::test]
    #[ignore = "profiling instrument; needs a library copy via LUMA_PROF_DB"]
    async fn profile_a_real_score_across_a_window() {
        use crate::eval::{Arena, Scope};
        use crate::storage::StorageRoot;
        use std::path::{Path, PathBuf};
        use std::time::Instant;

        let env = |key: &str| std::env::var(key).unwrap_or_else(|_| panic!("{key} must be set"));
        let db = PathBuf::from(env("LUMA_PROF_DB"));
        let track_id = env("LUMA_PROF_TRACK");
        let venue_id = env("LUMA_PROF_VENUE");
        let from: f32 = env("LUMA_PROF_FROM").parse().expect("LUMA_PROF_FROM");
        let to: f32 = env("LUMA_PROF_TO").parse().expect("LUMA_PROF_TO");
        let resource_root = PathBuf::from(env("LUMA_PROF_FIXTURES"));

        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(4)
            .connect(&format!("sqlite://{}", db.display()))
            .await
            .expect("open the library copy");

        let mut access = crate::database::local::venue_access::VenueAccess::<
            crate::database::local::venue_access::Read,
        >::read(
            &pool,
            crate::database::local::venue_access::VenueResource::Venue(&venue_id),
        )
        .await
        .expect("authorize the venue");
        let score_id: String =
            sqlx::query_scalar("SELECT id FROM scores WHERE track_id = ? AND venue_id = ?")
                .bind(&track_id)
                .bind(&venue_id)
                .fetch_one(access.connection())
                .await
                .expect("find the score");
        drop(access);

        let built = Instant::now();
        let scene = super::build_score_scene(
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
            let _ = scene.render(&[t], Scope::Composite, &mut scratch);
            let at = Instant::now();
            let out = scene.render(&[t], Scope::Composite, &mut scratch);
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

    #[tokio::test]
    async fn track_cleanup_derives_the_track_from_an_admitted_score() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE auth_write_admission (
                singleton INTEGER PRIMARY KEY,
                armed INTEGER NOT NULL,
                accepting INTEGER NOT NULL,
                maintenance INTEGER NOT NULL,
                active_uid TEXT
             );
             INSERT INTO auth_write_admission VALUES (1, 1, 1, 0, 'alice');
             CREATE TABLE venues (
                id TEXT PRIMARY KEY,
                uid TEXT,
                role TEXT NOT NULL DEFAULT 'owner'
             );
             CREATE TABLE venue_members (
                id TEXT PRIMARY KEY,
                uid TEXT NOT NULL,
                venue_id TEXT NOT NULL,
                role TEXT NOT NULL
             );
             CREATE TABLE scores (
                id TEXT PRIMARY KEY,
                venue_id TEXT NOT NULL,
                track_id TEXT NOT NULL
             );
             INSERT INTO venues (id, uid) VALUES ('venue', 'alice');
             INSERT INTO scores VALUES ('score', 'venue', 'track')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let (access, track_id) = score_scope(&pool, "score").await.unwrap();
        assert_eq!(access.venue_id(), "venue");
        assert_eq!(track_id, "track");
        drop(access);

        sqlx::query("UPDATE auth_write_admission SET active_uid = 'bob'")
            .execute(&pool)
            .await
            .unwrap();
        let unauthorized = score_scope(&pool, "score").await.err().unwrap();
        assert_eq!(unauthorized, "Venue resource not found");
    }
}
