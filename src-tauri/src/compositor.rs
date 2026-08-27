//! Track Compositor
//!
//! Builds a [`Scene`] (one [`CompiledAnnotation`] per score row) for a
//! `(track, venue)` pair and installs it on the render engine. The Scene IS the
//! reusable compiled form — every annotation's pattern is lowered to an eval
//! [`Plan`] once, then `Scene::render` evaluates per-frame (cheap, seek-safe), so
//! the legacy precomputed-`LayerTimeSeries` + composite-cache machinery is gone.

use serde_json::Value;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::audio::StemCache;
use crate::database::local::venue_access::{AuthorizedVenue, Read, VenueAccess, VenueResource};
use crate::eval::context::build_resident_context;
use crate::eval::{compile::compile_pattern, CompiledAnnotation, Scene};
use crate::models::node_graph::{BeatGrid, Graph};
use crate::models::scores::TrackScore;
use crate::render_engine::RenderEngine;
use crate::storage::StorageRoot;

/// Monotonically increasing generation counter. Each `leave_track` bumps this so
/// any in-flight `composite_track` can detect it has gone stale.
static COMPOSITING_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Compiled plan per annotation id + a signature of the inputs that determine it
/// `(pattern, args, span, venue)`. Drives the **incremental composite**: a pass
/// reuses an annotation's plan when its signature is unchanged and recompiles only
/// the ones that actually changed — so tweaking one annotation's color recompiles
/// one plan, not the whole track.
type PlanCacheEntry = (u64, std::sync::Arc<crate::eval::Plan>);
static PLAN_CACHE: once_cell::sync::Lazy<
    std::sync::Mutex<std::collections::HashMap<String, PlanCacheEntry>>,
> = once_cell::sync::Lazy::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// Drop all cached plans (called on `leave_track`; positions/audio context may
/// differ for the next track, so plans must not carry over).
pub fn clear_plan_cache() {
    if let Ok(mut c) = PLAN_CACHE.lock() {
        c.clear();
    }
}

/// Signature of everything that determines an annotation's compiled plan. `z` and
/// `blend_mode` are excluded — they're Scene metadata, not plan inputs.
fn annotation_sig(
    annotation: &TrackScore,
    venue_id: &str,
    implementation_id: &str,
    graph_revision: &str,
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    annotation.pattern_id.hash(&mut h);
    venue_id.hash(&mut h);
    implementation_id.hash(&mut h);
    graph_revision.hash(&mut h);
    annotation.start_time.to_bits().hash(&mut h);
    annotation.end_time.to_bits().hash(&mut h);
    annotation.args.to_string().hash(&mut h);
    h.finish()
}

/// Bump the generation so any in-flight compositing aborts at its next check-point.
pub fn cancel_compositing() {
    COMPOSITING_GENERATION.fetch_add(1, Ordering::SeqCst);
}

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
    stem_cache: &StemCache,
    score_id: &str,
) -> Result<(), String> {
    let (_access, track_id) = authorize_track_cleanup(pool, score_id).await?;
    cancel_compositing();
    clear_plan_cache();
    render_engine.set_active_scene(None);
    host_audio.unload();
    stem_cache.remove_track(&track_id);
    Ok(())
}

async fn authorize_track_cleanup<'a>(
    pool: &'a sqlx::SqlitePool,
    score_id: &str,
) -> Result<(VenueAccess<'a, Read>, String), String> {
    let mut access = VenueAccess::<Read>::read(pool, VenueResource::Score(score_id)).await?;
    let track_id: String = sqlx::query_scalar("SELECT track_id FROM scores WHERE id = ?")
        .bind(score_id)
        .fetch_one(access.connection())
        .await
        .map_err(|error| format!("Failed to resolve track cleanup scope: {error}"))?;
    Ok((access, track_id))
}

