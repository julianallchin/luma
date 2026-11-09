//! What the app is doing in the background, as the status bar reads it.
//!
//! One report of every kind of long-running work the user did not just ask
//! for and is not watching: setting Python up and fetching its models, each
//! track's analysis, steps held back after failures, and media moving to or
//! from the server. Record sync has its own report ([`super::sync`]).
//!
//! Like that one, this is a report, not a control surface: the scheduler owns
//! retries and backoff, and nothing here asks it to do anything.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundWork {
    /// The work in flight and the work held back, in the order the list shows
    /// it: setup first, then the tracks, then held steps, then transfers.
    pub jobs: Vec<BackgroundJob>,
}

/// One piece of background work.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundJob {
    pub kind: JobKind,
    /// What it is doing, in plain words: `Separating stems`, `Setting up
    /// Python`, `Uploading audio`.
    pub what: String,
    pub track_id: Option<String>,
    /// The track's name as the library shows it.
    pub track: Option<String>,
    /// The analysis steps it concerns, by wire name (`stems`, `beat_grid`).
    #[serde(default)]
    pub steps: Vec<String>,
    /// `(done, total)` when the work counts its own items.
    pub progress: Option<(usize, usize)>,
    pub state: JobState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    /// Creating the Python environment or installing its packages.
    PythonSetup,
    /// Fetching model weights once the environment is in.
    Models,
    /// A track's analysis.
    Analysis,
    /// Audio, stems or art moving to or from the server.
    Transfer,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state", content = "reason")]
pub enum JobState {
    Running,
    /// Waiting its turn.
    Queued,
    /// Held back after failing: on this track until its backoff ends, or on
    /// every track left in the batch when it failed the same way on several in
    /// a row. The reason is the failure's first line.
    Paused(String),
    /// Ended in failure, with nothing left to retry it this launch.
    Failed(String),
}

impl JobState {
    /// Worth a look: held back or failed, not merely waiting.
    #[must_use]
    pub fn is_trouble(&self) -> bool {
        matches!(self, Self::Paused(_) | Self::Failed(_))
    }
}

impl BackgroundWork {
    /// The bar's one phrase for the dominant work: `Setting up Python`,
    /// `Analyzing 3 tracks`, `Separating stems · Aurora`. `None` when nothing
    /// runs and nothing is held back.
    ///
    /// Setup outranks analysis, which waits for it anyway; analysis outranks
    /// transfers, which only move bytes. Held-back work speaks only when
    /// nothing runs.
    #[must_use]
    pub fn summary(&self) -> Option<String> {
        let busy = |kind| {
            self.jobs.iter().filter(move |job| {
                job.kind == kind && matches!(job.state, JobState::Running | JobState::Queued)
            })
        };
        if busy(JobKind::PythonSetup).next().is_some() {
            return Some("Setting up Python".into());
        }
        let tracks: Vec<&BackgroundJob> = busy(JobKind::Analysis).collect();
        match tracks.as_slice() {
            [] => {}
            [one] => {
                let name = one.track.as_deref().unwrap_or("1 track");
                return Some(match (&one.state, one.steps.as_slice()) {
                    (JobState::Running, [step]) => format!("{} · {name}", step_doing(step)),
                    _ => format!("Analyzing {name}"),
                });
            }
            many => return Some(format!("Analyzing {} tracks", many.len())),
        }
        if let Some(transfer) = busy(JobKind::Transfer).next() {
            return Some(match transfer.progress {
                Some((done, total)) if total > 0 => {
                    format!("{} · {} of {total}", transfer.what, (done + 1).min(total))
                }
                _ => transfer.what.clone(),
            });
        }
        if let Some(models) = busy(JobKind::Models).next() {
            return Some(models.what.clone());
        }
        if let Some(setup) = self
            .jobs
            .iter()
            .find(|job| job.kind == JobKind::PythonSetup && job.state.is_trouble())
        {
            return Some(setup.what.clone());
        }
        let held = self
            .jobs
            .iter()
            .filter(|job| job.kind == JobKind::Analysis && job.state.is_trouble())
            .count();
        match held {
            0 => None,
            1 => Some("1 analysis step paused".into()),
            n => Some(format!("{n} analysis steps paused")),
        }
    }

