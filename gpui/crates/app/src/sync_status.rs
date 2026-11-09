//! Sync, as the status bar says it — one of four states:
//!
//! - `Up to date`
//! - `Syncing`: records, or media moving through storage
//! - `Offline · 3 pending`: not an error; the changes upload when the
//!   connection is back
//! - `2 not saved`: changes the server refused. The only one that opens: a
//!   compact list of what was refused, for whoever is debugging it.
//!
//! The words are [`SyncSummary`]'s. A transport error never reaches the bar:
//! a connection that keeps failing is offline, and its text is in the log.
use std::mem::discriminant;
use std::time::{Duration, Instant};

use crate::Luma;
use gpui::prelude::*;
use gpui::{div, px, AnyElement, Context, Entity};
use luma_lib::models::sync::{SyncStatus, SyncSummary};
use luma_ui::dialog::Popup;
use luma_ui::glass;
use luma_ui::node::{Instrument as _, Role};

/// How long work in flight lasts before the bar says `Syncing`: a quick
/// background sync should not flash the bar at every edit.
const SYNCING_HOLD: Duration = Duration::from_millis(800);
/// How long a lost connection lasts before the bar says `Offline`: connecting
/// at launch, or riding out one dropped request, is not worth a word.
const OFFLINE_HOLD: Duration = Duration::from_secs(3);
/// A backlog this deep is worth showing at once, however new it is.
const BACKLOG: usize = 10;

pub(crate) struct SyncState {
    pub status: SyncStatus,
    /// The refused changes' list.
    pub list: Popup<()>,
    /// What the bar says: the status's summary once it has held long enough.
    shown: SyncSummary,
    /// When the status's summary became the kind it is now, while that is
    /// not yet what the bar shows.
    held_since: Option<(std::mem::Discriminant<SyncSummary>, Instant)>,
    /// The last failure to read the status, logged once.
    read_error: Option<String>,
}

impl Default for SyncState {
    fn default() -> Self {
        Self {
            status: SyncStatus::default(),
            list: Popup::default(),
            shown: SyncSummary::UpToDate,
            held_since: None,
            read_error: None,
        }
    }
}

impl SyncState {
    /// Take a new status; whether the bar's words changed.
    fn observe(&mut self, status: SyncStatus, now: Instant) -> bool {
        let summary = status.summary();
        let hold = match summary {
            SyncSummary::UpToDate | SyncSummary::NotSaved { .. } => None,
            SyncSummary::Syncing if status.pending_uploads >= BACKLOG => None,
            SyncSummary::Syncing => Some(SYNCING_HOLD),
            SyncSummary::Offline { .. } => Some(OFFLINE_HOLD),
        };
        let kind = discriminant(&summary);
        let shown = match hold {
            _ if discriminant(&self.shown) == kind => summary,
            None => summary,
            Some(hold) => {
                let since = match self.held_since {
                    Some((held, since)) if held == kind => since,
                    _ => now,
                };
                self.held_since = Some((kind, since));
                if now.duration_since(since) >= hold {
                    summary
                } else {
                    self.shown
                }
            }
        };
        if discriminant(&shown) == kind {
            self.held_since = None;
        }
        if !matches!(shown, SyncSummary::NotSaved { .. }) {
            // The item that held it is gone, so there is no exit to play.
            self.list.begin_close_at(true);
        }
        let changed = shown != self.shown || status != self.status;
        self.shown = shown;
        self.status = status;
        changed
    }
}

impl Luma {
    pub(crate) fn watch_sync(&mut self, cx: &mut Context<Self>) {
        if !self.library.cloud_sync_enabled() {
            return;
        }
        cx.spawn(async move |this, cx| loop {
            let Ok(pending) = this.update(cx, |this, _| this.library.sync_status()) else {
                return;
            };
            let result = pending.await;
            let Ok(wait) = this.update(cx, |this, cx| {
                let state = &mut this.sync_status;
                let changed = match result {
                    Ok(status) => {
                        state.read_error = None;
                        state.observe(status, Instant::now())
                    }
                    Err(error) => {
                        let error = error.to_string();
                        if state.read_error.as_ref() != Some(&error) {
                            log::warn!("[sync] could not read the sync status: {error}");
                            state.read_error = Some(error);
                        }
                        false
                    }
                };
                if changed {
                    cx.notify();
                }
                let busy = state.status.uploading
                    || state.status.downloading
                    || state.status.media.is_some();
                this.library
                    .debounce(Duration::from_millis(if busy { 500 } else { 1000 }))
            }) else {
                return;
            };
            wait.await;
        })
        .detach();
    }

    /// Close the refused changes' list. Reports whether it was open:
    /// Escape's rung.
    pub(crate) fn close_sync_refusals(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.sync_status.list.is_open() {
            return false;
        }
        self.sync_status.list.begin_close(cx);
        cx.notify();
        true
    }

