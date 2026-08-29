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
//! 6. Startup reconciliation — each preprocessor returns its own pending
//!    set via one bulk SQL query; the union is queued.
//!
//! On preprocessor failure: log + Sentry capture, record a row in
//! `preprocessing_failures` with exponential backoff, skip downstream
//! preprocessors for this track in this run, then continue with other tracks.
//! Reconcile on the next startup will pick the track up again once its
//! `next_retry_at` has elapsed.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use once_cell::sync::OnceCell;
use sqlx::SqlitePool;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::audio::StemCache;
use crate::database::local::tracks as tracks_db;
use crate::dispatch::Events;
use crate::models::tracks::{TrackImportPhase, TrackImportProgress};
use crate::preprocessing::artifact::Artifact;
use crate::preprocessing::failures;
use crate::preprocessing::preprocessor::{Preprocessor, PreprocessorContext, PreprocessorRef};
use crate::preprocessing::registry;
use crate::preprocessing::{AnalysisGuard, WorkerEnvironment};
use crate::services::tracks::analysis_worker_count;
use crate::storage::StorageRoot;
use crate::topo;

#[derive(Clone, Debug)]
pub struct ImportEventContext {
    pub import_id: String,
    pub source: String,
    pub done: usize,
    pub total: usize,
}

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

/// Topo-sort `preprocessors` into layers via the shared [`topo::layers`].
/// A preprocessor's "parents" are the preprocessors that produce its input
/// artifacts. `Artifact::Audio` has no producer and is filtered out.
pub fn topo_layers(preprocessors: &[PreprocessorRef]) -> Layered {
    // Map each non-audio artifact to its sole producer.
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

    let parents_of = |p: &PreprocessorRef| -> Vec<&'static str> {
        p.inputs()
            .iter()
            .filter(|a| !matches!(a, Artifact::Audio))
            .map(|a| {
                *producer_of.get(a).unwrap_or_else(|| {
                    panic!(
                        "Preprocessor {} depends on artifact {:?} which has no producer",
                        p.name(),
                        a
                    )
                })
            })
            .collect()
    };

    let layers = topo::layers(preprocessors, |p| p.name(), parents_of)
        .into_iter()
        .map(|layer| layer.into_iter().cloned().collect())
        .collect();
    Layered { layers }
}

// -----------------------------------------------------------------------------
// In-flight dedup
// -----------------------------------------------------------------------------

