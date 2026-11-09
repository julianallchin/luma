//! Topological scheduler + parallel dispatcher for the preprocessing DAG.
//!
//! Responsibilities:
//! 1. Run the layers of [`registry::dag`] in order — preprocessors in the
//!    same layer have no dependencies on each other and can run in parallel
//!    for a given track.
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
//! On preprocessor failure: log, record a row in
//! `preprocessing_failures` with exponential backoff, skip downstream
//! preprocessors for this track in this run, then continue with other tracks.
//! Until the track's `next_retry_at` has elapsed, the step stays skipped on
//! that track, with its dependents: reconcile does not queue the track for it,
//! and a run queued for another step passes it by. A run in which a step
//! rewrote one of its inputs tries it anyway, and reconcile forgets the
//! failures of a version that is no longer running, and those recorded before
//! the Python environment was last installed.
//!
//! A step that fails the same way on several tracks in a row is broken itself,
//! not the tracks: the rest of the batch skips it ([`Breaker`]).

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use once_cell::sync::OnceCell;
use sqlx::SqlitePool;
use tokio::task::JoinSet;

use crate::database::local::tracks as tracks_db;
use crate::events::{Event, Events};
use crate::models::tracks::{
    StageOutcome, TrackAnalysisEvent, TrackImportPhase, TrackImportProgress,
};
use crate::preprocessing::artifact::Artifact;
use crate::preprocessing::concurrency::{analysis_worker_count, run_bounded_track_jobs};
use crate::preprocessing::failures;
use crate::preprocessing::preprocessor::{
    writes_synced_rows, Preprocessor, PreprocessorContext, PreprocessorRef, AUTHORS_SYNCED_ANALYSIS,
};
use crate::preprocessing::registry;
use crate::preprocessing::{AnalysisGuard, WorkerEnvironment};
use crate::storage::StorageRoot;

#[derive(Clone, Debug)]
pub struct ImportEventContext {
    pub import_id: String,
    pub source: String,
    pub done: usize,
    pub total: usize,
}

impl ImportEventContext {
    fn emit(&self, events: &Events, progress: TrackImportProgress) {
        events.emit(Event::TrackImportState(progress));
    }

    fn progress(&self, phase: TrackImportPhase) -> TrackImportProgress {
        TrackImportProgress {
            import_id: self.import_id.clone(),
            source: self.source.clone(),
            phase,
            done: self.done,
            total: self.total,
            track_id: None,
            current_track: None,
            analysis: None,
            error: None,
        }
    }

    /// The import's last word: every track's analysis has ended, both the
    /// preprocessing DAG and the waveform branch beside it.
    pub fn complete(mut self, events: &Events, summary: AnalysisSummary) {
        self.done = summary.done;
        let mut progress = self.progress(TrackImportPhase::Complete);
        progress.error = (!summary.errors.is_empty()).then(|| summary.errors.join("\n"));
        self.emit(events, progress);
    }
}

/// What [`run_for_tracks`] leaves for the caller's terminal event.
#[derive(Debug, Default)]
pub struct AnalysisSummary {
    /// The import's `done` once the batch has run: requested items whose
    /// analysis attempt finished.
    pub done: usize,
    pub errors: Vec<String>,
}

/// One track's analysis stages under an import, as the events that announce
/// them.
///
/// The owner of the [`TrackAnalysisEvent`] contract: a stage settles at most
/// once whoever reports it, and [`Self::settle_rest`] settles whatever is left
/// when a track's run ends, so "every queued stage settles" holds on every
/// path out of the scheduler rather than on the ones that remembered to say so.
#[derive(Clone)]
pub struct StageReport {
    import: ImportEventContext,
    events: Events,
    track_id: String,
    settled: Arc<std::sync::Mutex<HashSet<String>>>,
}

impl StageReport {
    pub fn new(import: ImportEventContext, events: Events, track_id: impl Into<String>) -> Self {
        Self {
            import,
            events,
            track_id: track_id.into(),
            settled: Arc::default(),
        }
    }

    fn emit(&self, analysis: TrackAnalysisEvent) {
        let mut progress = self.import.progress(TrackImportPhase::Analyzing);
        progress.track_id = Some(self.track_id.clone());
        progress.analysis = Some(analysis);
        self.import.emit(&self.events, progress);
    }

