//! Sync, as the status bar says it: up to date, syncing, or what is wrong.
use crate::Luma;
use gpui::prelude::*;
use gpui::{div, px, AnyElement, Context};
use luma_lib::models::sync::SyncStatus;
use luma_ui::node::{Instrument as _, Role};

#[derive(Default)]
pub(crate) struct SyncState {
    pub status: SyncStatus,
    pub expanded: bool,
    pub read_error: Option<String>,
    activity_since: Option<std::time::Instant>,
    show_activity: bool,
}

impl SyncState {
    /// Everything worth reading when the row is expanded.
    fn details(&self) -> Vec<String> {
        let mut details = Vec::new();
        details.extend(self.status.error.iter().cloned());
        details.extend(self.read_error.iter().cloned());
        if let Some(at) = &self.status.last_synced_at {
            details.push(format!("Last synced {at}"));
        }
        details
    }

    fn busy(&self) -> bool {
        self.status.uploading || self.status.downloading
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
                let (status, error) = match result {
                    Ok(status) => (status, None),
                    Err(error) => (this.sync_status.status.clone(), Some(error.to_string())),
                };
                let state = &mut this.sync_status;
                let busy = status.uploading || status.downloading;
                if busy || status.pending_uploads > 0 {
                    state
                        .activity_since
                        .get_or_insert_with(std::time::Instant::now);
                } else {
                    state.activity_since = None;
                }
                // Quick background syncs should not flash UI at every edit.
                // A meaningful backlog is useful immediately; slower work gets
                // a compact indicator once it has lasted long enough to notice.
                let show_activity = status.pending_uploads >= 10
                    || state.activity_since.is_some_and(|since| {
                        since.elapsed() >= std::time::Duration::from_millis(800)
                    });
                if status != state.status
                    || error != state.read_error
                    || show_activity != state.show_activity
                {
                    state.status = status;
                    state.read_error = error;
                    state.show_activity = show_activity;
                    cx.notify();
                }
                this.library
                    .debounce(std::time::Duration::from_millis(if busy {
                        500
                    } else {
                        1000
                    }))
            }) else {
                return;
            };
            wait.await;
        })
        .detach();
    }
}

/// The status bar's sync item, while this library syncs at all.
///
/// The item is one word or two, and a transport error is the one thing worth
/// reading at a glance: "Sync needs attention" is what a user sees for twenty
/// minutes while nothing syncs and nothing says why. The first sentence of
/// the error is the item itself; the whole of it is one press away, hung
/// above the bar.
pub(crate) fn item(shell: &Luma) -> Option<crate::status_bar::Item> {
    if !shell.library.cloud_sync_enabled() {
        return None;
    }
    let state = &shell.sync_status;
    let status = &state.status;
    let error = status.error.as_ref().or(state.read_error.as_ref());
    let label = if let Some(error) = error {
        let headline = error
            .split(['.', '\n'])
            .next()
            .unwrap_or(error)
            .trim()
            .to_owned();
        if headline.is_empty() {
            "Sync needs attention".to_owned()
        } else {
            headline
        }
    } else if state.show_activity && status.pending_uploads > 0 {
        format!("Syncing · {}", status.pending_uploads)
    } else if state.show_activity && state.busy() {
        "Syncing…".to_owned()
    } else {
        "Synced".to_owned()
    };
    let mut item = crate::status_bar::Item::new("sync-status", label);
    item.danger = error.is_some();
    item.ok = error.is_none() && !state.show_activity;
    item.press = Some(|this, cx| {
        this.sync_status.expanded = !this.sync_status.expanded;
        cx.notify();
    });
    item.detail = state.expanded.then(|| details(state));
    Some(item)
}

/// Everything worth reading about sync, in a card above the bar.
fn details(state: &SyncState) -> AnyElement {
    let lines = state.details();
    let card = luma_ui::float::popover_card()
        .id("sync-details")
        .w(px(320.))
        .max_h(px(240.))
        .overflow_y_scroll()
        .gap(px(8.))
        .p(px(10.))
        .text_size(px(12.))
        .children(if lines.is_empty() {
            vec![div().child("Up to date").into_any_element()]
        } else {
            lines
                .into_iter()
                .map(|line| {
                    div()
                        .child(line.clone())
                        .agent_node(Role::Text, line)
                        .into_any_element()
                })
                .collect()
        });
    luma_ui::float::anchored_above(
        "sync-details-float",
        crate::shell::STATUS_BAR_HEIGHT,
        luma_ui::float::Dismiss::Never,
        card.into_any_element(),
    )
}
