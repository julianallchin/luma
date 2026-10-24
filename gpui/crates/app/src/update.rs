//! Self-update in the window: the hourly poll, the sidebar's "Restart to
//! update" row, and the About section's controls.
//!
//! The work is [`luma_update::Updater`]'s, on a thread of its own; this module
//! mirrors its status onto [`Luma`] and draws it, the way Zed's title bar
//! draws its `AutoUpdater`. A finished update waits on disk until the user
//! restarts — the app is never restarted under someone mid-show.

use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use luma_ui::float::{self, RowState};
use luma_ui::icons::IconName;
use luma_ui::node::{AgentNode, Instrument, Role};
use luma_ui::Enabled;
use luma_update::{Status, Updater};

use crate::Luma;

/// How long after launch the first automatic check waits: past the settings
/// read that says whether to check at all, and out of the way of a cold start.
const FIRST_CHECK: Duration = Duration::from_secs(10);

pub(crate) struct AutoUpdate {
    updater: Arc<Updater>,
    /// The updater's status, as of the last change it reported.
    pub(crate) status: Status,
    /// The `auto_update` setting, as last read; `None` before the first
    /// read. Gates the poll only: a manual check runs either way.
    enabled: Option<bool>,
    /// Whether this app polls at all — see [`Luma::watch_updates`].
    polling: bool,
}

impl AutoUpdate {
    pub(crate) fn new() -> Self {
        let updater = Updater::new(luma_lib::VERSION);
        Self {
            status: updater.status(),
            updater,
            enabled: None,
            polling: false,
        }
    }
}

impl Luma {
    /// Mirror the updater's status, and poll it. A no-op for a build that
    /// cannot update. A host with the cloud turned off — the harness — does
    /// not poll: it must never reach the network on its own.
    pub(crate) fn watch_updates(&mut self, cx: &mut Context<Self>) {
        if self.update.updater.unavailable().is_some() {
            return;
        }
        let mut statuses = self.update.updater.subscribe();
        cx.spawn(async move |this, cx| {
            while statuses.changed().await.is_ok() {
                let status = statuses.borrow_and_update().clone();
                if this
                    .update(cx, |this, cx| {
                        this.update.status = status;
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
            }
        })
        .detach();
        if !luma_ui::runtime::Runtime::with(|runtime| runtime.cloud) {
            return;
        }
        self.update.polling = true;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(FIRST_CHECK).await;
            loop {
                let polled = this.update(cx, |this, _| {
                    if this.update.enabled == Some(true) {
                        this.update.updater.check(false);
                    }
                });
                if polled.is_err() {
                    return;
                }
                cx.background_executor()
                    .timer(luma_update::POLL_INTERVAL)
                    .await;
            }
        })
        .detach();
    }

    /// Apply a read of the `auto_update` setting. Turning it on checks now
    /// rather than at the next tick of the hourly poll; the launch read is
    /// not a turn, and leaves the first check to the poll.
    pub(crate) fn set_auto_update(&mut self, enabled: bool) {
        let turned_on = enabled && self.update.enabled == Some(false);
        self.update.enabled = Some(enabled);
        if turned_on && self.update.polling {
            self.update.updater.check(false);
        }
    }

    fn check_for_updates(&mut self, cx: &mut Context<Self>) {
        self.update.updater.check(true);
        cx.notify();
    }

    /// Relaunch into the installed update. gpui waits for this process to
    /// exit before it starts the new one.
    fn restart_to_update(&mut self, cx: &mut Context<Self>) {
        if let Status::Updated { restart, .. } = &self.update.status {
            cx.set_restart_path(restart.clone());
            cx.restart();
        }
    }
}

/// The sidebar's update row: present only once there is something to press.
///
/// Downloading and installing say nothing here, as in Zed: an update the user
/// has not asked for should not ask for attention until it is ready.
pub(crate) fn sidebar(shell: &Luma, app: &Entity<Luma>) -> Option<AnyElement> {
    let Status::Updated { version, .. } = &shell.update.status else {
        return None;
    };
    let label = format!("Restart to update to {version}");
    let restart = app.clone();
    let keyed = app.clone();
    Some(
        div()
            .flex_shrink_0()
            .px(px(12.))
            .pb(px(4.))
            .child(
                float::nav_row(RowState::Rest, "update-restart")
                    .id("update-restart")
                    .tab_stop(true)
                    .on_key_down(move |event, _, cx| {
                        if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                            keyed.update(cx, |this, cx| this.restart_to_update(cx));
                        }
                    })
                    .on_click(move |_, _, cx| {
                        restart.update(cx, |this, cx| this.restart_to_update(cx));
                    })
                    .child(
                        gpui_component::Icon::new(IconName::RotateRight)
                            .size(px(12.))
                            .text_color(luma_ui::glass::ink(0.72)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(luma_ui::glass::ink(0.85))
                            .child(label.clone()),
                    )
                    .agent_node(Role::Button, label),
            )
            .into_any_element(),
    )
}

/// What the About section says about updates, and the button that fits it.
pub(crate) fn about(shell: &Luma, app: &Entity<Luma>) -> (AnyElement, Option<String>) {
    let updater = &shell.update.updater;
    let status = &shell.update.status;
    let help = match (updater.unavailable(), status) {
        (Some(reason), _) => Some(reason.to_string()),
        (None, Status::Checking) => Some("Checking for updates…".into()),
        (None, Status::Downloading { version }) => Some(format!("Downloading {version}…")),
        (None, Status::Installing { version }) => Some(format!("Installing {version}…")),
        (None, Status::Updated { version, .. }) => Some(format!(
            "Luma {version} is installed and runs after a restart."
        )),
        (None, Status::UpToDate) => Some("Luma is up to date.".into()),
        (None, Status::Errored(error)) => Some(format!("Could not update: {error}")),
        (None, Status::Idle) => None,
    };
    let button = if matches!(status, Status::Updated { .. }) {
        let restart = app.clone();
        luma_ui::button("Restart to Update", Enabled::Yes)
            .id("settings-restart-to-update")
            .on_click(move |_, _, cx| restart.update(cx, |this, cx| this.restart_to_update(cx)))
            .agent_node(Role::Button, "Restart to Update")
    } else {
        let pressable = updater.unavailable().is_none() && !status.busy();
        let check = app.clone();
        luma_ui::button("Check for Updates", Enabled::from(pressable))
            .id("settings-check-for-updates")
            .when(pressable, |button| {
                button.on_click(move |_, _, cx| {
                    check.update(cx, |this, cx| this.check_for_updates(cx))
                })
            })
            .agent_node(Role::Button, "Check for Updates")
            .agent_disabled(!pressable)
    };
    (button.into_any_element(), help)
}