    /// Announce every stage this track will settle.
    pub fn queued(&self, stages: Vec<String>) {
        self.emit(TrackAnalysisEvent::Queued { stages });
    }

    pub fn started(&self, stage: &str) {
        self.emit(TrackAnalysisEvent::StageStarted {
            stage: stage.to_string(),
        });
    }

    pub fn settle(&self, stage: &str, outcome: StageOutcome) {
        let first = self
            .settled
            .lock()
            .expect("stage report poisoned")
            .insert(stage.to_string());
        if first {
            self.emit(TrackAnalysisEvent::StageSettled {
                stage: stage.to_string(),
                outcome,
            });
        }
    }

    fn settle_rest<'a>(&self, stages: impl IntoIterator<Item = &'a str>, outcome: StageOutcome) {
        for stage in stages {
            self.settle(stage, outcome.clone());
        }
    }
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

// -----------------------------------------------------------------------------
// Circuit breaker
// -----------------------------------------------------------------------------

/// How many tracks in a row a step fails on the same way before the rest of
/// its batch skips it.
const SAME_FAILURES_TO_PAUSE: usize = 3;

/// One batch's circuit breaker.
///
/// A step that fails with the same error on track after track is broken
/// itself, in its code or its Python environment, and will fail that way on
/// every track left. Per-track backoff alone would still start a worker for
/// each of them, and for each again at every launch until the backoff outgrew
/// the gap between launches. After [`SAME_FAILURES_TO_PAUSE`] such failures in
/// a row, the rest of the batch skips the step, with its dependents. A skipped
/// track records no failure, so the next batch (the next launch's reconcile)
/// tries the step again, on that many tracks at most if it is still broken.
#[derive(Clone, Default)]
pub(crate) struct Breaker(Arc<Mutex<HashMap<&'static str, Streak>>>);

/// A step's run of identical failures: the error's first line, and on how many
/// tracks in a row it failed with it.
struct Streak {
    headline: String,
    tracks: usize,
}

impl Breaker {
    /// Why the rest of the batch skips `step`, once it does.
    fn paused(&self, step: &str) -> Option<String> {
        let streaks = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let streak = streaks
            .get(step)
            .filter(|streak| streak.tracks >= SAME_FAILURES_TO_PAUSE)?;
        Some(format!(
            "{} (not run: {step} failed this way on {} tracks in a row)",
            streak.headline, streak.tracks
        ))
    }

    fn succeeded(&self, step: &str) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(step);
    }

    fn failed(&self, step: &'static str, headline: &str) {
        let mut streaks = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let streak = streaks.entry(step).or_insert_with(|| Streak {
            headline: headline.to_owned(),
            tracks: 0,
        });
        if streak.headline != headline {
            streak.headline = headline.to_owned();
            streak.tracks = 0;
        }
        streak.tracks += 1;
    }
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

/// Whether `p` reads any of `artifacts`.
fn reads_any(p: &dyn Preprocessor, artifacts: &HashSet<Artifact>) -> bool {
    p.inputs().iter().any(|input| artifacts.contains(input))
}

// -----------------------------------------------------------------------------
// Public API
// -----------------------------------------------------------------------------

/// Plan + execute for a single track, one of the batch whose steps share
/// `breaker`. Layers run sequentially; within each layer, siblings run
/// concurrently. A failed preprocessor records its error and skips its
/// downstream preprocessors for this run.
///
/// Under an import, every preprocessor settles on `report` before this
/// returns — the ones a path out of here never reached included — unless the
/// run was cancelled, which publishes nothing.
pub async fn run_for_track(
    pool: &SqlitePool,
    storage: &StorageRoot,
    workers: &WorkerEnvironment,
    track_id: &str,
    analysis: &AnalysisGuard,
    report: Option<&StageReport>,
    breaker: &Breaker,
) -> Result<(), String> {
    let result =
        run_track_layers(pool, storage, workers, track_id, analysis, report, breaker).await;
    if let Some(report) = report.filter(|_| !analysis.is_cancelled()) {
        report.settle_rest(
            registry::dag().preprocessors().map(|p| p.name()),
            match &result {
                Ok(()) => StageOutcome::Skipped,
                Err(error) => StageOutcome::Failed(error.clone()),
            },
        );
    }
    result
}

async fn run_track_layers(
    pool: &SqlitePool,
    storage: &StorageRoot,
    workers: &WorkerEnvironment,
    track_id: &str,
    analysis: &AnalysisGuard,
    report: Option<&StageReport>,
    breaker: &Breaker,
) -> Result<(), String> {
    analysis.checkpoint()?;
    let Some(track) = tracks_db::get_track_by_id(pool, track_id).await? else {
        // Track was deleted (typically by sync) between enqueue and now.
        // Silent skip — there's nothing to preprocess and no failure to record.
        return Ok(());
    };
    storage.ensure_track_storage()?;
    let stems_dir = storage.stems_root();
    let authors_synced: bool = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT {AUTHORS_SYNCED_ANALYSIS} FROM tracks t WHERE t.id = ?"
    )))
    .bind(track_id)
    .fetch_one(pool)
    .await
    .map_err(|error| format!("Failed to read who analyses track {track_id}: {error}"))?;

    // Artifacts that failed in this run, whose step is in failure backoff, or
    // that are another principal's to produce and have not synced yet, so
    // their dependents wait.
    let mut failed_artifacts: HashSet<Artifact> = HashSet::new();
    // Artifacts a step wrote in this run. A step that failed on the old ones
    // gets a new try on them, backoff or not.
    let mut produced: HashSet<Artifact> = HashSet::new();
    let mut track_errors = Vec::new();

    for layer in registry::dag().layers() {
        // Read per layer, not per run: a concurrent run of this track may
        // have failed a later step while this one worked on the earlier ones.
        let held = failures::held_back(pool, track_id).await?;
        // Filter layer: skip preprocessors whose inputs failed, or whose
        // output is already complete.
        let mut to_run: Vec<PreprocessorRef> = Vec::new();
        let ctx = PreprocessorContext::new(
            pool,
            storage,
            workers,
            &track,
            stems_dir.clone(),
            analysis.clone(),
        );
        for p in layer {
            if reads_any(p.as_ref(), &failed_artifacts) {
                eprintln!(
                    "[preprocessing] skipping {} for track {track_id}: upstream failed",
                    p.name()
                );
                failed_artifacts.insert(p.output());
                if let Some(report) = report {
                    report.settle(p.name(), StageOutcome::Skipped);
                }
                continue;
            }
            if p.is_complete(&ctx, track_id).await? {
                if let Some(report) = report {
                    report.settle(p.name(), StageOutcome::Skipped);
                }
                continue;
            }
            if !authors_synced && writes_synced_rows(p.as_ref()) {
                failed_artifacts.insert(p.output());
                if let Some(report) = report {
                    report.settle(p.name(), StageOutcome::Skipped);
                }
                continue;
            }
            // A step that failed here recently waits out its backoff even when
            // the track is queued for another one. Its dependents wait with it,
            // and they are what keeps a track queued: nothing else stops a
            // failing upstream step from rerunning on every launch.
            let fresh_input = reads_any(p.as_ref(), &produced);
            if let Some(error) = held.get(p.name()).filter(|_| !fresh_input) {
                // A cancelled run reports nothing; see `run_one`.
                analysis.checkpoint()?;
                eprintln!(
                    "[preprocessing] skipping {} for track {track_id}: in failure backoff",
                    p.name()
                );
                failed_artifacts.insert(p.output());
                let reason = failures::headline(error);
                if let Some(report) = report {
                    report.settle(p.name(), StageOutcome::Failed(reason.to_owned()));
                }
                track_errors.push(format!("{}: {reason}", p.name()));
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
            let track = track.clone();
            let stems_dir = stems_dir.clone();
            let track_id_owned = track_id.to_string();
            let analysis = analysis.clone();
            let report = report.cloned();
            let breaker = breaker.clone();
            set.spawn(async move {
                let ctx = PreprocessorContext::new(
                    &pool, &storage, &workers, &track, stems_dir, analysis,
                );
                let res =
                    run_one(&ctx, &track_id_owned, p.as_ref(), report.as_ref(), &breaker).await;
                (p.name(), p.output(), res)
            });
        }

        let (layer_failures, layer_errors) = drain_layer(&mut set, &layer_outputs, track_id).await;
        produced.extend(
            layer_outputs
                .iter()
                .filter(|output| !layer_failures.contains(output)),
        );
        failed_artifacts.extend(layer_failures);
        track_errors.extend(layer_errors);
    }

    if track_errors.is_empty() {
        Ok(())
    } else {
        Err(track_errors.join("; "))
    }
}