/// Compile one score row into a [`CompiledAnnotation`] (context + plan + timeline
/// metadata). Returns `Ok(None)` for an empty graph (nothing to render).
#[allow(clippy::too_many_arguments)]
async fn compile_annotation(
    local_pool: &sqlx::SqlitePool,
    project_pool: &sqlx::SqlitePool,
    storage: &StorageRoot,
    resource_root: &Path,
    track_id: &str,
    venue_id: &str,
    annotation: &TrackScore,
    beat_grid: Option<BeatGrid>,
    graph: &Graph,
) -> Result<Option<CompiledAnnotation>, String> {
    if graph.nodes.is_empty() {
        return Ok(None);
    }

    let mut args: std::collections::HashMap<String, Value> = annotation
        .args
        .as_object()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .collect();
    // The annotation only carries arg *overrides*; fill unset args from the
    // pattern's own defaults (mirrors run_goldens / legacy
    // `arg_values.get(id).unwrap_or(default)`). Without this, a pattern-arg-fed
    // input (e.g. a gradient/stops) that the user didn't override fails to
    // resolve at compile. Assembled before the context build because the
    // selection pre-pass resolves arg-wired selections from this map.
    for ad in &graph.args {
        args.entry(ad.id.clone())
            .or_insert_with(|| ad.default_value.clone());
    }

    let span = (annotation.start_time as f32, annotation.end_time as f32);
    let (ctx, primitive_ids) = build_resident_context(
        local_pool,
        project_pool,
        storage,
        resource_root,
        track_id,
        venue_id,
        &graph.nodes,
        &graph.edges,
        &args,
        span,
        beat_grid,
    )
    .await;

    let plan =
        compile_pattern(&graph.nodes, &graph.edges, &args, ctx, primitive_ids).map_err(|e| {
            format!(
                "Failed to compile pattern {}: {:?}",
                annotation.pattern_id, e
            )
        })?;

    Ok(Some(CompiledAnnotation {
        plan: std::sync::Arc::new(plan),
        span,
        z_index: annotation.z_index,
        blend_mode: annotation.blend_mode,
    }))
}

/// Build a [`Scene`] for a `(track, venue)` from the given score rows.
pub(crate) async fn build_scene(
    local_pool: &sqlx::SqlitePool,
    project_pool: &sqlx::SqlitePool,
    storage: &StorageRoot,
    resource_root: &Path,
    track_id: &str,
    venue_id: &str,
    annotations: &[TrackScore],
) -> Result<Scene, String> {
    build_scene_with_policy(
        local_pool,
        project_pool,
        storage,
        resource_root,
        track_id,
        venue_id,
        annotations,
        false,
    )
    .await
}