    /// Something in the list is held back or failed.
    #[must_use]
    pub fn attention(&self) -> bool {
        self.jobs.iter().any(|job| job.state.is_trouble())
    }
}

/// An analysis step as a noun, for a list of them: `beats`, `stems`.
#[must_use]
pub fn step_label(step: &str) -> &str {
    match step {
        "beat_grid" => "beats",
        "stems" => "stems",
        "mert" => "features",
        "roots" => "chords",
        "n2n" => "drums",
        "classifier" => "sections",
        "genre" => "genre",
        "waveform" => "waveform",
        other => other,
    }
}

/// An analysis step as what it is doing: `Finding beats`, `Separating stems`.
#[must_use]
pub fn step_doing(step: &str) -> String {
    match step {
        "beat_grid" => "Finding beats".into(),
        "stems" => "Separating stems".into(),
        "mert" => "Listening for features".into(),
        "roots" => "Reading chords".into(),
        "n2n" => "Finding drum hits".into(),
        "classifier" => "Finding sections".into(),
        "genre" => "Guessing the genre".into(),
        "waveform" => "Drawing the waveform".into(),
        other => format!("Running {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(kind: JobKind, what: &str, state: JobState) -> BackgroundJob {
        BackgroundJob {
            kind,
            what: what.into(),
            track_id: None,
            track: None,
            steps: Vec::new(),
            progress: None,
            state,
        }
    }

    fn track(name: &str, steps: &[&str], state: JobState) -> BackgroundJob {
        BackgroundJob {
            track: Some(name.into()),
            track_id: Some(name.to_lowercase()),
            steps: steps.iter().map(|step| (*step).to_owned()).collect(),
            ..job(JobKind::Analysis, "Analyzing", state)
        }
    }

    fn work(jobs: Vec<BackgroundJob>) -> BackgroundWork {
        BackgroundWork { jobs }
    }

    #[test]
    fn idle_says_nothing() {
        assert_eq!(BackgroundWork::default().summary(), None);
        assert!(!BackgroundWork::default().attention());
    }

    /// One track names its step and the track; several are counted.
    #[test]
    fn analysis_names_one_track_and_counts_several() {
        let one = work(vec![track("Aurora", &["stems"], JobState::Running)]);
        assert_eq!(one.summary().as_deref(), Some("Separating stems · Aurora"));

        let queued = work(vec![track("Aurora", &[], JobState::Queued)]);
        assert_eq!(queued.summary().as_deref(), Some("Analyzing Aurora"));

        let three = work(vec![
            track("Aurora", &["stems"], JobState::Running),
            track("Borealis", &[], JobState::Queued),
            track("Cirrus", &[], JobState::Queued),
        ]);
        assert_eq!(three.summary().as_deref(), Some("Analyzing 3 tracks"));
    }

    /// Setup outranks the analysis waiting on it, analysis outranks a
    /// transfer, and held-back steps speak only when nothing runs.
    #[test]
    fn setup_outranks_analysis_which_outranks_transfers_and_held_steps() {
        let held = track(
            "Aurora",
            &["stems"],
            JobState::Paused("out of memory".into()),
        );
        let setup = job(JobKind::PythonSetup, "Setting up Python", JobState::Running);
        let transfer = BackgroundJob {
            progress: Some((1, 2)),
            ..job(JobKind::Transfer, "Uploading audio", JobState::Running)
        };
        let queued = track("Borealis", &[], JobState::Queued);

        let all = work(vec![
            setup.clone(),
            queued.clone(),
            held.clone(),
            transfer.clone(),
        ]);
        assert_eq!(all.summary().as_deref(), Some("Setting up Python"));
        assert!(all.attention());

        let no_setup = work(vec![queued, held.clone(), transfer.clone()]);
        assert_eq!(no_setup.summary().as_deref(), Some("Analyzing Borealis"));

        let moving = work(vec![held.clone(), transfer]);
        assert_eq!(
            moving.summary().as_deref(),
            Some("Uploading audio · 2 of 2")
        );

        let only_held = work(vec![held]);
        assert_eq!(
            only_held.summary().as_deref(),
            Some("1 analysis step paused")
        );
        assert!(only_held.attention());
    }
}