/// Run a single preprocessor for a single track, with failure recording,
/// status emission, and in-flight dedup. The caller skips a step in failure
/// backoff before it gets here; a step `breaker` has paused stops here.
async fn run_one(
    ctx: &PreprocessorContext<'_>,
    track_id: &str,
    p: &dyn Preprocessor,
    report: Option<&StageReport>,
    breaker: &Breaker,
) -> Result<(), String> {
    ctx.checkpoint()?;
    let settle = |outcome: StageOutcome| {
        if let Some(report) = report {
            report.settle(p.name(), outcome);
        }
    };
    let Some(_claim) = inflight().claim(track_id, p.name()).await else {
        // Another task ran it; if it succeeded, we're done. If it failed
        // we'll see that on the next is_complete check below.
        return if p.is_complete(ctx, track_id).await? {
            settle(StageOutcome::Done);
            Ok(())
        } else {
            let error = format!(
                "Concurrent {} run for track {track_id} did not complete",
                p.name()
            );
            settle(StageOutcome::Failed(error.clone()));
            Err(error)
        };
    };

    // Every stage runs a Python worker, so wait for the host's setup first. A
    // stage waiting on it is still queued, and the wait is no part of its cost.
    if let Err(error) = ctx.python_ready().await {
        // A cancelled wait is a lifecycle outcome; see below.
        ctx.checkpoint()?;
        // The environment failed, not this track: report it, but record no
        // backoff, so the next launch with a working setup retries at once.
        settle(StageOutcome::Failed(error.clone()));
        return Err(error);
    }

    // Checked last, so a track that waited for setup or for a concurrent run
    // sees a pause that came meanwhile. It records no failure: the track did
    // nothing wrong, and the next batch tries it.
    if let Some(reason) = breaker.paused(p.name()) {
        settle(StageOutcome::Failed(reason.clone()));
        return Err(reason);
    }

    if let Some(report) = report {
        report.started(p.name());
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
            breaker.succeeded(p.name());
            failures::clear(ctx.pool(), track_id, p.name()).await?;
            settle(StageOutcome::Done);
            Ok(())
        }
        Err(err) => {
            // The record keeps the worker's whole report, traceback included.
            // The stage's outcome and the track's error carry its first line,
            // which is what the import list and the import's error show.
            let reason = failures::headline(&err).to_owned();
            breaker.failed(p.name(), &reason);
            if let Some((_, report)) = err.split_once('\n') {
                eprintln!(
                    "[preprocessing] {} for track {track_id} reported:\n{}",
                    p.name(),
                    report.trim_start()
                );
            }
            if let Err(record_error) =
                failures::record(ctx.pool(), track_id, p.name(), p.version(), &err).await
            {
                return Err(format!(
                    "{reason}; additionally failed to record the preprocessing failure: {record_error}"
                ));
            }
            settle(StageOutcome::Failed(reason.clone()));
            Err(reason)
        }
    }
}

