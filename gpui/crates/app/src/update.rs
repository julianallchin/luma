//! Self-update in the window: the hourly poll, the "update ready" dialog, the
//! status bar's "Restart to update" item, and the About section's controls.
//!
//! The work is [`luma_update::Updater`]'s, on a thread of its own; this module
//! mirrors its status onto [`Luma`] and draws it, the way Zed's title bar
//! draws its `AutoUpdater`. Checking, downloading and installing are silent.
//! Once the new version is on disk, one dialog says so: relaunch now, or
//! later. The install has already replaced the old tree, so "later" means the
//! next launch runs the new version. The app is never restarted under someone
//! mid-show; only the button restarts it.

use std::sync::Arc;
use std::time::Duration;

use gpui::*;
use luma_ui::dialog::morph::{self, MorphSize};
use luma_ui::float;
use luma_ui::glass;
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
    /// The version the ready dialog was raised for, so that a status the
    /// updater reports twice raises it once.
    announced: Option<String>,
}

/// The "update ready" dialog's state, in [`crate::shell::Overlay::UpdateReady`].
pub(crate) struct Ready {
    version: SharedString,
}

impl AutoUpdate {
    pub(crate) fn new() -> Self {
        let updater = Updater::new(luma_lib::VERSION);
        Self {
            status: updater.status(),
            updater,
            enabled: None,
            polling: false,
            announced: None,
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
                        this.announce_update(cx);
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

    /// Raise the ready dialog the first time a version is on disk. A dialog
    /// already up is left alone: the status bar's item still offers the restart.
    fn announce_update(&mut self, cx: &mut Context<Self>) {
        let Status::Updated { version, .. } = &self.update.status else {
            return;
        };
        if self.update.announced.as_ref() == Some(version) || self.overlay.as_open().is_some() {
            return;
        }
        self.update.announced = Some(version.clone());
        let version = version.clone().into();
        self.overlay
            .open(crate::shell::Overlay::UpdateReady(Ready { version }));
        cx.notify();
    }

    /// The dialog's "Relaunch now".
    fn relaunch_from_dialog(&mut self, cx: &mut Context<Self>) {
        self.close_overlay(cx);
        self.restart_to_update(cx);
    }

    /// Relaunch into the installed update. gpui waits for this process to
    /// exit before it starts the new one.
    pub(crate) fn restart_to_update(&mut self, cx: &mut Context<Self>) {
        if let Status::Updated { restart, .. } = &self.update.status {
            cx.set_restart_path(restart.clone());
            cx.restart();
        }
    }
}

/// The status bar's update item: present only once there is something to
/// press.
///
/// Downloading and installing say nothing here, as in Zed: an update the user
/// has not asked for should not ask for attention until it is ready.
pub(crate) fn item(shell: &Luma) -> Option<crate::status_bar::Item> {
    let Status::Updated { version, .. } = &shell.update.status else {
        return None;
    };
    let mut item =
        crate::status_bar::Item::new("update-restart", format!("Restart to update to {version}"));
    item.icon = Some(IconName::RotateRight);
    item.press = Some(|this, cx| this.restart_to_update(cx));
    Some(item)
}

/// What the About section says about updates, and the restart button once
/// one is installed. There is no "Check for Updates": checking and downloading
/// happen on their own.
pub(crate) fn about(shell: &Luma, app: &Entity<Luma>) -> (Option<AnyElement>, Option<String>) {
    let status = &shell.update.status;
    let help = match (shell.update.updater.unavailable(), status) {
        (Some(reason), _) => Some(reason.to_string()),
        (None, Status::Checking) => Some("Checking for updates…".into()),
        (None, Status::Downloading { version }) => Some(format!("Downloading {version}…")),
        (None, Status::Installing { version }) => Some(format!("Installing {version}…")),
        (None, Status::Updated { version, .. }) => Some(format!(
            "Luma {version} is installed and runs after a restart."
        )),
        (None, Status::UpToDate) => Some("Luma is up to date.".into()),
        // The updater logged why; a person reads that it did not happen.
        (None, Status::Errored(_)) => Some("Could not check for updates.".into()),
        (None, Status::Idle) => None,
    };
    let button = matches!(status, Status::Updated { .. }).then(|| {
        let restart = app.clone();
        luma_ui::button("Restart to Update", Enabled::Yes)
            .id("settings-restart-to-update")
            .on_click(move |_, _, cx| restart.update(cx, |this, cx| this.restart_to_update(cx)))
            .agent_node(Role::Button, "Restart to Update")
            .into_any_element()
    });
    (button, help)
}

/// The ready dialog: what is ready, and the two answers.
pub(crate) fn render(
    state: &Ready,
    app: &Entity<Luma>,
    later_focus: &FocusHandle,
    later_focused: bool,
    relaunch_focus: &FocusHandle,
    relaunch_focused: bool,
) -> AnyElement {
    let title: SharedString = format!("Luma {} is ready", state.version).into();
    let body: SharedString =
        "The update has downloaded. Relaunch to use it now, or it starts the next time you open Luma."
            .into();
    let later = app.clone();
    let relaunch = app.clone();
    let card = div()
        .size_full()
        .flex()
        .flex_col()
        .justify_between()
        .p(px(20.))
        .gap(px(16.))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(
                    div()
                        .text_size(px(15.))
                        .text_color(glass::ink(0.95))
                        .child(title.clone())
                        .agent_node(Role::Text, title),
                )
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(glass::ink(0.6))
                        .child(body.clone())
                        .agent_node(Role::Text, body),
                ),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .justify_end()
                .gap(px(8.))
                .child(
                    float::btn("Later", "update-later")
                        .id("update-later")
                        .track_focus(later_focus)
                        .tab_stop(true)
                        .on_click(move |_, _, cx| {
                            later.update(cx, |this, cx| this.close_overlay(cx));
                        })
                        .agent_node(Role::Button, "Later")
                        .agent_focused(later_focused),
                )
                .child(
                    float::btn_primary("Relaunch now")
                        .id("update-relaunch")
                        .track_focus(relaunch_focus)
                        .tab_stop(true)
                        .on_click(move |_, _, cx| {
                            relaunch.update(cx, |this, cx| this.relaunch_from_dialog(cx));
                        })
                        .agent_node(Role::Button, "Relaunch now")
                        .agent_focused(relaunch_focused),
                ),
        );
    morph::fixed_card(
        "Update ready dialog",
        MorphSize::new(400.0, 176.0),
        card.into_any_element(),
    )
}
