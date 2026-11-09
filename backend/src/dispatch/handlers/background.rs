//! The background work the status bar reads: one cheap snapshot of the
//! compute in flight, off in-memory state and one indexed read of the failure
//! table.

use std::collections::HashMap;

use crate::dispatch::AppServices;
use crate::error::CommandError;
use crate::models::background::{step_doing, BackgroundJob, BackgroundWork, JobKind, JobState};
use crate::preprocessing::failures;
use crate::runtime::python::PythonSetup;

/// What the app is computing in the background right now: Python setup and its
/// model fetch, each track's analysis, and the steps held back after failures.
/// Moving records and media is [`super::sync::sync_status`]'s.
pub async fn background_work(services: &AppServices) -> Result<BackgroundWork, CommandError> {
    let mut jobs = Vec::new();

    let setup = services.workers.setup();
    let setting_up = setup.is_some_and(|setup| setup.state() == PythonSetup::Running);
    if let Some(setup) = setup {
        match setup.state() {
            PythonSetup::Running => jobs.push(job(
                JobKind::PythonSetup,
                "Setting up Python",
                JobState::Running,
            )),
            PythonSetup::Failed(step) => jobs.push(job(
                JobKind::PythonSetup,
                "Python setup failed",
                JobState::Failed(step),
            )),
            PythonSetup::Ready if setup.preloading() => {
                jobs.push(job(JobKind::Models, "Fetching models", JobState::Running));
            }
            PythonSetup::Ready => {}
        }
    }

    let activity = services.analysis_tasks.activity().snapshot();
    let held = failures::all_held_back(&services.db.0).await?;
    let names = track_names(
        services,
        activity
            .tracks
            .iter()
            .map(|track| track.track_id.as_str())
            .chain(held.iter().map(|step| step.track_id.as_str())),
    )
    .await?;
    let name = |track_id: &str| names.get(track_id).cloned();

    let mut tracks: Vec<BackgroundJob> = activity
        .tracks
        .iter()
        .map(|track| {
            let (what, state) = match track.running.as_slice() {
                [] if setting_up => ("Waiting for Python setup".to_owned(), JobState::Queued),
                [] => ("Queued".to_owned(), JobState::Queued),
                running => (doing_all(running), JobState::Running),
            };
            BackgroundJob {
                track_id: Some(track.track_id.clone()),
                track: name(&track.track_id),
                steps: track
                    .running
                    .iter()
                    .map(|step| (*step).to_owned())
                    .collect(),
                ..job(JobKind::Analysis, &what, state)
            }
        })
        .collect();
    // Running first, then queued; each by name.
    tracks.sort_by(|a, b| {
        (a.state != JobState::Running, &a.track).cmp(&(b.state != JobState::Running, &b.track))
    });
    jobs.extend(tracks);

    jobs.extend(
        activity
            .paused
            .into_iter()
            .map(|(step, reason)| BackgroundJob {
                steps: vec![step.to_owned()],
                ..job(
                    JobKind::Analysis,
                    &format!("{} paused", step_doing(step)),
                    JobState::Paused(reason),
                )
            }),
    );
    jobs.extend(held.into_iter().map(|step| {
        BackgroundJob {
            track: name(&step.track_id),
            track_id: Some(step.track_id),
            ..job(
                JobKind::Analysis,
                &format!("{} paused", step_doing(&step.preprocessor)),
                JobState::Paused(failures::headline(&step.last_error).to_owned()),
            )
            .with_step(step.preprocessor)
        }
    }));
    Ok(BackgroundWork { jobs })
}

fn job(kind: JobKind, what: &str, state: JobState) -> BackgroundJob {
    BackgroundJob {
        kind,
        what: what.to_owned(),
        track_id: None,
        track: None,
        steps: Vec::new(),
        state,
    }
}

impl BackgroundJob {
    fn with_step(mut self, step: String) -> Self {
        self.steps.push(step);
        self
    }
}