/// Multi-track entry point: per-track scheduling delegates to
/// [`run_for_track`], at most [`analysis_worker_count`] tracks at a time. The
/// tracks are one batch: a step that fails the same way on several of them in
/// a row is skipped on the rest ([`Breaker`]).
///
/// Under an import each track reports its stages as it runs; the terminal
/// event is the caller's, because the DAG is not the only branch an import
/// waits on — see [`ImportEventContext::complete`].
pub async fn run_for_tracks(
    pool: SqlitePool,
    storage: StorageRoot,
    workers: WorkerEnvironment,
    events: Events,
    track_ids: Vec<String>,
    analysis: AnalysisGuard,
    import: Option<ImportEventContext>,
) -> AnalysisSummary {
    let max_parallel = analysis_worker_count();
    let completed = Arc::new(AtomicUsize::new(
        import.as_ref().map_or(0, |import| import.done),
    ));
    let errors = Arc::new(Mutex::new(Vec::new()));
    let breaker = Breaker::default();

    run_bounded_track_jobs(&track_ids, &analysis, max_parallel, |track_id| {
        let (pool, storage, workers, events) = (
            pool.clone(),
            storage.clone(),
            workers.clone(),
            events.clone(),
        );
        let (completed, errors, analysis, import, breaker) = (
            completed.clone(),
            errors.clone(),
            analysis.clone(),
            import.clone(),
            breaker.clone(),
        );
        async move {
            let report = import.map(|mut import| {
                import.done = completed.load(Ordering::Relaxed);
                StageReport::new(import, events, track_id.clone())
            });
            let result = run_for_track(
                &pool,
                &storage,
                &workers,
                &track_id,
                &analysis,
                report.as_ref(),
                &breaker,
            )
            .await;
            if let Err(e) = result {
                eprintln!("[preprocessing] track {track_id} failed: {e}");
                errors
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(format!("{track_id}: {e}"));
            }
            completed.fetch_add(1, Ordering::Relaxed);
        }
    })
    .await;

    eprintln!(
        "[preprocessing] finished all {} tracks ({max_parallel} parallel workers)",
        track_ids.len()
    );
    let errors = std::mem::take(&mut *errors.lock().unwrap_or_else(PoisonError::into_inner));
    AnalysisSummary {
        done: completed.load(Ordering::Relaxed),
        errors,
    }
}

