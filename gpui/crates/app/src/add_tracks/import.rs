//! Importing: the per-song progress an import reports, starting one from
//! files or a source selection, and adding what it brought in to the venue.

use luma_lib::models::tracks::{
    StageOutcome, TrackAnalysisEvent, TrackImportPhase, TrackImportProgress,
};

use gpui::*;

use super::Route;
use crate::{Luma, TrackImportRequest};

pub(crate) struct TrackImportActivity {
    generation: u64,
    import_id: Option<String>,
    pub(super) progress: Option<TrackImportProgress>,
    pub(super) active: bool,
    /// What went wrong with the import as a whole, in plain words. The
    /// causes are in the log.
    pub(super) error: Option<String>,
    /// The venue the dialog was adding to when the import started. Every
    /// imported song joins it, which is what the person asked for by
    /// importing from there.
    venue_id: Option<String>,
    /// One per imported song, known once the files are in the library.
    /// Empty while they are still being copied in.
    pub(super) items: Vec<ImportItem>,
    /// Whether the progress page has been looked at since the import ended,
    /// so the status bar keeps a failed import on screen until it has been.
    pub(super) seen: bool,
}

pub(crate) struct ImportItem {
    pub(super) track_id: String,
    pub(super) title: String,
    pub(super) art: Option<String>,
    pub(super) analysis: ItemAnalysis,
}

/// One song's analysis as the scheduler has reported it: the stages it was
/// queued with, and which of them have started and settled.
///
/// Status is *derived* from this, never stored, so "Ready" cannot be written
/// down while a stage is still out — see [`Self::status`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct ItemAnalysis {
    /// Every stage the song will report. Empty until the scheduler queues it.
    stages: Vec<String>,
    /// Started and not yet settled, in the order they started.
    running: Vec<String>,
    settled: Vec<String>,
    /// The stages that failed, by their wire names. Why is in the log.
    failures: Vec<String>,
    /// The import has ended: whatever has not settled now never will.
    ended: bool,
}

/// What a row says about its song.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ItemStatus {
    Queued,
    Analyzing {
        running: Vec<String>,
        settled: usize,
        total: usize,
    },
    Ready,
    Failed(String),
}

impl ItemStatus {
    fn finished(&self) -> bool {
        matches!(self, Self::Ready | Self::Failed(_))
    }
}

impl ItemAnalysis {
    pub(super) fn status(&self) -> ItemStatus {
        let total = self.stages.len();
        let all_settled = total > 0 && self.settled.len() >= total;
        if all_settled || self.ended {
            if !self.failures.is_empty() {
                let stages = self
                    .failures
                    .iter()
                    .map(|stage| stage_label(stage))
                    .collect::<Vec<_>>()
                    .join(", ");
                return ItemStatus::Failed(format!("Could not analyze {stages}"));
            }
            if total > 0 && !all_settled {
                return ItemStatus::Failed("Analysis stopped before it finished".to_string());
            }
            // Never queued and the import is over: the song was already in
            // the library, so there was nothing to analyse.
            return ItemStatus::Ready;
        }
        if self.running.is_empty() && self.settled.is_empty() {
            ItemStatus::Queued
        } else {
            ItemStatus::Analyzing {
                running: self.running.clone(),
                settled: self.settled.len(),
                total,
            }
        }
    }

    fn apply(&mut self, event: &TrackAnalysisEvent) {
        match event {
            TrackAnalysisEvent::Queued { stages } => self.stages = stages.clone(),
            TrackAnalysisEvent::StageStarted { stage } => {
                if !self.settled.contains(stage) && !self.running.contains(stage) {
                    self.running.push(stage.clone());
                }
            }
            TrackAnalysisEvent::StageSettled { stage, outcome } => {
                self.running.retain(|running| running != stage);
                if self.settled.contains(stage) {
                    return;
                }
                self.settled.push(stage.clone());
                if let StageOutcome::Failed(error) = outcome {
                    log::warn!("Analysis of {stage} failed: {error}");
                    self.failures.push(stage.clone());
                }
            }
        }
    }
}