/// Several steps running at once, as one phrase: `Finding beats, separating
/// stems`.
fn doing_all(steps: &[&str]) -> String {
    steps
        .iter()
        .enumerate()
        .map(|(index, step)| {
            let doing = step_doing(step);
            if index == 0 {
                doing
            } else {
                doing.to_lowercase()
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Each track's name as the library shows it: its title, or its file's name.
async fn track_names<'a>(
    services: &AppServices,
    ids: impl Iterator<Item = &'a str>,
) -> Result<HashMap<String, String>, CommandError> {
    let ids: Vec<&str> = ids.collect();
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let ids =
        serde_json::to_string(&ids).map_err(|error| CommandError::Internal(error.to_string()))?;
    let rows: Vec<(String, Option<String>, String)> = sqlx::query_as(
        "SELECT id, title, file_path FROM tracks WHERE id IN (SELECT value FROM json_each(?))",
    )
    .bind(ids)
    .fetch_all(&services.db.0)
    .await
    .map_err(|error| CommandError::Internal(format!("Failed to read track names: {error}")))?;
    Ok(rows
        .into_iter()
        .map(|(id, title, file_path)| {
            let name = title.filter(|title| !title.is_empty()).unwrap_or_else(|| {
                std::path::Path::new(&file_path)
                    .file_name()
                    .map_or(file_path.clone(), |name| {
                        name.to_string_lossy().into_owned()
                    })
            });
            (id, name)
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A library holding tracks `t1` (titled) and `t2` (untitled).
    async fn library(directory: &std::path::Path) -> AppServices {
        let services = crate::test_support::signed_out(directory, directory.to_path_buf()).await;
        sqlx::query(
            "INSERT INTO tracks (id, track_hash, title, duration_seconds, file_path) VALUES
               ('t1', 'h1', 'Aurora', 1.0, '/music/aurora.wav'),
               ('t2', 'h2', NULL, 1.0, '/music/borealis.flac')",
        )
        .execute(&services.db.0)
        .await
        .unwrap();
        services
    }

    #[tokio::test]
    async fn an_idle_library_reports_nothing() {
        let directory = tempfile::tempdir().unwrap();
        let services = library(directory.path()).await;
        let work = background_work(&services).await.unwrap();
        assert_eq!(work, BackgroundWork::default());
        assert_eq!(work.summary(), None);
    }

    /// A batch's tracks read as running or queued by name while it holds
    /// them, and are gone once it lets go.
    #[tokio::test]
    async fn a_running_batch_reports_each_track_by_name() {
        let directory = tempfile::tempdir().unwrap();
        let services = library(directory.path()).await;
        let activity = services.analysis_tasks.activity();
        let batch = activity.batch(&["t1".to_owned(), "t2".to_owned()]);
        let running = activity.running("t1", "stems");

        let work = background_work(&services).await.unwrap();
        let tracks: Vec<_> = work
            .jobs
            .iter()
            .map(|job| (job.track.as_deref(), job.what.as_str(), job.state.clone()))
            .collect();
        assert_eq!(
            tracks,
            vec![
                (Some("Aurora"), "Separating stems", JobState::Running),
                (Some("borealis.flac"), "Queued", JobState::Queued),
            ]
        );
        assert_eq!(work.summary().as_deref(), Some("Analyzing 2 tracks"));

        drop(running);
        batch.finish("t1");
        let work = background_work(&services).await.unwrap();
        assert_eq!(work.summary().as_deref(), Some("Analyzing borealis.flac"));

        drop(batch);
        assert_eq!(
            background_work(&services).await.unwrap(),
            BackgroundWork::default()
        );
    }

    /// A step in failure backoff reads as paused, with the failure's first
    /// line; once its backoff has passed it does not.
    #[tokio::test]
    async fn a_step_in_backoff_reads_as_paused_with_its_failure() {
        let directory = tempfile::tempdir().unwrap();
        let services = library(directory.path()).await;
        let pool = &services.db.0;
        failures::record(pool, "t1", "stems", 1, "Out of memory\nTraceback: …")
            .await
            .unwrap();
        failures::record(pool, "t2", "roots", 1, "No chords")
            .await
            .unwrap();
        sqlx::query(
            "UPDATE preprocessing_failures SET next_retry_at = '2000-01-01T00:00:00Z'
              WHERE track_id = 't2'",
        )
        .execute(pool)
        .await
        .unwrap();

        let work = background_work(&services).await.unwrap();
        assert_eq!(work.jobs.len(), 1, "{work:?}");
        let held = &work.jobs[0];
        assert_eq!(held.track.as_deref(), Some("Aurora"));
        assert_eq!(held.steps, vec!["stems".to_owned()]);
        assert_eq!(held.what, "Separating stems paused");
        assert_eq!(held.state, JobState::Paused("Out of memory".into()));
        assert_eq!(work.summary().as_deref(), Some("1 analysis step paused"));
    }
}
