//! Topological scheduler + parallel dispatcher for the preprocessing DAG.
//!
//! Responsibilities:
//! 1. Topo-sort the registered preprocessors into layers — preprocessors in
//!    the same layer have no dependencies on each other and can run in
//!    parallel for a given track. Cycles panic at startup (programming error).
//! 2. Per-track planning — given a registry, return only the subset of
//!    preprocessors whose outputs are missing or stale for that track.
//! 3. Per-track execution — run the planned set in topological order,
//!    parallelising siblings within each layer.
//! 4. Multi-track execution — process many tracks with bounded concurrency.
//! 5. In-flight dedup — concurrent calls for the same (track, preprocessor)
//!    coalesce so we never run the same heavyweight worker twice in parallel.
//! 6. Startup reconciliation — query for any track with stale or missing
//!    preprocessor runs and queue them.
//!
//! On preprocessor failure: log + Sentry capture, persist a `failed` row in
//! `preprocessing_runs`, skip downstream preprocessors for this track in
//! this run, then continue with other tracks. The next startup will retry.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use once_cell::sync::OnceCell;
use sqlx::SqlitePool;
use tauri::{AppHandle, Emitter};
use tokio::sync::{Mutex, Semaphore};
use tokio::task::JoinSet;

use crate::audio::StemCache;
use crate::database::local::tracks as tracks_db;
use crate::preprocessing::artifact::Artifact;
use crate::preprocessing::preprocessor::{Preprocessor, PreprocessorContext, PreprocessorRef};
use crate::preprocessing::registry;
use crate::preprocessing::state;
use crate::services::tracks::{analysis_worker_count, ensure_storage, storage_dirs};

// -----------------------------------------------------------------------------
// Topological sort
// -----------------------------------------------------------------------------

/// A layered topological ordering — `layers[i]` may run in parallel; layer
/// `i+1` depends only on outputs from layers `0..=i`.
#[derive(Clone)]
pub struct Layered {
    layers: Vec<Vec<PreprocessorRef>>,
}

impl Layered {
    pub fn layers(&self) -> &[Vec<PreprocessorRef>] {
        &self.layers
    }
}

/// Topo-sort `preprocessors` into layers. Panics if a cycle is detected
/// (this is a programming error in the registry, not a runtime condition).
pub fn topo_layers(preprocessors: &[PreprocessorRef]) -> Layered {
    // Map artifact -> producer name (each artifact has at most one producer).
    let mut producer_of: HashMap<Artifact, &'static str> = HashMap::new();
    for p in preprocessors {
        if let Some(prev) = producer_of.insert(p.output(), p.name()) {
            panic!(
                "Two preprocessors produce the same artifact {:?}: {} and {}",
                p.output(),
                prev,
                p.name()
            );
        }
    }

    // For each preprocessor, the set of preprocessor names it depends on.
    let mut deps: HashMap<&'static str, HashSet<&'static str>> = HashMap::new();
    for p in preprocessors {
        let mut required = HashSet::new();
        for input in p.inputs() {
            if matches!(input, Artifact::Audio) {
                continue;
            }
            let producer = producer_of.get(input).unwrap_or_else(|| {
                panic!(
                    "Preprocessor {} depends on artifact {:?} which has no producer",
                    p.name(),
                    input
                )
            });
            required.insert(*producer);
        }
        deps.insert(p.name(), required);
    }

    // Kahn's algorithm — peel layers of nodes with no remaining deps.
    let mut remaining: HashMap<&'static str, HashSet<&'static str>> = deps;
    let mut by_name: HashMap<&'static str, PreprocessorRef> = preprocessors
        .iter()
        .map(|p| (p.name(), p.clone()))
        .collect();
    let mut layers: Vec<Vec<PreprocessorRef>> = Vec::new();
    while !remaining.is_empty() {
        let ready: Vec<&'static str> = remaining
            .iter()
            .filter_map(|(name, deps)| if deps.is_empty() { Some(*name) } else { None })
            .collect();
        if ready.is_empty() {
            let stuck: Vec<&str> = remaining.keys().copied().collect();
            panic!("Cycle detected in preprocessor DAG; nodes still pending: {stuck:?}");
        }
        let mut layer: Vec<PreprocessorRef> = Vec::with_capacity(ready.len());
        for name in &ready {
            remaining.remove(name);
            if let Some(p) = by_name.remove(name) {
                layer.push(p);
            }
        }
        for deps in remaining.values_mut() {
            for name in &ready {
                deps.remove(name);
            }
        }
        layers.push(layer);
    }
    Layered { layers }
}