/// Build a candidate scene without the live compositor's fault tolerance.
///
/// The live renderer intentionally skips a broken legacy clip so one bad row
/// cannot blank a running show. A staged agent edit has the opposite contract:
/// `check()` and previews must expose every compile error before `apply()` can
/// make the candidate live.
pub(crate) async fn build_scene_strict(
    local_pool: &sqlx::SqlitePool,
    project_pool: &sqlx::SqlitePool,
    storage: &StorageRoot,
    resource_root: &Path,
    track_id: &str,
    venue_id: &str,
    annotations: &[TrackScore],
) -> Result<Scene, String> {
    build_scene_with_policy(
        local_pool,
        project_pool,
        storage,
        resource_root,
        track_id,
        venue_id,
        annotations,
        true,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn build_scene_with_policy(
    local_pool: &sqlx::SqlitePool,
    project_pool: &sqlx::SqlitePool,
    storage: &StorageRoot,
    resource_root: &Path,
    track_id: &str,
    venue_id: &str,
    annotations: &[TrackScore],
    strict: bool,
) -> Result<Scene, String> {
    let beat_grid = load_beat_grid(local_pool, track_id).await?;
    let mut compiled = Vec::with_capacity(annotations.len());
    for annotation in annotations {
        let graph_document = match resolve_pattern_graph_document(
            local_pool,
            &annotation.pattern_id,
            Some(venue_id),
        )
        .await
        {
            Ok(document) => document,
            Err(error) if strict => {
                return Err(format!(
                    "Clip {} (pattern {}) could not resolve its graph: {error}",
                    annotation.id, annotation.pattern_id
                ));
            }
            Err(error) => {
                log::warn!(
                    "[composite] skipping annotation {} (pattern {}): {error}",
                    annotation.id,
                    annotation.pattern_id
                );
                continue;
            }
        };
        let sig = annotation_sig(
            annotation,
            venue_id,
            &graph_document.implementation_id,
            &graph_document.revision,
        );
        let span = (annotation.start_time as f32, annotation.end_time as f32);

        // Incremental live compositing may reuse the cache. A strict candidate
        // check must compile from authoritative inputs every time. Live cache
        // entries include the resolved implementation and graph revision, so a
        // save or venue-override change can never reuse another graph's plan.
        // Beat grids, groups, and fixture geometry are invalidated by their
        // owning lifecycle paths.
        let hit = (!strict).then(|| {
            PLAN_CACHE.lock().ok().and_then(|c| {
                c.get(&annotation.id)
                    .filter(|(s, _)| *s == sig)
                    .map(|(_, p)| p.clone())
            })
        });
        let hit = hit.flatten();
        if let Some(plan) = hit {
            compiled.push(CompiledAnnotation {
                plan,
                span,
                z_index: annotation.z_index,
                blend_mode: annotation.blend_mode,
            });
            continue;
        }

        // One bad/unlowered pattern must not blank the whole track — skip it with
        // a warning and composite the rest (legacy tolerated this per-node).
        match compile_annotation(
            local_pool,
            project_pool,
            storage,
            resource_root,
            track_id,
            venue_id,
            annotation,
            beat_grid.clone(),
            &graph_document.graph,
        )
        .await
        {
            Ok(Some(ann)) => {
                // Candidate plans may contain temporary ids and must not
                // poison the live renderer's incremental cache.
                if !strict {
                    if let Ok(mut c) = PLAN_CACHE.lock() {
                        c.insert(annotation.id.clone(), (sig, ann.plan.clone()));
                    }
                }
                compiled.push(ann);
            }
            Ok(None) => {}
            Err(e) if strict => {
                return Err(format!(
                    "Clip {} (pattern {}) could not compile: {e}",
                    annotation.id, annotation.pattern_id
                ));
            }
            Err(e) => log::warn!(
                "[composite] skipping annotation {} (pattern {}): {e}",
                annotation.id,
                annotation.pattern_id
            ),
        }
    }
    Ok(Scene::new(compiled))
}

/// Live annotation passed from the editor — its in-memory state, which during a
/// drag is *ahead* of the database (edits persist on a 300ms trailing edge). The
/// editor sends these so compositing uses live args instead of stale DB rows.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveAnnotation {
    pub id: String,
    pub pattern_id: String,
    pub start_time: f64,
    pub end_time: f64,
    pub z_index: i64,
    pub blend_mode: crate::models::node_graph::BlendMode,
    #[serde(default)]
    pub args: Value,
}

impl LiveAnnotation {
    fn into_track_score(self) -> TrackScore {
        TrackScore {
            id: self.id,
            uid: None,
            score_id: String::new(),
            pattern_id: self.pattern_id,
            start_time: self.start_time,
            end_time: self.end_time,
            z_index: self.z_index,
            blend_mode: self.blend_mode,
            args: self.args,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }
}

fn live_track_scores(annotations: Option<Vec<LiveAnnotation>>) -> Option<Vec<TrackScore>> {
    annotations.map(|live| {
        live.into_iter()
            .map(LiveAnnotation::into_track_score)
            .collect()
    })
}

/// Composite all patterns on a track into a [`Scene`] and install it as the
/// render engine's active scene — unless a concurrent `leave_track` or
/// composite has already superseded this pass.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn install_track_scene(
    pool: &sqlx::SqlitePool,
    storage: &StorageRoot,
    resource_root: &Path,
    render_engine: &RenderEngine,
    track_id: &str,
    venue_id: &str,
    annotations: Option<Vec<LiveAnnotation>>,
) -> Result<(), String> {
    let generation = COMPOSITING_GENERATION.load(Ordering::SeqCst);
    let mut access = VenueAccess::<Read>::read(pool, VenueResource::Venue(venue_id)).await?;

    // Prefer the editor's live annotations (fresh args mid-drag); fall back to the
    // DB for callers that don't pass them.
    let annotations: Vec<TrackScore> = match live_track_scores(annotations) {
        // `Some([])` is an authoritative empty live document, not a request to
        // fall back to persisted rows. This is how deleting the final clip
        // clears the installed scene immediately.
        Some(live) => live,
        None => fetch_scores(&mut access, track_id).await?,
    };
    drop(access);
    if annotations.is_empty() {
        clear_plan_cache();
        render_engine.set_active_scene(None);
        return Ok(());
    }

    let scene = build_scene(
        pool,
        pool,
        storage,
        resource_root,
        track_id,
        venue_id,
        &annotations,
    )
    .await?;

    // Bail if a newer leave_track / composite invalidated this pass.
    if COMPOSITING_GENERATION.load(Ordering::SeqCst) != generation {
        return Ok(());
    }

    if scene.is_empty() {
        render_engine.set_active_scene(None);
    } else {
        render_engine.set_active_scene(Some(scene));
    }
    Ok(())
}

/// Fetch annotations for a (track, venue) pair, sorted by z_index ascending.
pub(crate) async fn fetch_scores(
    access: &mut impl AuthorizedVenue,
    track_id: &str,
) -> Result<Vec<TrackScore>, String> {
    crate::database::local::scores::get_scores_for_track(access, track_id)
        .await
        .map_err(|e| format!("Failed to fetch scores: {}", e))
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

/// Get track duration in seconds.
pub(crate) async fn get_track_duration(
    pool: &sqlx::SqlitePool,
    track_id: &str,
) -> Result<Option<f32>, String> {
    crate::database::local::tracks::get_track_duration(pool, track_id)
        .await
        .map(|opt| opt.map(|v| v as f32))
}

/// Fetch pattern graph JSON.
pub(crate) async fn fetch_pattern_graph(
    pool: &sqlx::SqlitePool,
    pattern_id: &str,
    venue_id: Option<&str>,
) -> Result<String, String> {
    let document = resolve_pattern_graph_document(pool, pattern_id, venue_id).await?;
    serde_json::to_string(&document.graph)
        .map_err(|error| format!("Failed to serialize validated pattern graph: {error}"))
}

async fn resolve_pattern_graph_document(
    pool: &sqlx::SqlitePool,
    pattern_id: &str,
    venue_id: Option<&str>,
) -> Result<crate::services::graph_documents::GraphDocument, String> {
    let implementation_id = crate::services::graph_documents::resolve_graph_implementation(
        pool, pattern_id, venue_id, None,
    )
    .await
    .map_err(|error| error.to_string())?;
    crate::services::graph_documents::load_graph_document_unscoped(
        pool,
        pattern_id,
        &implementation_id,
    )
    .await
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::{authorize_track_cleanup, fetch_pattern_graph, live_track_scores};
    use crate::models::node_graph::{Graph, NodeInstance, PatternArgDef, PatternArgType};
    use serde_json::json;
    use std::collections::HashMap;

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
        let annotations = super::fetch_scores(&mut access, &track_id)
            .await
            .expect("fetch the score");
        drop(access);
        println!("annotations: {}", annotations.len());

        let built = Instant::now();
        let scene = super::build_scene(
            &pool,
            &pool,
            &StorageRoot::from_path(db.parent().unwrap_or(Path::new(".")).to_path_buf()),
            &resource_root,
            &track_id,
            &venue_id,
            &annotations,
        )
        .await
        .expect("build the scene");
        println!("build_scene: {:.1} ms", built.elapsed().as_secs_f64() * 1e3);

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

    #[test]
    fn explicit_empty_live_annotations_remain_authoritatively_empty() {
        let scores = live_track_scores(Some(Vec::new()));
        assert!(scores.is_some_and(|scores| scores.is_empty()));
        assert!(live_track_scores(None).is_none());
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
             CREATE TABLE venue_memberships (
                venue_id TEXT NOT NULL,
                user_id TEXT NOT NULL,
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

        let (access, track_id) = authorize_track_cleanup(&pool, "score").await.unwrap();
        assert_eq!(access.venue_id(), "venue");
        assert_eq!(track_id, "track");
        drop(access);

        sqlx::query("UPDATE auth_write_admission SET active_uid = 'bob'")
            .execute(&pool)
            .await
            .unwrap();
        let unauthorized = authorize_track_cleanup(&pool, "score").await.err().unwrap();
        assert_eq!(unauthorized, "Venue resource not found");
    }

    #[tokio::test]
    async fn runtime_resolves_the_venue_implementation_before_loading_graph() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query("CREATE TABLE patterns (id TEXT PRIMARY KEY)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE implementations (
                id TEXT PRIMARY KEY,
                pattern_id TEXT NOT NULL,
                name TEXT,
                graph_json TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE venue_implementation_overrides (
                venue_id TEXT NOT NULL,
                pattern_id TEXT NOT NULL,
                implementation_id TEXT NOT NULL,
                PRIMARY KEY (venue_id, pattern_id)
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO patterns (id) VALUES ('pattern')")
            .execute(&pool)
            .await
            .unwrap();

        let default = Graph {
            nodes: Vec::new(),
            edges: Vec::new(),
            args: Vec::new(),
        };
        let venue = Graph {
            nodes: vec![NodeInstance {
                id: "pattern_args".into(),
                type_id: "pattern_args".into(),
                params: HashMap::new(),
                position_x: None,
                position_y: None,
            }],
            edges: Vec::new(),
            args: vec![PatternArgDef {
                id: "gain".into(),
                name: "gain".into(),
                arg_type: PatternArgType::Scalar,
                default_value: json!(0.5),
            }],
        };
        sqlx::query(
            "INSERT INTO implementations (id, pattern_id, name, graph_json)
             VALUES ('default', 'pattern', NULL, ?), ('venue', 'pattern', 'venue', ?)",
        )
        .bind(serde_json::to_string(&default).unwrap())
        .bind(serde_json::to_string(&venue).unwrap())
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO venue_implementation_overrides
             (venue_id, pattern_id, implementation_id)
             VALUES ('club', 'pattern', 'venue')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let default_loaded: Graph =
            serde_json::from_str(&fetch_pattern_graph(&pool, "pattern", None).await.unwrap())
                .unwrap();
        let venue_loaded: Graph = serde_json::from_str(
            &fetch_pattern_graph(&pool, "pattern", Some("club"))
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(default_loaded.args.is_empty());
        assert_eq!(venue_loaded.args[0].id, "gain");
    }
}