/// Startup reconciliation: forget the failures recorded before the Python
/// environment was last installed; each preprocessor then forgets the failures
/// other versions of it recorded, and returns the IDs of tracks that need it via
/// one bulk SQL query (artifact missing/stale and not in failure backoff). The
/// union is queued for processing.
pub async fn reconcile_on_startup(
    pool: SqlitePool,
    storage: StorageRoot,
    workers: WorkerEnvironment,
    events: Events,
    analysis: AnalysisGuard,
) -> Result<(), String> {
    analysis.checkpoint()?;
    if let Some(installed) = workers.environment_installed_at() {
        failures::forget_before(&pool, installed).await?;
    }
    let mut needs: HashSet<String> = HashSet::new();
    for p in registry::dag().preprocessors() {
        failures::forget_other_versions(&pool, p.name(), p.version()).await?;
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
    run_for_tracks(pool, storage, workers, events, queued, analysis, None).await;
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

    /// What the failure table says, step by step: a step that ran again would
    /// have cleared its row or counted another attempt.
    async fn failure_rows(pool: &SqlitePool) -> Vec<(String, i64)> {
        sqlx::query_as(
            "SELECT preprocessor, attempts FROM preprocessing_failures ORDER BY preprocessor",
        )
        .fetch_all(pool)
        .await
        .unwrap()
    }

    /// The stages a track's run settled, in order.
    #[derive(Clone, Default)]
    struct Settled(Arc<std::sync::Mutex<Vec<(String, StageOutcome)>>>);

    impl crate::events::EventSink for Settled {
        fn emit(&self, event: Event) {
            if let Event::TrackImportState(TrackImportProgress {
                analysis: Some(TrackAnalysisEvent::StageSettled { stage, outcome }),
                ..
            }) = event
            {
                self.0.lock().unwrap().push((stage, outcome));
            }
        }
    }

    /// A library holding track `t1`, with audio and no analysis.
    async fn library(directory: &std::path::Path) -> crate::dispatch::AppServices {
        let services = crate::test_support::signed_out(directory, directory.to_path_buf()).await;
        let audio = directory.join("song.wav");
        std::fs::write(&audio, b"RIFF").unwrap();
        sqlx::query(
            "INSERT INTO tracks (id, track_hash, title, duration_seconds, file_path)
             VALUES ('t1', 'hash', 'Song', 1.0, ?)",
        )
        .bind(audio.to_string_lossy())
        .execute(&services.db.0)
        .await
        .unwrap();
        services
    }

    /// Put `held` into failure backoff on `t1`.
    async fn hold(pool: &SqlitePool, held: &[&str]) {
        for name in held {
            let p = registry::dag()
                .preprocessors()
                .find(|p| p.name() == *name)
                .unwrap();
            let error = format!("{name} worker failed: it broke\n\nTraceback …");
            failures::record(pool, "t1", name, p.version(), &error)
                .await
                .unwrap();
        }
    }

    /// Whether reconcile would queue `t1`, for any step.
    async fn queued(pool: &SqlitePool) -> bool {
        for p in registry::dag().preprocessors() {
            if p.list_pending(pool)
                .await
                .unwrap()
                .iter()
                .any(|id| id == "t1")
            {
                return true;
            }
        }
        false
    }

    /// Run `t1` as an import does, and check that no step ran: the failure
    /// table is as it was. Every `held` step reports the failure that holds
    /// it, by its first line, and every other step settles as skipped.
    async fn assert_runs_nothing(services: &crate::dispatch::AppServices, held: &[&str]) {
        let pool = &services.db.0;
        let before = failure_rows(pool).await;
        let settled = Settled::default();
        let import = ImportEventContext {
            import_id: "import".into(),
            source: "test".into(),
            done: 0,
            total: 1,
        };
        let report = StageReport::new(import, Events::new(settled.clone()), "t1");
        let epoch = services.analysis_tasks.current_epoch().unwrap();
        let lease = services.analysis_tasks.lease(epoch).unwrap();
        let run = run_for_track(
            pool,
            &services.storage,
            &services.workers,
            "t1",
            &lease.guard(),
            Some(&report),
            &Breaker::default(),
        )
        .await;

        assert_eq!(
            failure_rows(pool).await,
            before,
            "a step in backoff, or one waiting on it, ran"
        );
        let error = run.expect_err("a held step is the track's failure");
        for name in held {
            assert!(
                error.contains(&format!("{name}: {name} worker failed: it broke")),
                "{name} is missing from the track's error: {error}"
            );
        }
        let settled = settled.0.lock().unwrap();
        for (stage, outcome) in settled.iter() {
            let expected = if held.contains(&stage.as_str()) {
                StageOutcome::Failed(format!("{stage} worker failed: it broke"))
            } else {
                StageOutcome::Skipped
            };
            assert_eq!(outcome, &expected, "{stage}");
        }
        assert_eq!(
            settled.len(),
            registry::dag().preprocessors().count(),
            "a stage never settled"
        );
    }

    /// Every step waits on the two the DAG starts from.
    #[tokio::test]
    async fn a_queued_track_reruns_no_step_in_failure_backoff() {
        let directory = tempfile::tempdir().unwrap();
        let services = library(directory.path()).await;
        let first: Vec<_> = registry::dag().layers()[0]
            .iter()
            .map(|p| p.name())
            .collect();
        hold(&services.db.0, &first).await;
        assert!(
            queued(&services.db.0).await,
            "reconcile would not queue the track at all"
        );

        assert_runs_nothing(&services, &first).await;
    }

    /// The case that reran a broken MERT on every track at every launch:
    /// beats and stems are done, MERT is held, and the steps waiting on it
    /// keep the track queued. The chords and genre steps, which would run
    /// next to MERT, are held too, so a step that ran shows in the table.
    #[tokio::test]
    async fn a_held_step_after_complete_inputs_stays_held() {
        let directory = tempfile::tempdir().unwrap();
        let services = library(directory.path()).await;
        let pool = &services.db.0;
        let version = |name: &str| {
            registry::dag()
                .preprocessors()
                .find(|p| p.name() == name)
                .unwrap()
                .version()
        };
        sqlx::query(
            "INSERT INTO track_beats
                (track_id, beats_json, downbeats_json, bpm, beats_per_bar, processor_version)
             VALUES ('t1', '[0.5,1.0,1.5,2.0]', '[0.5,2.5]', 128.0, 4, ?)",
        )
        .bind(i64::from(version("beat_grid")))
        .execute(pool)
        .await
        .unwrap();
        let stems = services.storage.stems_root().join("hash");
        std::fs::create_dir_all(&stems).unwrap();
        for stem in ["bass", "drums", "other", "vocals"] {
            let file = stems.join(format!("{stem}.ogg"));
            std::fs::write(&file, b"OggS").unwrap();
            sqlx::query(
                "INSERT INTO track_stems (track_id, stem_name, file_path, processor_version)
                 VALUES ('t1', ?, ?, ?)",
            )
            .bind(stem)
            .bind(file.to_string_lossy())
            .bind(i64::from(version("stems")))
            .execute(pool)
            .await
            .unwrap();
        }
        let held = registry::dag().layers()[1]
            .iter()
            .map(|p| p.name())
            .collect::<Vec<_>>();
        assert!(held.contains(&"mert"), "MERT moved out of the second layer");
        hold(pool, &held).await;
        assert!(queued(pool).await, "reconcile would not queue the track");

        assert_runs_nothing(&services, &held).await;
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
            reads_any(&downstream, &failed_outputs),
            "downstream work was not suppressed after an indeterminate layer"
        );
    }

    /// A step that fails on every track with the same error, as one does when
    /// its Python environment is broken.
    #[derive(Default)]
    struct Broken {
        runs: AtomicUsize,
    }

    #[async_trait]
    impl Preprocessor for Broken {
        fn name(&self) -> &'static str {
            "broken_step"
        }
        fn version(&self) -> u32 {
            1
        }
        fn inputs(&self) -> &'static [Artifact] {
            &[Artifact::Audio]
        }
        fn output(&self) -> Artifact {
            Artifact::Genre
        }
        fn artifact_table(&self) -> &'static str {
            "track_genres"
        }
        async fn run(&self, _ctx: &PreprocessorContext<'_>, _track_id: &str) -> Result<(), String> {
            self.runs.fetch_add(1, Ordering::Relaxed);
            Err("Broken worker failed: No module named 'torch'\n\nTraceback …".into())
        }
    }

    /// Once a step has failed the same way on enough tracks in a row, the
    /// rest of the batch doesn't start its worker. The tracks it skips record
    /// no failure, so the next batch tries them, and report the failure that
    /// paused it.
    #[tokio::test]
    async fn a_step_failing_the_same_way_track_after_track_is_paused() {
        let directory = tempfile::tempdir().unwrap();
        let services = library(directory.path()).await;
        let pool = &services.db.0;
        let tracks: Vec<String> = (1..=SAME_FAILURES_TO_PAUSE + 2)
            .map(|n| format!("t{n}"))
            .collect();
        for id in &tracks[1..] {
            sqlx::query(
                "INSERT INTO tracks (id, track_hash, title, duration_seconds, file_path)
                 VALUES (?, ?, 'Song', 1.0, '/audio/song.wav')",
            )
            .bind(id)
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
        }
        let step = Broken::default();
        let batch = Breaker::default();
        let epoch = services.analysis_tasks.current_epoch().unwrap();
        let lease = services.analysis_tasks.lease(epoch).unwrap();

        let mut errors = Vec::new();
        for id in &tracks {
            let track = tracks_db::get_track_by_id(pool, id).await.unwrap().unwrap();
            let ctx = PreprocessorContext::new(
                pool,
                &services.storage,
                &services.workers,
                &track,
                services.storage.stems_root(),
                lease.guard(),
            );
            errors.push(run_one(&ctx, id, &step, None, &batch).await.unwrap_err());
        }

        assert_eq!(
            step.runs.load(Ordering::Relaxed),
            SAME_FAILURES_TO_PAUSE,
            "the step ran on after it had failed the same way enough times"
        );
        let recorded: Vec<String> =
            sqlx::query_scalar("SELECT track_id FROM preprocessing_failures ORDER BY track_id")
                .fetch_all(pool)
                .await
                .unwrap();
        assert_eq!(recorded, tracks[..SAME_FAILURES_TO_PAUSE]);
        for error in &errors[SAME_FAILURES_TO_PAUSE..] {
            assert!(
                error.starts_with("Broken worker failed: No module named 'torch'"),
                "a skipped track hides why: {error}"
            );
        }
    }

    /// Only an unbroken run of one failure pauses a step: a success, or a
    /// different failure, starts the count again, and each step counts its
    /// own.
    #[test]
    fn a_success_or_another_failure_starts_the_count_again() {
        let batch = Breaker::default();
        let one_short = |step: &'static str, headline: &str| {
            for _ in 1..SAME_FAILURES_TO_PAUSE {
                batch.failed(step, headline);
            }
        };
        one_short("step", "the same");
        batch.succeeded("step");
        one_short("step", "the same");
        assert!(
            batch.paused("step").is_none(),
            "a success did not start the count again"
        );
        batch.failed("step", "another");
        one_short("step", "the same");
        assert!(
            batch.paused("step").is_none(),
            "another failure did not start the count again"
        );

        one_short("other_step", "the same");
        batch.failed("other_step", "the same");
        assert!(batch.paused("other_step").is_some());
        assert!(
            batch.paused("step").is_none(),
            "a step was paused for another's failures"
        );
    }
}
