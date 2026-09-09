//! A quiet sidebar row; failed deliveries explain themselves on expansion.
use crate::Luma;
use gpui::prelude::*;
use gpui::{div, px, AnyElement, Context, Entity};
use luma_lib::models::sync::SyncStatus;
use luma_ui::node::{AgentNode as _, Instrument as _, Role};

#[derive(Default)]
pub(crate) struct SidebarSync {
    pub status: SyncStatus,
    pub expanded: bool,
    pub read_error: Option<String>,
    retrying: bool,
    activity_since: Option<std::time::Instant>,
    show_activity: bool,
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
                if status.syncing || status.pending_changes > 0 {
                    state
                        .activity_since
                        .get_or_insert_with(std::time::Instant::now);
                } else {
                    state.activity_since = None;
                }
                // Quick background syncs should not flash UI at every edit.
                // A meaningful backlog is useful immediately; slower work gets
                // a compact indicator once it has lasted long enough to notice.
                let show_activity = status.pending_changes >= 10
                    || state.activity_since.is_some_and(|since| {
                        since.elapsed() >= std::time::Duration::from_millis(800)
                    });
                let syncing = status.syncing;
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
                    .debounce(std::time::Duration::from_millis(if syncing {
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

    fn retry_sidebar_sync(&mut self, cx: &mut Context<Self>) {
        if self.sync_status.retrying || self.sync_status.status.syncing {
            return;
        }
        self.sync_status.retrying = true;
        let pending = self.library.retry_sync();
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                this.sync_status.retrying = false;
                this.sync_status.read_error = result.err().map(|e| e.to_string());
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }
}

pub(crate) fn sidebar(shell: &Luma, app: &Entity<Luma>) -> AnyElement {
    if !shell.library.cloud_sync_enabled() {
        return div().into_any_element();
    }
    let state = &shell.sync_status;
    let status = &state.status;
    let count =
        status.failures.len() + status.errors.len() + usize::from(state.read_error.is_some());
    if !state.show_activity && count == 0 && !state.expanded {
        return div().into_any_element();
    }
    let label = if status.syncing || (status.pending_changes > 0 && count == 0) {
        if status.pending_changes > 0 {
            format!("Syncing · {}", status.pending_changes)
        } else {
            "Syncing…".into()
        }
    } else if count > 0 {
        format!("Sync needs attention · {count}")
    } else {
        "Up to date".into()
    };
    let toggle = app.clone();
    let keyboard_toggle = app.clone();
    let keyboard_retry = app.clone();
    let retry = app.clone();
    let mut details = status.errors.clone();
    details.extend(state.read_error.iter().cloned());
    details.extend(status.failures.iter().map(|failure| {
        format!(
            "{} · {}
{}
{}",
            failure.table_name.replace('_', " "),
            if failure.permanent {
                "Blocked"
            } else {
                "Will retry"
            },
            failure.last_error.as_deref().unwrap_or("Delivery failed"),
            failure.record_id.replace('\u{1f}', " / ")
        )
    }));
    div()
        .flex()
        .flex_col()
        .flex_shrink_0()
        .px(px(12.))
        .py(px(6.))
        .gap(px(6.))
        .text_size(px(11.))
        .text_color(if count > 0 {
            luma_ui::ladder::danger()
        } else {
            luma_ui::ladder::muted_foreground()
        })
        .child(
            div()
                .id("sync-status")
                .tab_index(0)
                .on_key_down(move |event, _, cx| {
                    if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                        keyboard_toggle.update(cx, |this, cx| {
                            this.sync_status.expanded = !this.sync_status.expanded;
                            cx.notify();
                        });
                    }
                })
                .cursor_pointer()
                .py(px(4.))
                .child(label.clone())
                .on_click(move |_, _, cx| {
                    toggle.update(cx, |this, cx| {
                        this.sync_status.expanded = !this.sync_status.expanded;
                        cx.notify();
                    })
                })
                .agent_node(Role::Button, label),
        )
        .when(state.expanded, |el| {
            el.child(
                div()
                    .id("sync-details")
                    .max_h(px(240.))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .when(status.syncing, |el| {
                        let phase = status
                            .progress
                            .as_ref()
                            .map(|progress| progress.phase.clone())
                            .unwrap_or_else(|| "Preparing sync…".into());
                        let el = el.child(div().child(phase.clone()).agent_node(Role::Text, phase));
                        let Some(progress) = status.progress.as_ref() else {
                            return el;
                        };
                        let Some(total) = progress.total.filter(|total| *total > 0) else {
                            return el;
                        };
                        let completed = progress.completed.min(total);
                        let summary = format!("{completed} / {total} {}", progress.unit);
                        el.child(div().child(summary.clone()).agent_node(Role::Text, summary))
                            .child(
                                div()
                                    .h(px(3.))
                                    .w_full()
                                    .bg(luma_ui::ladder::foreground_alpha(0.12))
                                    .child(
                                        div()
                                            .h_full()
                                            .w(gpui::relative(completed as f32 / total as f32))
                                            .bg(luma_ui::ladder::foreground_alpha(0.65)),
                                    ),
                            )
                    })
                    .children(details.into_iter().map(|message| div().child(message)))
                    .when(!status.syncing && count > 0, |el| {
                        el.child(
                            luma_ui::button(
                                "Retry sync",
                                (!state.retrying && !status.syncing).into(),
                            )
                            .id("retry-sync")
                            .on_key_down(move |event, _, cx| {
                                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                                    keyboard_retry
                                        .update(cx, |this, cx| this.retry_sidebar_sync(cx));
                                }
                            })
                            .on_click(move |_, _, cx| {
                                retry.update(cx, |this, cx| this.retry_sidebar_sync(cx))
                            })
                            .agent_node(Role::Button, "Retry sync")
                            .agent_disabled(state.retrying || status.syncing),
                        )
                    }),
            )
        })
        .into_any_element()
}
