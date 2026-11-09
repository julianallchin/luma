//! Background work, as the status bar says it: one short phrase for the
//! dominant work at the bar's right end (`Analyzing 3 tracks`, `Setting up
//! Python`), nothing when idle, and on a press a compact list of all of it.
//!
//! The words are [`BackgroundWork::summary`]'s, except while an import runs:
//! then the bar says where the import stands, since its analysis is most of
//! what is running. This is compute only: moving records and media is sync's
//! item, and is not repeated here.
use std::time::{Duration, Instant};

use crate::Luma;
use gpui::prelude::*;
use gpui::{div, px, AnyElement, Context, Entity};
use luma_lib::models::background::{BackgroundJob, BackgroundWork, JobKind, JobState};
use luma_ui::dialog::Popup;
use luma_ui::glass;
use luma_ui::node::{Instrument as _, Role};

/// How long work lasts before the bar mentions it: a step that finishes in
/// under a second should not flash the bar.
const HOLD: Duration = Duration::from_millis(800);

#[derive(Default)]
pub(crate) struct BackgroundState {
    pub work: BackgroundWork,
    /// The list hung above the item.
    pub list: Popup<()>,
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

impl Luma {
    /// Close the list. Reports whether it was open: Escape's rung.
    pub(crate) fn close_background_list(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.background.list.is_open() {
            return false;
        }
        self.background.list.begin_close(cx);
        cx.notify();
        true
    }
}

/// The status bar's background-work item, while there is any to speak of.
pub(crate) fn item(shell: &Luma, app: &Entity<Luma>) -> Option<crate::status_bar::Item> {
    let label = crate::add_tracks::progress::status(shell)
        .map(|import| import.summary)
        .or_else(|| shell.background.shown.clone())?;
    let mut item = crate::status_bar::Item::new("background-work", label);
    item.press = Some(|this, cx| {
        if !this.close_background_list(cx) {
            this.background.list.open(());
            cx.notify();
        }
    });
    item.detail = crate::status_bar::popover(
        "background-work-float",
        &shell.background.list,
        || list(shell, app),
        app,
        |this, cx| {
            this.close_background_list(cx);
        },
    );
    Some(item)
}

/// Everything in the background, one short line each: the import, setup, the
/// tracks running, how many wait, and the steps held back.
fn list(shell: &Luma, app: &Entity<Luma>) -> AnyElement {
    let jobs = &shell.background.work.jobs;
    let mut rows: Vec<AnyElement> = Vec::new();

    if let Some(import) = crate::add_tracks::progress::status(shell) {
        let show = app.clone();
        rows.push(row(
            &import.summary,
            false,
            Some(
                luma_ui::button("Show", luma_ui::Enabled::Yes)
                    .id("background-import-show")
                    .on_click(move |_, _, cx| {
                        show.update(cx, |this, cx| {
                            this.close_background_list(cx);
                            this.show_track_import(cx);
                        })
                    })
                    .agent_node(Role::Button, "Show import")
                    .into_any_element(),
            ),
        ));
    }
    let analysis = |state: fn(&JobState) -> bool| {
        jobs.iter()
            .filter(move |job| job.kind == JobKind::Analysis && state(&job.state))
    };
    rows.extend(
        jobs.iter()
            .filter(|job| job.kind != JobKind::Analysis)
            .map(|job| row(&job.what, false, None)),
    );
    let running: Vec<&BackgroundJob> = analysis(|state| *state == JobState::Running).collect();
    let queued: Vec<&BackgroundJob> = analysis(|state| *state == JobState::Queued).collect();
    rows.extend(running.iter().map(|job| row(&with_track(job), false, None)));
    match (running.len(), queued.as_slice()) {
        (_, []) => {}
        (0, [first, ..]) if first.what != "Queued" => rows.push(row(
            &format!("{} queued · {}", tracks(queued.len()), first.what),
            true,
            None,
        )),
        (0, _) => rows.push(row(&format!("{} queued", tracks(queued.len())), true, None)),
        (_, queued) => rows.push(row(&format!("{} more queued", queued.len()), true, None)),
    }
    rows.extend(analysis(JobState::is_trouble).map(|job| row(&with_track(job), true, None)));

    let card = luma_ui::float::popover_card()
        .id("background-work")
        .w(px(280.))
        .p(px(10.))
        .text_size(px(12.))
        .child(
            div()
                .id("background-work-rows")
                .max_h(px(320.))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap(px(6.))
                .children(rows),
        )
        .agent_node(Role::Card, "Background work");
    card.into_any_element()
}

/// A job and the track it is about: `Separating stems · Aurora`.
fn with_track(job: &BackgroundJob) -> String {
    match &job.track {
        Some(track) => format!("{} · {track}", job.what),
        None => job.what.clone(),
    }
}

fn tracks(count: usize) -> String {
    if count == 1 {
        "1 track".into()
    } else {
        format!("{count} tracks")
    }
}

/// One short line, muted when it is waiting or held back, with what it
/// offers at its end.
fn row(text: &str, muted: bool, trailing: Option<AnyElement>) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(glass::ink(if muted {
                    glass::TEXT_TERTIARY
                } else {
                    glass::TEXT_SECONDARY
                }))
                .child(text.to_owned()),
        )
        .children(trailing)
        .agent_node(Role::Row, text.to_owned())
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