/// Tracks (track_id, preprocessor_name) pairs currently executing so a
/// duplicate request awaits the in-flight one rather than racing it.
#[derive(Clone, Default)]
struct InflightSet {
    inner: Arc<std::sync::Mutex<HashMap<(String, &'static str), Arc<tokio::sync::Notify>>>>,
}

impl InflightSet {
    /// Try to claim execution. Returns an owned guard if the caller should
    /// run, `None` if another task is already running and the caller waited
    /// for it to finish.
    async fn claim(&self, track_id: &str, name: &'static str) -> Option<InflightClaim> {
        let key = (track_id.to_string(), name);
        loop {
            let notified = {
                let mut entries = self.inner.lock().unwrap();
                let Some(waiter) = entries.get(&key).cloned() else {
                    let notify = Arc::new(tokio::sync::Notify::new());
                    entries.insert(key.clone(), notify);
                    return Some(InflightClaim {
                        inner: Arc::clone(&self.inner),
                        key,
                    });
                };
                // Register while the map lock still excludes the claim's
                // Drop. `notify_waiters` does not retain a permit for future
                // waiters, so enabling later would lose a wakeup.
                let mut notified = Box::pin(waiter.notified_owned());
                notified.as_mut().enable();
                notified
            };
            notified.await;
            let entries = self.inner.lock().unwrap();
            if !entries.contains_key(&key) {
                return None;
            }
            drop(entries);
            // Otherwise loop and re-wait on the new in-flight run.
        }
    }
}

struct InflightClaim {
    inner: Arc<std::sync::Mutex<HashMap<(String, &'static str), Arc<tokio::sync::Notify>>>>,
    key: (String, &'static str),
}

impl Drop for InflightClaim {
    fn drop(&mut self) {
        let mut guard = self.inner.lock().unwrap();
        if let Some(notify) = guard.remove(&self.key) {
            notify.notify_waiters();
        }
    }
}

fn inflight() -> &'static InflightSet {
    static SET: OnceCell<InflightSet> = OnceCell::new();
    SET.get_or_init(InflightSet::default)
}

/// Drain every task in one DAG layer. A [`JoinError`] has lost the task's
/// `(name, output)` payload, so any such error makes the whole layer's outputs
/// indeterminate. Conservatively blocking all of them is the only safe basis
/// for deciding whether the next layer may run.
async fn drain_layer(
    set: &mut JoinSet<(&'static str, Artifact, Result<(), String>)>,
    layer_outputs: &[Artifact],
    track_id: &str,
) -> (HashSet<Artifact>, Vec<String>) {
    let mut failed_outputs = HashSet::new();
    let mut errors = Vec::new();
    let mut indeterminate = false;

    while let Some(joined) = set.join_next().await {
        match joined {
            Ok((_name, _output, Ok(()))) => {}
            Ok((name, output, Err(error))) => {
                eprintln!("[preprocessing] {name} failed for track {track_id}: {error}");
                sentry::capture_message(
                    &format!("Preprocessor {name} failed for track {track_id}: {error}"),
                    sentry::Level::Error,
                );
                failed_outputs.insert(output);
                errors.push(format!("{name}: {error}"));
            }
            Err(error) => {
                indeterminate = true;
                errors.push(format!("preprocessor task join error: {error}"));
            }
        }
    }

    if indeterminate {
        failed_outputs.extend(layer_outputs.iter().copied());
    }
    (failed_outputs, errors)
}

fn is_blocked(p: &dyn Preprocessor, failed_artifacts: &HashSet<Artifact>) -> bool {
    p.inputs()
        .iter()
        .any(|input| failed_artifacts.contains(input))
}

// -----------------------------------------------------------------------------
// Public API
// -----------------------------------------------------------------------------

/// Plan + execute for a single track. Layers run sequentially; within each
/// layer, siblings run concurrently. A failed preprocessor records its error
/// and skips its downstream preprocessors for this run.
pub async fn run_for_track(
    pool: &SqlitePool,
    storage: &StorageRoot,
    workers: &WorkerEnvironment,
    events: &Events,
    stem_cache: &StemCache,
    track_id: &str,
    preprocessors: &[PreprocessorRef],
    analysis: &AnalysisGuard,
    import: Option<&ImportEventContext>,
) -> Result<(), String> {
    analysis.checkpoint()?;
    let Some(track) = tracks_db::get_track_by_id(pool, track_id).await? else {
        // Track was deleted (typically by sync) between enqueue and now.
        // Silent skip — there's nothing to preprocess and no failure to record.
        return Ok(());
    };
    storage.ensure_track_storage()?;
    let stems_dir = storage.stems_root();

    let layered = topo_layers(preprocessors);

    // Track which artifacts failed in this run so we can skip dependents.
    let mut failed_artifacts: HashSet<Artifact> = HashSet::new();
    let mut track_errors = Vec::new();

    for layer in layered.layers() {
        // Filter layer: skip preprocessors whose inputs failed, or whose
        // output is already complete.
        let mut to_run: Vec<PreprocessorRef> = Vec::new();
        let ctx = PreprocessorContext::new(
            pool,
            storage,
            workers,
            events,
            stem_cache,
            &track,
            stems_dir.clone(),
            analysis.clone(),
        );
        for p in layer {
            if is_blocked(p.as_ref(), &failed_artifacts) {
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
        let layer_outputs: Vec<Artifact> = to_run.iter().map(|p| p.output()).collect();

        // Spawn one task per preprocessor in this layer. Each task takes its
        // own owned context so it can be `'static`.
        let mut set: JoinSet<(&'static str, Artifact, Result<(), String>)> = JoinSet::new();
        for p in to_run {
            let pool = pool.clone();
            let storage = storage.clone();
            let workers = workers.clone();
            let events = events.clone();
            let stem_cache = stem_cache.clone();
            let track = track.clone();
            let stems_dir = stems_dir.clone();
            let track_id_owned = track_id.to_string();
            let analysis = analysis.clone();
            let import = import.cloned();
            set.spawn(async move {
                let ctx = PreprocessorContext::new(
                    &pool,
                    &storage,
                    &workers,
                    &events,
                    &stem_cache,
                    &track,
                    stems_dir,
                    analysis,
                );
                let res = run_one(&ctx, &track_id_owned, p.as_ref(), import.as_ref()).await;
                (p.name(), p.output(), res)
            });
        }

        let (layer_failures, layer_errors) = drain_layer(&mut set, &layer_outputs, track_id).await;
        failed_artifacts.extend(layer_failures);
        track_errors.extend(layer_errors);
    }

    if track_errors.is_empty() {
        Ok(())
    } else {
        Err(track_errors.join("; "))
    }
}

/// Run a single preprocessor for a single track, with failure backoff,
/// status emission, and in-flight dedup.
async fn run_one(
    ctx: &PreprocessorContext<'_>,
    track_id: &str,
    p: &dyn Preprocessor,
    import: Option<&ImportEventContext>,
) -> Result<(), String> {
    ctx.checkpoint()?;
    let Some(_claim) = inflight().claim(track_id, p.name()).await else {
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
    };

    if let Some(import) = import {
        ctx.events().emit(
            "track-import-state",
            TrackImportProgress {
                import_id: import.import_id.clone(),
                source: import.source.clone(),
                phase: TrackImportPhase::Analyzing,
                done: import.done,
                total: import.total,
                track_id: Some(track_id.to_string()),
                current_track: None,
                step: Some(p.name().to_string()),
                error: None,
            },
        );
    }

    let started = std::time::Instant::now();
    let result = p.run(ctx, track_id).await;
    // The only stage-cost record the pipeline keeps. Emitted for failures too,
    // because a slow failure and a fast one are different problems.
    eprintln!(
        "[preprocessing] {} track={track_id} duration_ms={} {}",
        p.name(),
        started.elapsed().as_millis(),
        if result.is_ok() { "ok" } else { "failed" }
    );

    // Cancellation is a lifecycle outcome, not a failed analysis. Do not
    // publish status or exponential-backoff rows for a retired identity.
    if let Err(error) = ctx.checkpoint() {
        return Err(error);
    }

    match result {
        Ok(()) => {
            failures::clear(ctx.pool(), track_id, p.name()).await?;
            ctx.events().emit("track-status-changed", track_id);
            Ok(())
        }
        Err(err) => {
            if let Err(record_error) =
                failures::record(ctx.pool(), track_id, p.name(), p.version(), &err).await
            {
                return Err(format!(
                    "{err}; additionally failed to record the preprocessing failure: {record_error}"
                ));
            }
            if let Some(import) = import {
                ctx.events().emit(
                    "track-import-state",
                    TrackImportProgress {
                        import_id: import.import_id.clone(),
                        source: import.source.clone(),
                        phase: TrackImportPhase::Analyzing,
                        done: import.done,
                        total: import.total,
                        track_id: Some(track_id.to_string()),
                        current_track: None,
                        step: Some(p.name().to_string()),
                        error: Some(err.clone()),
                    },
                );
            }
            Err(err)
        }
    }
}

/// Multi-track entry point. Bounded parallelism via Semaphore; per-track
/// scheduling delegates to [`run_for_track`].
pub async fn run_for_tracks(
    pool: SqlitePool,
    storage: StorageRoot,
    workers: WorkerEnvironment,
    events: Events,
    stem_cache: StemCache,
    track_ids: Vec<String>,
    analysis: AnalysisGuard,
    import: Option<ImportEventContext>,
) {
    let analyzable_total = track_ids.len();
    let preprocessors = registry::registered_preprocessors();
    // Validate the DAG once up front — panics on cycle.
    let _ = topo_layers(&preprocessors);
    let max_parallel = analysis_worker_count();
    let semaphore = Arc::new(Semaphore::new(max_parallel));
    let completed = Arc::new(std::sync::atomic::AtomicUsize::new(
        import.as_ref().map_or(0, |import| import.done),
    ));

    let mut handles = Vec::with_capacity(analyzable_total);
    for track_id in track_ids {
        let pool = pool.clone();
        let storage = storage.clone();
        let workers = workers.clone();
        let events = events.clone();
        let stem_cache = stem_cache.clone();
        let preprocessors = preprocessors.clone();
        let sem = semaphore.clone();
        let completed = completed.clone();
        let analysis = analysis.clone();
        let mut import = import.clone();

        let task_track_id = track_id.clone();
        let handle = tokio::spawn(async move {
            let _permit = sem
                .acquire()
                .await
                .map_err(|error| format!("analysis semaphore closed: {error}"))?;
            let done = completed.load(std::sync::atomic::Ordering::Relaxed);
            if let Some(import) = &mut import {
                import.done = done;
                events.emit(
                    "track-import-state",
                    TrackImportProgress {
                        import_id: import.import_id.clone(),
                        source: import.source.clone(),
                        phase: TrackImportPhase::Analyzing,
                        done,
                        total: import.total,
                        track_id: Some(track_id.clone()),
                        current_track: None,
                        step: None,
                        error: None,
                    },
                );
            }
            let result = run_for_track(
                &pool,
                &storage,
                &workers,
                &events,
                &stem_cache,
                &track_id,
                &preprocessors,
                &analysis,
                import.as_ref(),
            )
            .await;
            if let Err(e) = &result {
                eprintln!("[preprocessing] track {track_id} failed: {e}");
                sentry::capture_message(
                    &format!("Preprocessing failed for track {track_id}: {e}"),
                    sentry::Level::Error,
                );
            }
            let done = completed.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            if let Some(import) = &import {
                events.emit(
                    "track-import-state",
                    TrackImportProgress {
                        import_id: import.import_id.clone(),
                        source: import.source.clone(),
                        phase: TrackImportPhase::Analyzing,
                        done,
                        total: import.total,
                        track_id: Some(track_id),
                        current_track: None,
                        step: None,
                        error: result.as_ref().err().cloned(),
                    },
                );
            }
            result
        });
        handles.push((task_track_id, handle));
    }

    let mut terminal_errors = Vec::new();
    for (track_id, handle) in handles {
        match handle.await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => terminal_errors.push(format!("{track_id}: {error}")),
            Err(error) => {
                terminal_errors.push(format!("{track_id}: analysis task failed: {error}"))
            }
        }
    }
    if let Some(import) = import {
        let done = completed.load(std::sync::atomic::Ordering::Relaxed);
        events.emit(
            "track-import-state",
            TrackImportProgress {
                import_id: import.import_id,
                source: import.source,
                phase: TrackImportPhase::Complete,
                done,
                total: import.total,
                track_id: None,
                current_track: None,
                step: None,
                error: (!terminal_errors.is_empty()).then(|| terminal_errors.join("\n")),
            },
        );
    }
    eprintln!(
        "[preprocessing] finished all {analyzable_total} tracks ({max_parallel} parallel workers)"
    );
}

/// Startup reconciliation: each preprocessor returns the IDs of tracks that
/// need it via one bulk SQL query (artifact missing/stale and not in failure
/// backoff). The union is queued for processing.
pub async fn reconcile_on_startup(
    pool: SqlitePool,
    storage: StorageRoot,
    workers: WorkerEnvironment,
    events: Events,
    stem_cache: StemCache,
    analysis: AnalysisGuard,
) -> Result<(), String> {
    analysis.checkpoint()?;
    let preprocessors = registry::registered_preprocessors();
    // Validate DAG (panics on cycle).
    let _ = topo_layers(&preprocessors);

    let mut needs: HashSet<String> = HashSet::new();
    for p in &preprocessors {
        for id in p.list_pending(&pool).await? {
            needs.insert(id);
        }
    }

    if needs.is_empty() {
        return Ok(());
    }

    let queued: Vec<String> = needs.into_iter().collect();
    eprintln!(
        "[preprocessing] {} tracks need preprocessing, queueing...",
        queued.len()
    );
    run_for_tracks(
        pool, storage, workers, events, stem_cache, queued, analysis, None,
    )
    .await;
    Ok(())
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

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
        fn artifact_table(&self) -> &'static str {
            "stub_artifact"
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
        // Expect three layers (mert now reads drums.ogg so it sits behind
        // stems instead of alongside it):
        //   layer 0 = depends only on Audio
        //             { beat_grid, stems }
        //   layer 1 = depends on Audio+Stems
        //             { mert (Audio + Stems), roots (Stems) }
        //   layer 2 = depends on Mert
        //             { n2n (Stems + Mert), classifier (BeatGrid + Mert) }
        assert_eq!(layered.layers().len(), 3);
        let layer0_names: HashSet<_> = layered.layers()[0].iter().map(|p| p.name()).collect();
        assert!(layer0_names.contains("beat_grid"));
        assert!(layer0_names.contains("stems"));
        assert_eq!(layer0_names.len(), 2);
        let layer1_names: HashSet<_> = layered.layers()[1].iter().map(|p| p.name()).collect();
        assert!(layer1_names.contains("mert"));
        assert!(layer1_names.contains("roots"));
        let layer2_names: HashSet<_> = layered.layers()[2].iter().map(|p| p.name()).collect();
        assert!(layer2_names.contains("n2n"));
        assert!(layer2_names.contains("classifier"));
    }

    #[tokio::test]
    async fn owned_inflight_claim_releases_on_early_error() {
        const PROCESSOR: &str = "inflight_release_regression";
        let track_id = uuid::Uuid::new_v4().to_string();

        async fn fail_after_claim(track_id: &str) -> Result<(), String> {
            let _claim = inflight()
                .claim(track_id, PROCESSOR)
                .await
                .expect("first caller owns the claim");
            Err("simulated persistence failure".into())
        }

        assert!(fail_after_claim(&track_id).await.is_err());
        let reclaimed = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            inflight().claim(&track_id, PROCESSOR),
        )
        .await
        .expect("claim was not wedged by the early return")
        .expect("claim is available again");
        drop(reclaimed);
    }

    #[test]
    #[should_panic(expected = "Cycle")]
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
    async fn inflight_dedup_coalesces_concurrent_claims() {
        let set: Arc<InflightSet> = Arc::new(InflightSet::default());
        let claim1 = set
            .claim("trackA", "beat_grid")
            .await
            .expect("first claim should succeed");
        let set2 = set.clone();
        let task = tokio::spawn(async move { set2.claim("trackA", "beat_grid").await });
        // Yield so the second claim begins waiting.
        tokio::task::yield_now().await;
        // Release the first claim — second should observe completion and return None.
        drop(claim1);
        let claim2 = task.await.unwrap();
        assert!(claim2.is_none(), "second concurrent claim should coalesce");
    }

    #[tokio::test]
    async fn layer_drain_aggregates_every_panic_and_failure_and_blocks_dependents() {
        let layer_outputs = [Artifact::BeatGrid, Artifact::Stems, Artifact::Genre];
        let mut set: JoinSet<(&'static str, Artifact, Result<(), String>)> = JoinSet::new();
        set.spawn(async {
            panic!("first layer panic");
        });
        set.spawn(async {
            panic!("second layer panic");
        });
        set.spawn(async {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            (
                "ordinary_failure",
                Artifact::Genre,
                Err("ordinary error".to_string()),
            )
        });

        let (failed_outputs, errors) = drain_layer(&mut set, &layer_outputs, "panic-fixture").await;

        assert_eq!(errors.len(), 3, "the layer did not drain every task");
        assert_eq!(
            errors
                .iter()
                .filter(|error| error.contains("join error"))
                .count(),
            2,
            "both panics must survive aggregation"
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("ordinary_failure: ordinary error")),
            "the non-panicking sibling failure was lost"
        );
        assert_eq!(
            failed_outputs,
            layer_outputs.into_iter().collect(),
            "an unidentified panic must make every layer output indeterminate"
        );
        let downstream = StubProc {
            name: "downstream",
            version: 1,
            inputs: &[Artifact::BeatGrid, Artifact::Stems],
            output: Artifact::Roots,
        };
        assert!(
            is_blocked(&downstream, &failed_outputs),
            "downstream work was not suppressed after an indeterminate layer"
        );
    }
}