    /// The refused changes have been read: stop counting them.
    fn dismiss_sync_refusals(&mut self, cx: &mut Context<Self>) {
        self.close_sync_refusals(cx);
        let pending = self.library.dismiss_sync_refusals();
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        let mut status = this.sync_status.status.clone();
                        status.refused.clear();
                        this.sync_status.observe(status, Instant::now());
                    }
                    Err(error) => this.report_failure(
                        "Could not clear the list of changes not saved",
                        error,
                        cx,
                    ),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }
}

/// The status bar's sync item, while this library syncs at all.
pub(crate) fn item(shell: &Luma, app: &Entity<Luma>) -> Option<crate::status_bar::Item> {
    if !shell.library.cloud_sync_enabled() {
        return None;
    }
    let state = &shell.sync_status;
    let mut item = crate::status_bar::Item::new("sync-status", state.shown.to_string());
    item.ok = state.shown == SyncSummary::UpToDate;
    if matches!(state.shown, SyncSummary::NotSaved { .. }) {
        item.attention = true;
        item.press = Some(|this, cx| {
            if !this.close_sync_refusals(cx) {
                this.sync_status.list.open(());
                cx.notify();
            }
        });
        item.detail = crate::status_bar::popover(
            "sync-refused-float",
            &state.list,
            || refused(state, app),
            app,
            |this, cx| {
                this.close_sync_refusals(cx);
            },
        );
    }
    Some(item)
}

/// The refused changes, newest first, in a card above the bar.
fn refused(state: &SyncState, app: &Entity<Luma>) -> AnyElement {
    let dismiss = app.clone();
    let rows = state.status.refused.iter().map(|refusal| {
        let what = refusal.describe();
        div()
            .flex()
            .items_baseline()
            .gap(px(8.))
            .child(div().flex_1().min_w_0().truncate().child(what.clone()))
            .child(
                div()
                    .flex_none()
                    .text_color(glass::ink(glass::TEXT_TERTIARY))
                    .child(clock(&refusal.at)),
            )
            .agent_node(Role::Text, what)
            .into_any_element()
    });
    let card = luma_ui::float::popover_card()
        .id("sync-refused")
        .w(px(280.))
        .gap(px(8.))
        .p(px(10.))
        .text_size(px(12.))
        .child(
            div()
                .text_color(glass::ink(glass::TEXT_SECONDARY))
                .child("The server kept its own version of these."),
        )
        .child(
            div()
                .id("sync-refused-rows")
                .max_h(px(180.))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap(px(4.))
                .children(rows),
        )
        .child(
            div().flex().justify_end().child(
                luma_ui::button("Dismiss", luma_ui::Enabled::Yes)
                    .id("sync-refused-dismiss")
                    .on_click(move |_, _, cx| {
                        dismiss.update(cx, |this, cx| this.dismiss_sync_refusals(cx))
                    })
                    .agent_node(Role::Button, "Dismiss"),
            ),
        )
        .agent_node(Role::Card, "Not saved");
    card.into_any_element()
}

/// `HH:MM` local time of an RFC 3339 instant, or the instant as written.
fn clock(at: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(at).map_or_else(
        |_| at.to_owned(),
        |at| at.with_timezone(&chrono::Local).format("%H:%M").to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connected(pending: usize, uploading: bool) -> SyncStatus {
        SyncStatus {
            connected: true,
            uploading,
            pending_uploads: pending,
            ..SyncStatus::default()
        }
    }

    /// Work that finishes before the hold never reaches the bar; work that
    /// outlasts it does.
    #[test]
    fn a_quick_sync_does_not_flash_the_bar() {
        let start = Instant::now();
        let mut state = SyncState::default();
        state.observe(connected(1, true), start);
        assert_eq!(state.shown, SyncSummary::UpToDate);
        state.observe(connected(0, false), start + SYNCING_HOLD / 2);
        assert_eq!(state.shown, SyncSummary::UpToDate);

        state.observe(connected(1, true), start + SYNCING_HOLD);
        state.observe(connected(1, true), start + SYNCING_HOLD * 3);
        assert_eq!(state.shown, SyncSummary::Syncing);
    }

    /// A lost connection is held longer than work in flight, and a backlog
    /// shows at once.
    #[test]
    fn offline_waits_for_its_hold_and_a_backlog_does_not() {
        let start = Instant::now();
        let mut state = SyncState::default();
        let offline = SyncStatus {
            pending_uploads: 3,
            ..SyncStatus::default()
        };
        state.observe(offline.clone(), start);
        state.observe(offline.clone(), start + SYNCING_HOLD);
        assert_eq!(state.shown, SyncSummary::UpToDate);
        state.observe(offline, start + OFFLINE_HOLD);
        assert_eq!(state.shown.to_string(), "Offline · 3 pending");

        let mut state = SyncState::default();
        state.observe(connected(BACKLOG, true), start);
        assert_eq!(state.shown, SyncSummary::Syncing);
    }
}
