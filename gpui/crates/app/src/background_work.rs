//! Background work, as the status bar says it: one short phrase for the
//! dominant work at the bar's right end (`Analyzing 3 tracks`, `Setting up
//! Python`), nothing when idle, and on a press a compact list of all of it.
//!
//! The words are [`BackgroundWork::summary`]'s, except while an import runs:
//! then the bar says where the import stands, since its analysis is most of
//! what is running. Sync has its own item and is not repeated here.
use std::time::{Duration, Instant};

use crate::Luma;
use gpui::prelude::*;
use gpui::{div, px, AnyElement, Context, Div, Entity};
use luma_lib::models::background::{BackgroundJob, BackgroundWork, JobKind, JobState};
use luma_ui::glass;
use luma_ui::node::{Instrument as _, Role};

/// How long work lasts before the bar mentions it: a step that finishes in
/// under a second should not flash the bar.
const HOLD: Duration = Duration::from_millis(800);

#[derive(Default)]
pub(crate) struct BackgroundState {
    pub work: BackgroundWork,
    /// The list is open.
    pub expanded: bool,
    /// What the bar says: the summary once work has lasted [`HOLD`].
    shown: Option<String>,
    /// When work began, while the bar does not say so yet.
    since: Option<Instant>,
    /// The last failure to read the work, logged once.
    read_error: Option<String>,
}

impl BackgroundState {
    /// Take a new report; whether the bar or its list changed.
    fn observe(&mut self, work: BackgroundWork, now: Instant) -> bool {
        let shown = match (work.summary(), &self.shown) {
            (None, _) => {
                self.since = None;
                None
            }
            // Already showing work: new words at once.
            (Some(summary), Some(_)) => Some(summary),
            (Some(summary), None) => {
                let since = *self.since.get_or_insert(now);
                (now.duration_since(since) >= HOLD).then_some(summary)
            }
        };
        if shown.is_some() {
            self.since = None;
        }
        let changed = shown != self.shown || work != self.work;
        self.shown = shown;
        self.work = work;
        changed
    }
}

impl Luma {
    pub(crate) fn watch_background(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| loop {
            let Ok(pending) = this.update(cx, |this, _| this.library.background_work()) else {
                return;
            };
            let result = pending.await;
            let Ok(wait) = this.update(cx, |this, cx| {
                let state = &mut this.background;
                let changed = match result {
                    Ok(work) => {
                        state.read_error = None;
                        state.observe(work, Instant::now())
                    }
                    Err(error) => {
                        let error = error.to_string();
                        if state.read_error.as_ref() != Some(&error) {
                            log::warn!("[background] could not read the background work: {error}");
                            state.read_error = Some(error);
                        }
                        false
                    }
                };
                if changed {
                    cx.notify();
                }
                let busy = !state.work.jobs.is_empty();
                this.library
                    .debounce(Duration::from_millis(if busy { 500 } else { 1000 }))
            }) else {
                return;
            };
            wait.await;
        })
        .detach();
    }
}

/// The status bar's background-work item, while there is any to speak of.
pub(crate) fn item(shell: &Luma, app: &Entity<Luma>) -> Option<crate::status_bar::Item> {
    let import = crate::add_tracks::progress::status(shell);
    let state = &shell.background;
    let label = import
        .as_ref()
        .map(|import| import.summary.clone())
        .or_else(|| state.shown.clone())?;
    let mut item = crate::status_bar::Item::new("background-work", label);
    item.attention = import.as_ref().is_some_and(|import| import.failed)
        || (state.shown.is_some() && state.work.attention());
    item.press = Some(|this, cx| {
        this.background.expanded = !this.background.expanded;
        cx.notify();
    });
    item.detail = state.expanded.then(|| list(shell, app));
    Some(item)
}