// -----------------------------------------------------------------------------
// In-flight dedup
// -----------------------------------------------------------------------------

/// Tracks (track_id, preprocessor_name) pairs currently executing so a
/// duplicate request awaits the in-flight one rather than racing it.
#[derive(Default)]
struct InflightSet {
    inner: Mutex<HashMap<(String, &'static str), Arc<tokio::sync::Notify>>>,
}

impl InflightSet {
    /// Try to claim execution. Returns `Some(notify)` if the caller should
    /// run, `None` if another task is already running and the caller waited
    /// for it to finish.
    async fn claim(&self, track_id: &str, name: &'static str) -> Option<Arc<tokio::sync::Notify>> {
        loop {
            let waiter: Arc<tokio::sync::Notify>;
            {
                let mut guard = self.inner.lock().await;
                if let Some(existing) = guard.get(&(track_id.to_string(), name)) {
                    waiter = existing.clone();
                } else {
                    let notify = Arc::new(tokio::sync::Notify::new());
                    guard.insert((track_id.to_string(), name), notify.clone());
                    return Some(notify);
                }
            }
            waiter.notified().await;
            // Recheck — likely the previous run finished and removed itself.
            let guard = self.inner.lock().await;
            if !guard.contains_key(&(track_id.to_string(), name)) {
                return None;
            }
            // Otherwise loop and re-wait on the new in-flight run.
        }
    }

    async fn release(&self, track_id: &str, name: &'static str) {
        let mut guard = self.inner.lock().await;
        if let Some(notify) = guard.remove(&(track_id.to_string(), name)) {
            notify.notify_waiters();
        }
    }
}

fn inflight() -> &'static InflightSet {
    static SET: OnceCell<InflightSet> = OnceCell::new();
    SET.get_or_init(InflightSet::default)
}

// -----------------------------------------------------------------------------
// Public API
// -----------------------------------------------------------------------------

/// Plan for a single track: returns the subset of `preprocessors` (in topo
/// order across layers) that need to run because their output is missing
/// or stale at the current version.
#[allow(dead_code)]
pub async fn plan_for_track(
    pool: &SqlitePool,
    app_handle: &AppHandle,
    stem_cache: &StemCache,
    track_id: &str,
    preprocessors: &[PreprocessorRef],
) -> Result<Vec<PreprocessorRef>, String> {
    let track = tracks_db::get_track_by_id(pool, track_id)
        .await?
        .ok_or_else(|| format!("Track {track_id} not found"))?;
    ensure_storage(app_handle)?;
    let (_, _, stems_dir) = storage_dirs(app_handle)?;
    let ctx = PreprocessorContext::new(pool, app_handle, stem_cache, &track, stems_dir);

    let layered = topo_layers(preprocessors);
    let mut plan = Vec::new();
    for layer in layered.layers() {
        for p in layer {
            if !p.is_complete(&ctx, track_id).await? {
                plan.push(p.clone());
            }
        }
    }
    Ok(plan)
}