/// A stage's wire name as a person reads it. The wire names are stable
/// identifiers (`beat_grid`), not prose; one that is not listed here shows
/// as itself rather than disappearing.
pub(super) fn stage_label(stage: &str) -> &str {
    match stage {
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

impl TrackImportActivity {
    /// Fold one progress event into the per-song analyses.
    ///
    /// Only analysis events touch a song: phase one's copy events name the
    /// song they copied too, and reading one of those as "analysis started"
    /// is exactly how a song used to show Ready while it was still analysing.
    fn apply(&mut self, event: &TrackImportProgress) {
        if event.phase == TrackImportPhase::Complete {
            for item in &mut self.items {
                item.analysis.ended = true;
            }
            return;
        }
        let (Some(track_id), Some(analysis)) = (&event.track_id, &event.analysis) else {
            return;
        };
        if let Some(item) = self
            .items
            .iter_mut()
            .find(|item| &item.track_id == track_id)
        {
            item.analysis.apply(analysis);
        }
    }

    pub(super) fn failed(&self) -> usize {
        self.items
            .iter()
            .filter(|item| matches!(item.analysis.status(), ItemStatus::Failed(_)))
            .count()
    }

    /// The one line that says where the import is — the status bar item and the
    /// progress page's title both read it.
    ///
    /// Once the songs are known it is counted off their rows, so the header
    /// and the list cannot disagree: it reads as finished exactly when every
    /// row does.
    pub(super) fn summary(&self) -> String {
        if !self.items.is_empty() {
            let total = self.items.len();
            let finished = self
                .items
                .iter()
                .filter(|item| item.analysis.status().finished())
                .count();
            if finished < total {
                return if total == 1 {
                    "Analyzing 1 song…".to_string()
                } else {
                    format!("Analyzing {total} songs · {finished} of {total} done")
                };
            }
            let failed = self.failed();
            return if failed > 0 {
                format!("{} of {total} failed to import", failed)
            } else {
                format!("Imported {}", plural(total, "song"))
            };
        }
        if self.active {
            let (done, total) = self
                .progress
                .as_ref()
                .map_or((0, 0), |progress| (progress.done, progress.total));
            if total == 0 {
                "Importing songs…".to_string()
            } else {
                format!("Importing {} of {total} songs…", (done + 1).min(total))
            }
        } else if self.error.is_some() {
            "Import failed".to_string()
        } else {
            "Nothing was imported".to_string()
        }
    }
}

fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

impl Luma {
    /// The platform's own file picker, then the same import pipeline the DJ
    /// sources feed. A cancelled prompt is not an error: it is a person
    /// changing their mind, and it leaves the dialog exactly as it was.
    pub(super) fn import_track_files(&mut self, folder: bool, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(gpui::PathPromptOptions {
            // The Linux portal picker is folder-only when `directories` is
            // set, which greys out every file, so files and folders are two
            // separate prompts.
            files: !folder,
            directories: folder,
            multiple: true,
            prompt: Some("Import".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let paths = audio_files_in(paths);
            if paths.is_empty() {
                return;
            }
            this.update(cx, |this, cx| {
                this.begin_track_import(TrackImportRequest::Files(paths), cx)
            })
            .ok();
        })
        .detach();
    }

    /// Start `request` and follow it to completion.
    ///
    /// One path for every kind of import — a DJ library's selection and a
    /// folder of files differ only in what they name, and the progress,
    /// failure reporting and browser refresh after them are the same job.
    pub(super) fn begin_track_import(
        &mut self,
        request: TrackImportRequest,
        cx: &mut Context<Self>,
    ) {
        self.next_track_import = self
            .next_track_import
            .checked_add(1)
            .expect("track import generation exhausted");
        let generation = self.next_track_import;
        let venue_id = match self.overlay.open_mut() {
            Some(crate::shell::Overlay::AddTracks(state)) => {
                state.request(Route::Progress, cx);
                Some(state.venue_id.clone())
            }
            _ => None,
        };
        self.track_import = Some(TrackImportActivity {
            generation,
            import_id: None,
            progress: None,
            active: true,
            error: None,
            venue_id,
            items: Vec::new(),
            seen: false,
        });
        let mut progress = self.library.import_progress();
        let pending = self.library.import_tracks(request);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            let result = match result {
                Ok(result) => result,
                Err(error) => {
                    this.update(cx, |this, cx| {
                        if let Some(activity) = &mut this.track_import {
                            if activity.generation == generation {
                                log::warn!("The import did not start: {error}");
                                activity.active = false;
                                activity.error = Some("Could not import the songs".to_string());
                            }
                            cx.notify();
                        }
                    })
                    .ok();
                    return;
                }
            };
            let import_id = result.import_id.clone();
            this.update(cx, |this, cx| {
                let mut venue = None;
                if let Some(activity) = &mut this.track_import {
                    if activity.generation == generation {
                        activity.import_id = Some(import_id.clone());
                        for failure in &result.failures {
                            log::warn!("A song was not imported: {}", failure.message);
                        }
                        activity.error = (!result.failures.is_empty()).then(|| {
                            format!(
                                "{} could not be imported",
                                plural(result.failures.len(), "song")
                            )
                        });
                        activity.items = result
                            .tracks
                            .iter()
                            .map(|track| ImportItem {
                                track_id: track.id.clone(),
                                title: track
                                    .title
                                    .clone()
                                    .filter(|title| !title.is_empty())
                                    .or_else(|| track.source_filename.clone())
                                    .unwrap_or_else(|| track.file_path.clone()),
                                art: track.album_art_path.clone(),
                                analysis: ItemAnalysis::default(),
                            })
                            .collect();
                        venue = activity.venue_id.clone();
                    }
                }
                if let Some(venue_id) = venue {
                    let track_ids = result.tracks.iter().map(|track| track.id.clone()).collect();
                    this.add_imported_to_venue(venue_id, track_ids, cx);
                }
                this.refresh_open_track_browser(cx);
                cx.notify();
            })
            .ok();
            loop {
                let event = match progress.recv().await {
                    Ok(event) => event,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                };
                if event.import_id != import_id {
                    continue;
                }
                let complete = event.phase == TrackImportPhase::Complete;
                this.update(cx, |this, cx| {
                    if let Some(activity) = &mut this.track_import {
                        if activity.generation == generation {
                            activity.apply(&event);
                            activity.progress = Some(event.clone());
                            activity.active = !complete;
                            if let Some(error) = &event.error {
                                log::warn!("The import reported: {error}");
                                activity.error =
                                    Some("Some songs could not be imported".to_string());
                            }
                        }
                    }
                    if complete {
                        this.refresh_open_track_browser(cx);
                    }
                    cx.notify();
                })
                .ok();
                if complete {
                    break;
                }
            }
        })
        .detach();
    }

    /// Put every song an import brought in into the venue it was imported
    /// from, then show them in the song list. The dialog stays open: it is
    /// showing the import's progress, which is still going.
    fn add_imported_to_venue(
        &mut self,
        venue_id: String,
        track_ids: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        if track_ids.is_empty() {
            return;
        }
        let pending: Vec<_> = track_ids
            .iter()
            .map(|track_id| {
                let request_id = uuid::Uuid::new_v4().to_string();
                self.library
                    .ensure_track_in_venue(&request_id, track_id, &venue_id, None)
            })
            .collect();
        cx.spawn(async move |this, cx| {
            let mut errors = Vec::new();
            for added in pending {
                if let Err(error) = added.await {
                    errors.push(error.to_string());
                }
            }
            let Ok(rows) = this.read_with(cx, |this, _| this.library.tracks(&venue_id)) else {
                return;
            };
            let rows = rows.await;
            this.update(cx, |this, cx| {
                let generation = this
                    .songs
                    .as_ref()
                    .filter(|songs| songs.venue_id() == venue_id)
                    .map(crate::tracks::Tracks::load_generation);
                if let (Ok(rows), Some(generation)) = (rows, generation) {
                    this.with_tracks_for_venue(&venue_id, generation, cx, |songs| {
                        songs.replace_rows(rows)
                    });
                }
                if !errors.is_empty() {
                    for error in &errors {
                        log::warn!("An imported song did not join the venue: {error}");
                    }
                    this.report(
                        format!(
                            "Could not add {} to this venue",
                            plural(errors.len(), "imported song")
                        ),
                        cx,
                    );
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

const AUDIO_EXTENSIONS: [&str; 9] = [
    "mp3", "wav", "flac", "aif", "aiff", "m4a", "aac", "ogg", "opus",
];

/// Files stay as picked; a folder becomes the audio files under it, sorted so
/// the import order is stable.
fn audio_files_in(paths: Vec<std::path::PathBuf>) -> Vec<std::path::PathBuf> {
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for path in entries.flatten().map(|entry| entry.path()) {
            if path.is_dir() {
                walk(&path, out);
            } else if path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| AUDIO_EXTENSIONS.iter().any(|a| ext.eq_ignore_ascii_case(a)))
            {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    for path in paths {
        if path.is_dir() {
            let mut found = Vec::new();
            walk(&path, &mut found);
            found.sort();
            out.extend(found);
        } else {
            out.push(path);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{
        ImportItem, ItemAnalysis, ItemStatus, StageOutcome, TrackAnalysisEvent,
        TrackImportActivity, TrackImportPhase, TrackImportProgress,
    };

    const STAGES: [&str; 4] = ["beat_grid", "stems", "roots", "waveform"];

    fn event(
        phase: TrackImportPhase,
        track_id: Option<&str>,
        analysis: Option<TrackAnalysisEvent>,
    ) -> TrackImportProgress {
        TrackImportProgress {
            import_id: "import".into(),
            source: "files".into(),
            phase,
            done: 0,
            total: 2,
            track_id: track_id.map(Into::into),
            current_track: None,
            analysis,
            error: None,
        }
    }

    fn analysis(track: &str, analysis: TrackAnalysisEvent) -> TrackImportProgress {
        event(TrackImportPhase::Analyzing, Some(track), Some(analysis))
    }

    fn queued(track: &str) -> TrackImportProgress {
        analysis(
            track,
            TrackAnalysisEvent::Queued {
                stages: STAGES.iter().map(|stage| stage.to_string()).collect(),
            },
        )
    }

    fn started(track: &str, stage: &str) -> TrackImportProgress {
        analysis(
            track,
            TrackAnalysisEvent::StageStarted {
                stage: stage.into(),
            },
        )
    }

    fn settled(track: &str, stage: &str, outcome: StageOutcome) -> TrackImportProgress {
        analysis(
            track,
            TrackAnalysisEvent::StageSettled {
                stage: stage.into(),
                outcome,
            },
        )
    }

    fn activity() -> TrackImportActivity {
        let item = |id: &str| ImportItem {
            track_id: id.into(),
            title: id.into(),
            art: None,
            analysis: ItemAnalysis::default(),
        };
        TrackImportActivity {
            generation: 1,
            import_id: Some("import".into()),
            progress: None,
            active: true,
            error: None,
            venue_id: None,
            items: vec![item("a"), item("b")],
            seen: false,
        }
    }

    fn status(activity: &TrackImportActivity, index: usize) -> ItemStatus {
        activity.items[index].analysis.status()
    }

    /// The bug this fold was rewritten for: phase one's "copied" event names
    /// the song, and the old fold read it as analysis starting — so the
    /// scheduler's real start then read as the finish.
    #[test]
    fn a_copy_event_is_not_analysis() {
        let mut activity = activity();
        activity.apply(&event(TrackImportPhase::Importing, Some("a"), None));
        assert_eq!(status(&activity, 0), ItemStatus::Queued);
        activity.apply(&queued("a"));
        activity.apply(&started("a", "beat_grid"));
        assert!(matches!(status(&activity, 0), ItemStatus::Analyzing { .. }));
        assert_eq!(activity.summary(), "Analyzing 2 songs · 0 of 2 done");
    }

    /// "Ready" exactly when every queued stage has settled, whatever order
    /// the stages start and settle in — parallel siblings settle in any
    /// order — and never one event sooner.
    #[test]
    fn ready_is_never_shown_while_a_stage_is_pending() {
        // Every interleaving the scheduler can produce for four stages is a
        // shuffle of their start and settle events with each start before its
        // settle; a seeded walk visits enough of them to cover the orders.
        let mut seed = 0x2545_f491_4f6c_dd1d_u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..500 {
            let mut activity = activity();
            activity.apply(&queued("a"));
            let mut unstarted: Vec<&str> = STAGES.to_vec();
            let mut running: Vec<&str> = Vec::new();
            let mut settled_count = 0;
            while settled_count < STAGES.len() {
                let start = !unstarted.is_empty() && (running.is_empty() || next() % 2 == 0);
                if start {
                    let stage = unstarted.remove(next() as usize % unstarted.len());
                    activity.apply(&started("a", stage));
                    running.push(stage);
                } else {
                    let stage = running.remove(next() as usize % running.len());
                    activity.apply(&settled("a", stage, StageOutcome::Done));
                    settled_count += 1;
                }
                let now = status(&activity, 0);
                if settled_count < STAGES.len() {
                    assert!(
                        matches!(now, ItemStatus::Analyzing { .. }),
                        "{settled_count} of {} settled reads {now:?}",
                        STAGES.len()
                    );
                    assert!(!activity.summary().starts_with("Imported"));
                } else {
                    assert_eq!(now, ItemStatus::Ready);
                }
            }
        }
    }

    #[test]
    fn a_failed_stage_fails_the_song_once_the_rest_settle() {
        let mut activity = activity();
        activity.apply(&queued("a"));
        activity.apply(&started("a", "stems"));
        activity.apply(&settled("a", "stems", StageOutcome::Failed("boom".into())));
        // Siblings still running: the song is not finished in either sense.
        assert!(matches!(status(&activity, 0), ItemStatus::Analyzing { .. }));
        for stage in ["beat_grid", "roots", "waveform"] {
            activity.apply(&settled("a", stage, StageOutcome::Skipped));
        }
        assert_eq!(
            status(&activity, 0),
            ItemStatus::Failed("Could not analyze stems".into())
        );
        // A stage settles once: a repeat cannot change the song's story.
        activity.apply(&settled("a", "stems", StageOutcome::Done));
        assert_eq!(
            status(&activity, 0),
            ItemStatus::Failed("Could not analyze stems".into())
        );
    }

    #[test]
    fn completion_settles_only_what_the_rows_can_account_for() {
        let mut activity = activity();
        // `a` was queued and stopped part-way; `b` was never queued, which is
        // what a song already in the library looks like.
        activity.apply(&queued("a"));
        activity.apply(&started("a", "beat_grid"));
        activity.apply(&event(TrackImportPhase::Complete, None, None));
        activity.active = false;
        assert_eq!(
            status(&activity, 0),
            ItemStatus::Failed("Analysis stopped before it finished".into())
        );
        assert_eq!(status(&activity, 1), ItemStatus::Ready);
        assert_eq!(activity.summary(), "1 of 2 failed to import");
    }

    #[test]
    fn the_header_finishes_with_the_last_row() {
        let mut activity = activity();
        for track in ["a", "b"] {
            activity.apply(&queued(track));
        }
        for stage in STAGES {
            activity.apply(&settled("a", stage, StageOutcome::Done));
        }
        assert_eq!(activity.summary(), "Analyzing 2 songs · 1 of 2 done");
        for stage in STAGES {
            activity.apply(&settled("b", stage, StageOutcome::Done));
        }
        // Before the terminal event arrives: the rows are the truth.
        assert!(activity.active);
        assert_eq!(activity.summary(), "Imported 2 songs");
    }
}