/// Everything in the background, in a card above the bar: the import, setup,
/// each track's analysis, what is held back, and transfers.
fn list(shell: &Luma, app: &Entity<Luma>) -> AnyElement {
    let jobs = &shell.background.work.jobs;
    let mut groups: Vec<AnyElement> = Vec::new();

    if let Some(import) = crate::add_tracks::progress::status(shell) {
        let show = app.clone();
        groups.push(
            group(
                "Import",
                vec![row(
                    import.failed,
                    &import.summary,
                    None,
                    Some(
                        luma_ui::button("Show", luma_ui::Enabled::Yes)
                            .id("background-import-show")
                            .on_click(move |_, _, cx| {
                                show.update(cx, |this, cx| {
                                    this.background.expanded = false;
                                    this.show_track_import(cx);
                                })
                            })
                            .agent_node(Role::Button, "Show import")
                            .into_any_element(),
                    ),
                )],
            )
            .into_any_element(),
        );
    }

    let setup: Vec<AnyElement> = jobs
        .iter()
        .filter(|job| matches!(job.kind, JobKind::PythonSetup | JobKind::Models))
        .map(job_row)
        .collect();
    if !setup.is_empty() {
        groups.push(group("Python", setup).into_any_element());
    }

    let analysis = |state: fn(&JobState) -> bool| {
        jobs.iter()
            .filter(move |job| job.kind == JobKind::Analysis && state(&job.state))
    };
    let running: Vec<&BackgroundJob> = analysis(|state| *state == JobState::Running).collect();
    let queued: Vec<&BackgroundJob> = analysis(|state| *state == JobState::Queued).collect();
    let mut rows: Vec<AnyElement> = running.iter().map(|job| job_row(job)).collect();
    match (running.len(), queued.as_slice()) {
        (_, []) => {}
        // Nothing running yet: say why the tracks wait, once.
        (0, [first, ..]) => rows.push(row(
            false,
            &format!("{} queued", tracks(queued.len())),
            (first.what != "Queued").then(|| first.what.clone()),
            None,
        )),
        (_, queued) => rows.push(row(
            false,
            &format!("{} more queued", queued.len()),
            None,
            None,
        )),
    }
    if !rows.is_empty() {
        groups.push(group("Analysis", rows).into_any_element());
    }

    let held: Vec<AnyElement> = analysis(JobState::is_trouble).map(job_row).collect();
    if !held.is_empty() {
        groups.push(group("Paused", held).into_any_element());
    }

    let transfers: Vec<AnyElement> = jobs
        .iter()
        .filter(|job| job.kind == JobKind::Transfer)
        .map(job_row)
        .collect();
    if !transfers.is_empty() {
        groups.push(group("Transfers", transfers).into_any_element());
    }

    let card = luma_ui::float::popover_card()
        .id("background-work")
        .w(px(300.))
        .p(px(10.))
        .text_size(px(12.))
        .child(
            div()
                .id("background-work-rows")
                .max_h(px(320.))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap(px(10.))
                .children(groups),
        )
        .agent_node(Role::Card, "Background work");
    luma_ui::float::anchored_above(
        "background-work-float",
        crate::shell::STATUS_BAR_HEIGHT,
        luma_ui::float::Dismiss::Never,
        card.into_any_element(),
    )
}

fn tracks(count: usize) -> String {
    if count == 1 {
        "1 track".into()
    } else {
        format!("{count} tracks")
    }
}

/// A quiet heading over its rows.
fn group(heading: &'static str, rows: Vec<AnyElement>) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(4.))
        .child(
            div()
                .text_size(px(11.))
                .text_color(glass::ink(glass::TEXT_TERTIARY))
                .child(heading),
        )
        .children(rows)
}

/// One job: what, on which track, how far, and why when it is held back.
fn job_row(job: &BackgroundJob) -> AnyElement {
    let reason = match &job.state {
        JobState::Paused(reason) | JobState::Failed(reason) => Some(reason.clone()),
        JobState::Running | JobState::Queued => None,
    };
    let (title, detail) = match (&job.track, job.progress) {
        // A held step names its track over why.
        (Some(track), _) if reason.is_some() => (
            job.what.clone(),
            Some(format!("{track} · {}", reason.unwrap_or_default())),
        ),
        (Some(track), _) => (track.clone(), Some(job.what.clone())),
        (None, Some((done, total))) => (
            job.what.clone(),
            Some(format!("{} of {total}", (done + 1).min(total))),
        ),
        (None, None) => (job.what.clone(), reason),
    };
    row(job.state.is_trouble(), &title, detail, None)
}

/// A row: the notice mark when it is worth a look, the title over a muted
/// second line, and what it offers at its end.
fn row(
    trouble: bool,
    title: &str,
    detail: Option<String>,
    trailing: Option<AnyElement>,
) -> AnyElement {
    let label = match &detail {
        Some(detail) => format!("{title}: {detail}"),
        None => title.to_owned(),
    };
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .when(trouble, |row| row.child(luma_ui::notice::mark()))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .truncate()
                        .text_color(glass::ink(glass::TEXT_SECONDARY))
                        .child(title.to_owned()),
                )
                .children(detail.map(|detail| {
                    div()
                        .truncate()
                        .text_size(px(11.))
                        .text_color(glass::ink(glass::TEXT_TERTIARY))
                        .child(detail)
                })),
        )
        .children(trailing)
        .agent_node(Role::Row, label)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn analyzing(tracks: usize) -> BackgroundWork {
        BackgroundWork {
            jobs: (0..tracks)
                .map(|index| BackgroundJob {
                    kind: JobKind::Analysis,
                    what: "Queued".into(),
                    track_id: Some(format!("t{index}")),
                    track: Some(format!("Track {index}")),
                    steps: Vec::new(),
                    progress: None,
                    state: JobState::Queued,
                })
                .collect(),
        }
    }

    /// Work that ends before the hold never reaches the bar; work that
    /// outlasts it does, and from then on its words change at once.
    #[test]
    fn quick_work_does_not_flash_the_bar() {
        let start = Instant::now();
        let mut state = BackgroundState::default();
        state.observe(analyzing(1), start);
        assert_eq!(state.shown, None);
        state.observe(BackgroundWork::default(), start + HOLD / 2);
        assert_eq!(state.shown, None);

        state.observe(analyzing(1), start + HOLD);
        assert_eq!(state.shown, None);
        state.observe(analyzing(1), start + HOLD * 2);
        assert_eq!(state.shown.as_deref(), Some("Analyzing Track 0"));
        state.observe(analyzing(3), start + HOLD * 2);
        assert_eq!(state.shown.as_deref(), Some("Analyzing 3 tracks"));

        // Idle goes quiet at once.
        state.observe(BackgroundWork::default(), start + HOLD * 2);
        assert_eq!(state.shown, None);
    }
}
