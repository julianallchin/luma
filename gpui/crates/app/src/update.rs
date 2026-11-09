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
//!
//! The dialog lists what the new version brings, from the manifest's
//! changelog ([`luma_update::manifest::Manifest::changes_since`]): every
//! release since the running one, so a skipped release is not lost.

use std::sync::Arc;
use std::time::Duration;

use gpui::*;
use luma_ui::dialog::morph::{self, MorphSize};
use luma_ui::float;
use luma_ui::glass;
use luma_ui::icons::IconName;
use luma_ui::node::{AgentNode, Instrument, Role};
use luma_ui::Enabled;
use luma_update::manifest::Change;
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
    /// By release, newest first: see `Manifest::changes_since`.
    changes: Vec<Change>,
}

/// The most changelog lines the dialog lists before "and N more".
const SHOWN_CHANGES: usize = 8;

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

    /// The menu's "Check for Updates…": ask now, and show the answer where
    /// the About section says it.
    pub(crate) fn check_for_updates(&mut self, cx: &mut Context<Self>) {
        self.update.updater.check(true);
        self.open_about(cx);
    }

    /// Raise the ready dialog the first time a version is on disk. A dialog
    /// already up (or still playing its exit) is left alone; the shell calls
    /// this again each frame, so the ready dialog follows once it has gone.
    /// The venue picker is up at every launch, and an update that landed
    /// under it would otherwise never be announced.
    pub(crate) fn announce_update(&mut self, cx: &mut Context<Self>) {
        let Status::Updated {
            version, changes, ..
        } = &self.update.status
        else {
            return;
        };
        if self.update.announced.as_ref() == Some(version) || self.overlay.get().is_some() {
            return;
        }
        self.update.announced = Some(version.clone());
        let changes = changes.clone();
        self.overlay
            .open(crate::shell::Overlay::UpdateReady(Ready { changes }));
        cx.notify();
    }

    /// Behave as if `version` had just installed, bringing `changes` (each
    /// release's version and lines, newest first): the harness's way to the
    /// ready dialog, since a source build never updates.
    pub fn update_ready_fixture(
        &mut self,
        version: String,
        changes: Vec<(String, Vec<String>)>,
        cx: &mut Context<Self>,
    ) {
        let changes = changes
            .into_iter()
            .map(|(version, lines)| Change {
                version,
                commit: None,
                lines,
            })
            .collect();
        self.update.status = Status::Updated {
            version,
            restart: std::path::PathBuf::new(),
            changes,
        };
        self.announce_update(cx);
    }

    /// The dialog's "Relaunch".
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
/// one is installed. Checking and downloading happen on their own; the
/// menu's "Check for Updates…" only asks sooner.
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

/// The ready dialog: the prompt, what the update brings by release, and the
/// two answers.
pub(crate) fn render(
    state: &Ready,
    app: &Entity<Luma>,
    later_focus: &FocusHandle,
    later_focused: bool,
    relaunch_focus: &FocusHandle,
    relaunch_focused: bool,
) -> AnyElement {
    let title: SharedString = "New version ready".into();
    let (groups, more) = shown(&state.changes);
    let more: Option<SharedString> = (more > 0).then(|| format!("and {more} more").into());
    let lines =
        groups.iter().map(|(_, lines)| lines.len()).sum::<usize>() + usize::from(more.is_some());
    let size = MorphSize::new(WIDTH, height(groups.len(), lines));
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
                .gap(px(LIST_GAP))
                .child(
                    div()
                        .text_size(px(15.))
                        .line_height(px(TITLE_LINE))
                        .text_color(glass::ink(0.95))
                        .child(title.clone())
                        .agent_node(Role::Text, title),
                )
                .children(groups.into_iter().flat_map(|(version, lines)| {
                    std::iter::once(header(version).into_any_element()).chain(
                        lines
                            .into_iter()
                            .map(|line| change(line).into_any_element()),
                    )
                }))
                .children(more.map(|more| {
                    div()
                        .text_size(px(13.))
                        .line_height(px(LINE))
                        .text_color(glass::ink(0.45))
                        .child(more.clone())
                        .agent_node(Role::Text, more)
                })),
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
                    float::btn_primary("Relaunch")
                        .id("update-relaunch")
                        .track_focus(relaunch_focus)
                        .tab_stop(true)
                        .on_click(move |_, _, cx| {
                            relaunch.update(cx, |this, cx| this.relaunch_from_dialog(cx));
                        })
                        .agent_node(Role::Button, "Relaunch")
                        .agent_focused(relaunch_focused),
                ),
        );
    morph::fixed_card("Update ready dialog", size, card.into_any_element())
}

/// The releases' lines, [`SHOWN_CHANGES`] in all, each release's under its
/// version; and how many lines did not fit.
fn shown(changes: &[Change]) -> (Vec<(SharedString, Vec<SharedString>)>, usize) {
    let mut room = SHOWN_CHANGES;
    let mut more = 0;
    let mut groups = Vec::new();
    for change in changes {
        let take = change.lines.len().min(room);
        room -= take;
        more += change.lines.len() - take;
        if take > 0 {
            let lines = change.lines[..take]
                .iter()
                .cloned()
                .map(Into::into)
                .collect();
            groups.push((SharedString::from(change.label().to_string()), lines));
        }
    }
    (groups, more)
}

/// A release's version, small, above its lines.
fn header(version: SharedString) -> impl IntoElement {
    div()
        .pt(px(HEADER_GAP))
        .text_size(px(11.))
        .line_height(px(LINE))
        .text_color(glass::ink(0.45))
        .child(version.clone())
        .agent_node(Role::Text, version)
}

/// One changelog line: a dot and the words, on one line. The lines are at
/// most eight words, so the ellipsis is a guard rather than the rule.
fn change(line: SharedString) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(BULLET_INDENT - BULLET))
        .child(
            div()
                .flex_none()
                .size(px(BULLET))
                .rounded_full()
                .bg(glass::ink(0.4)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(px(13.))
                .line_height(px(LINE))
                .text_color(glass::ink(0.7))
                .child(line.clone()),
        )
        .agent_node(Role::Text, line)
}

const WIDTH: f32 = 400.0;
const TITLE_LINE: f32 = 20.0;
/// Every row under the title is one line this tall.
const LINE: f32 = 18.0;
const LIST_GAP: f32 = 4.0;
/// Extra room above a version, so it reads as the start of a group.
const HEADER_GAP: f32 = 6.0;
const BULLET: f32 = 4.0;
const BULLET_INDENT: f32 = 14.0;

/// The card's height for `headers` versions and `lines` rows of text under
/// them, which it fits exactly: every row is one line.
fn height(headers: usize, lines: usize) -> f32 {
    // Padding, the title, the gap above the buttons, and the buttons.
    const PROMPT: f32 = 20.0 + TITLE_LINE + 16.0 + 32.0 + 20.0;
    let rows = (headers + lines) as f32;
    PROMPT + rows * (LIST_GAP + LINE) + headers as f32 * HEADER_GAP
}