/// Plan + execute for a single track. Layers run sequentially; within each
/// layer, siblings run concurrently. A failed preprocessor records its error
/// and skips its downstream preprocessors for this run.
pub async fn run_for_track(
    pool: &SqlitePool,
    app_handle: &AppHandle,
    stem_cache: &StemCache,
    track_id: &str,
    preprocessors: &[PreprocessorRef],
) -> Result<(), String> {
    let track = tracks_db::get_track_by_id(pool, track_id)
        .await?
        .ok_or_else(|| format!("Track {track_id} not found"))?;
    ensure_storage(app_handle)?;
    let (_, _, stems_dir) = storage_dirs(app_handle)?;

    let layered = topo_layers(preprocessors);

    // Track which artifacts failed in this run so we can skip dependents.
    let mut failed_artifacts: HashSet<Artifact> = HashSet::new();

    for layer in layered.layers() {
        // Filter layer: skip preprocessors whose inputs failed, or whose
        // output is already complete.
        let mut to_run: Vec<PreprocessorRef> = Vec::new();
        let ctx = PreprocessorContext::new(pool, app_handle, stem_cache, &track, stems_dir.clone());
        for p in layer {
            let blocked = p
                .inputs()
                .iter()
                .any(|input| failed_artifacts.contains(input));
            if blocked {
                eprintln!(
                    "[preprocessing] skipping {} for track {track_id}: upstream failed",
                    p.name()
                );
                failed_artifacts.insert(p.output());
                continue;
            }
            if p.is_complete(&ctx, track_id).await? {
                continue;
            }
            to_run.push(p.clone());
        }

        if to_run.is_empty() {
            continue;
        }

        // Spawn one task per preprocessor in this layer. Each task takes its
        // own owned context so it can be `'static`.
        let mut set: JoinSet<(&'static str, Artifact, Result<(), String>)> = JoinSet::new();
        for p in to_run {
            let pool = pool.clone();
            let app_handle = app_handle.clone();
            let stem_cache = stem_cache.clone();
            let track = track.clone();
            let stems_dir = stems_dir.clone();
            let track_id_owned = track_id.to_string();
            set.spawn(async move {
                let ctx =
                    PreprocessorContext::new(&pool, &app_handle, &stem_cache, &track, stems_dir);
                let res = run_one(&ctx, &track_id_owned, p.as_ref()).await;
                (p.name(), p.output(), res)
            });
        }

        while let Some(joined) = set.join_next().await {
            let (name, output, result) = joined.map_err(|e| format!("Join error: {e}"))?;
            if let Err(err) = result {
                eprintln!("[preprocessing] {name} failed for track {track_id}: {err}");
                sentry::capture_message(
                    &format!("Preprocessor {name} failed for track {track_id}: {err}"),
                    sentry::Level::Error,
                );
                failed_artifacts.insert(output);
            }
        }
    }

    Ok(())
}

/// Run a single preprocessor for a single track, with state-table
/// bookkeeping, status emission, and in-flight dedup.
async fn run_one(
    ctx: &PreprocessorContext<'_>,
    track_id: &str,
    p: &dyn Preprocessor,
) -> Result<(), String> {
    let claim = inflight().claim(track_id, p.name()).await;
    if claim.is_none() {
        // Another task ran it; if it succeeded, we're done. If it failed
        // we'll see that on the next is_complete check below.
        return if p.is_complete(ctx, track_id).await? {
            Ok(())
        } else {
            Err(format!(
                "Concurrent {} run for track {track_id} did not complete",
                p.name()
            ))
        };
    }

    state::upsert_run_started(ctx.pool(), track_id, p.name(), p.version()).await?;
    let _ = ctx
        .app_handle()
        .emit("track-import-progress", (track_id, p.status_label()));

    let result = p.run(ctx, track_id).await;

    match &result {
        Ok(()) => {
            state::mark_run_completed(ctx.pool(), track_id, p.name(), p.version()).await?;
            let _ = ctx.app_handle().emit("track-status-changed", track_id);
        }
        Err(err) => {
            state::mark_run_failed(ctx.pool(), track_id, p.name(), p.version(), err).await?;
        }
    }

    inflight().release(track_id, p.name()).await;
    result
}

/// Multi-track entry point. Bounded parallelism via Semaphore; per-track
/// scheduling delegates to [`run_for_track`].
pub async fn run_for_tracks(
    pool: SqlitePool,
    app_handle: AppHandle,
    stem_cache: StemCache,
    track_ids: Vec<String>,
) {
    let total = track_ids.len();
    let preprocessors = registry::registered_preprocessors();
    // Validate the DAG once up front — panics on cycle.
    let _ = topo_layers(&preprocessors);
    let max_parallel = analysis_worker_count();
    let semaphore = Arc::new(Semaphore::new(max_parallel));
    let completed = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let mut handles = Vec::with_capacity(total);
    for track_id in track_ids {
        let pool = pool.clone();
        let app_handle = app_handle.clone();
        let stem_cache = stem_cache.clone();
        let preprocessors = preprocessors.clone();
        let sem = semaphore.clone();
        let completed = completed.clone();

        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.expect("semaphore closed");
            let done = completed.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            let _ = app_handle.emit(
                "track-import-progress",
                (
                    track_id.as_str(),
                    format!("Analyzing track {done}/{total}…"),
                ),
            );
            if let Err(e) =
                run_for_track(&pool, &app_handle, &stem_cache, &track_id, &preprocessors).await
            {
                eprintln!("[preprocessing] track {track_id} failed: {e}");
                sentry::capture_message(
                    &format!("Preprocessing failed for track {track_id}: {e}"),
                    sentry::Level::Error,
                );
            }
        }));
    }

    for handle in handles {
        let _ = handle.await;
    }
    let _ = app_handle.emit("track-import-complete", total);
    eprintln!("[preprocessing] finished all {total} tracks ({max_parallel} parallel workers)");
}

/// Startup reconciliation: identify any (track, preprocessor) pair where the
/// run is missing or at a stale version, and queue those tracks.
pub async fn reconcile_on_startup(
    pool: SqlitePool,
    app_handle: AppHandle,
    stem_cache: StemCache,
) -> Result<(), String> {
    let preprocessors = registry::registered_preprocessors();
    // Validate DAG (panics on cycle).
    let _ = topo_layers(&preprocessors);

    let track_ids: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM tracks
         WHERE file_path IS NOT NULL
           AND file_path != ''
           AND file_path NOT LIKE '%.stub'",
    )
    .fetch_all(&pool)
    .await
    .map_err(|e| format!("Failed to list tracks for reconciliation: {e}"))?;

    let expected: Vec<(&str, u32)> = preprocessors
        .iter()
        .map(|p| (p.name(), p.version()))
        .collect();

    let mut needs = Vec::new();
    for id in track_ids {
        let stale = state::list_stale(&pool, &id, &expected).await?;
        if !stale.is_empty() {
            needs.push(id);
        }
    }

    if needs.is_empty() {
        return Ok(());
    }

    eprintln!(
        "[preprocessing] {} tracks need preprocessing, queueing...",
        needs.len()
    );
    run_for_tracks(pool, app_handle, stem_cache, needs).await;
    Ok(())
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    /// In-memory test pool with just the `preprocessing_runs` table.
    async fn test_pool() -> SqlitePool {
        let opts = SqliteConnectOptions::new()
            .filename(":memory:")
            .create_if_missing(true)
            .foreign_keys(false);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .expect("failed to create in-memory pool");
        sqlx::query(
            "CREATE TABLE preprocessing_runs (
                track_id TEXT NOT NULL,
                preprocessor TEXT NOT NULL,
                version INTEGER NOT NULL,
                status TEXT NOT NULL CHECK (status IN ('running','completed','failed')),
                started_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')),
                completed_at TEXT,
                error TEXT,
                PRIMARY KEY (track_id, preprocessor)
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    /// Test-only preprocessor stub used to exercise topo + planning.
    struct StubProc {
        name: &'static str,
        version: u32,
        inputs: &'static [Artifact],
        output: Artifact,
    }

    #[async_trait]
    impl Preprocessor for StubProc {
        fn name(&self) -> &'static str {
            self.name
        }
        fn version(&self) -> u32 {
            self.version
        }
        fn inputs(&self) -> &'static [Artifact] {
            self.inputs
        }
        fn output(&self) -> Artifact {
            self.output
        }
        fn status_label(&self) -> &'static str {
            "stub"
        }
        async fn run(&self, _ctx: &PreprocessorContext<'_>, _track_id: &str) -> Result<(), String> {
            Ok(())
        }
    }

    fn registered_three() -> Vec<PreprocessorRef> {
        registry::registered_preprocessors()
    }

    #[test]
    fn topo_layers_orders_real_registry() {
        let layered = topo_layers(&registered_three());
        // Expect two layers: { beat_grid, stems } then { roots }.
        assert_eq!(layered.layers().len(), 2);
        let layer0_names: HashSet<_> = layered.layers()[0].iter().map(|p| p.name()).collect();
        assert!(layer0_names.contains("beat_grid"));
        assert!(layer0_names.contains("stems"));
        assert_eq!(layer0_names.len(), 2);
        let layer1_names: HashSet<_> = layered.layers()[1].iter().map(|p| p.name()).collect();
        assert_eq!(layer1_names.len(), 1);
        assert!(layer1_names.contains("roots"));
    }

    #[test]
    #[should_panic(expected = "Cycle detected")]
    fn topo_layers_panics_on_cycle() {
        // Synthetic registry where two preprocessors depend on each other's
        // outputs — should panic with "Cycle detected".
        let cyclic: Vec<PreprocessorRef> = vec![
            Arc::new(StubProc {
                name: "a",
                version: 1,
                inputs: &[Artifact::Roots],
                output: Artifact::BeatGrid,
            }),
            Arc::new(StubProc {
                name: "b",
                version: 1,
                inputs: &[Artifact::BeatGrid],
                output: Artifact::Roots,
            }),
        ];
        let _ = topo_layers(&cyclic);
    }

    #[tokio::test]
    async fn version_bump_invalidates_completed_run() {
        let pool = test_pool().await;
        state::mark_run_completed(&pool, "track1", "beat_grid", 1)
            .await
            .unwrap();
        assert!(state::has_completed_run(&pool, "track1", "beat_grid", 1)
            .await
            .unwrap());
        // Asking about v2 should be false — bump invalidates.
        assert!(!state::has_completed_run(&pool, "track1", "beat_grid", 2)
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn list_stale_returns_only_missing_or_stale() {
        let pool = test_pool().await;
        state::mark_run_completed(&pool, "track1", "beat_grid", 1)
            .await
            .unwrap();
        state::mark_run_completed(&pool, "track1", "stems", 1)
            .await
            .unwrap();
        // roots is missing; beat_grid is at v1 but expected v2 (stale).
        let stale = state::list_stale(
            &pool,
            "track1",
            &[("beat_grid", 2), ("stems", 1), ("roots", 1)],
        )
        .await
        .unwrap();
        assert_eq!(stale.len(), 2);
        assert!(stale.contains(&"beat_grid".to_string()));
        assert!(stale.contains(&"roots".to_string()));
        assert!(!stale.contains(&"stems".to_string()));
    }

    #[tokio::test]
    async fn inflight_dedup_coalesces_concurrent_claims() {
        let set: Arc<InflightSet> = Arc::new(InflightSet::default());
        let claim1 = set.claim("trackA", "beat_grid").await;
        assert!(claim1.is_some(), "first claim should succeed");
        let set2 = set.clone();
        let task = tokio::spawn(async move { set2.claim("trackA", "beat_grid").await });
        // Yield so the second claim begins waiting.
        tokio::task::yield_now().await;
        // Release the first claim — second should observe completion and return None.
        set.release("trackA", "beat_grid").await;
        let claim2 = task.await.unwrap();
        assert!(claim2.is_none(), "second concurrent claim should coalesce");
    }
}
