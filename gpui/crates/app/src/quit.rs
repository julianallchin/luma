//! Quitting, and closing the window, which is the same thing: Luma has one
//! window, and gpui quits when it closes (`QuitMode::LastWindowClosed`).
//!
//! Every way out comes through [`Luma::request_quit`]: ⌘Q and the menu's
//! Quit, ⌘W and Close Window, and the window's own close button, which the
//! platform asks first through `on_window_should_close`. With nothing waiting
//! to upload the window closes at once. With local changes not yet on the
//! server it stays, and a dialog says so: while connected it shows them
//! draining and closes the window itself when the last one is up, or on
//! "Quit Now"; while offline nothing would drain, so it says the changes sync
//! next time and offers to quit.
//!
//! Leaving always writes first what was waiting on a debounce: which page each
//! venue is on, a camera, a sheet's trailing commit (see [`Luma::leave`]).
//!
//! What counts as unsynced is [`SyncStatus::unsynced`]'s, and the queue is
//! durable: quitting anyway loses nothing, it only leaves the work on this
//! machine until the next launch. The dialog is there so that leaving it
//! behind is a choice.
//!
//! Not covered: a quit macOS asks for itself (the Dock's Quit, logging out),
//! which gpui does not route through the app before terminating.

use gpui::prelude::*;
use gpui::{div, px, AnyElement, App, Entity, FocusHandle, Global, SharedString, WeakEntity};
use luma_lib::models::sync::SyncStatus;
use luma_ui::dialog::morph::{self, MorphSize};
use luma_ui::float;
use luma_ui::glass;
use luma_ui::node::{AgentNode as _, Instrument as _, Role};

use crate::shell::Overlay;
use crate::Luma;

/// The shell a quit is asked of. A global because ⌘Q is an app action that
/// must land when no window has focus, and because the close button's hook is
/// the platform's, not an element's.
struct Gate(WeakEntity<Luma>);

impl Global for Gate {}

/// The "Syncing your changes" dialog's state, in [`Overlay::QuitSync`].
#[derive(Default)]
pub(crate) struct QuitSync {
    /// The most changes seen waiting since the dialog opened: what the bar
    /// measures the drain against.
    peak: usize,
}

/// Ask the shell to quit, or quit if there is no shell to ask.
pub(crate) fn request(cx: &mut App) {
    let shell = cx.try_global::<Gate>().and_then(|gate| gate.0.upgrade());
    match shell {
        Some(shell) => shell.update(cx, |this, cx| this.request_quit(cx)),
        None => close_windows(cx),
    }
}

/// Close every window, which quits. On a later turn of the event loop, not
/// in this one: a quit is asked from inside a window's own dispatch or its
/// close hook, and the window must outlive the code that is asking.
fn close_windows(cx: &mut App) {
    cx.spawn(async move |cx| {
        cx.update(|cx| {
            for window in cx.windows() {
                window
                    .update(cx, |_, window, _| window.remove_window())
                    .ok();
            }
        });
    })
    .detach();
}

impl Luma {
    /// Make this shell the one quits are asked of, and route the window's
    /// close button through [`Self::request_quit`]. Once per window.
    pub(crate) fn guard_quit(&mut self, window: &mut gpui::Window, cx: &mut gpui::Context<Self>) {
        if self.quit_guarded {
            return;
        }
        self.quit_guarded = true;
        cx.set_global(Gate(cx.entity().downgrade()));
        let shell = cx.entity().downgrade();
        window.on_window_should_close(cx, move |_, cx| {
            // The window never closes from here: the request closes it itself
            // once it knows nothing would be left behind.
            shell
                .update(cx, |this, cx| this.request_quit(cx))
                .map(|_| false)
                .unwrap_or(true)
        });
    }

    /// Quit now if nothing is waiting to upload; otherwise raise the dialog.
    ///
    /// The status is read afresh: the bar's copy is up to a second old, and
    /// an edit made just before ⌘Q is exactly the one it would miss.
    pub(crate) fn request_quit(&mut self, cx: &mut gpui::Context<Self>) {
        if !self.library.cloud_sync_enabled() {
            return self.leave(cx);
        }
        if matches!(self.overlay.as_open(), Some(Overlay::QuitSync(_))) {
            return;
        }
        let status = self.library.sync_status();
        cx.spawn(async move |this, cx| {
            let status = status.await;
            this.update(cx, |this, cx| match status {
                Ok(status) if status.unsynced() > 0 => {
                    this.sync_status.status = status;
                    this.overlay.open(Overlay::QuitSync(QuitSync::default()));
                    this.quit_sync_changed(cx);
                    cx.notify();
                }
                // Nothing waiting, or no way to tell: a status that cannot
                // be read must not keep anyone from quitting.
                _ => this.leave(cx),
            })
            .ok();
        })
        .detach();
    }

    /// A new sync status, while the dialog is up: measure it, and quit once
    /// the last change is on the server.
    pub(crate) fn quit_sync_changed(&mut self, cx: &mut gpui::Context<Self>) {
        let left = self.sync_status.status.unsynced();
        let Some(Overlay::QuitSync(state)) = self.overlay.open_mut() else {
            return;
        };
        state.peak = state.peak.max(left);
        if left == 0 {
            self.leave(cx);
        }
    }

    /// "Quit Now" and "Quit": leave the changes for the next launch.
    fn quit_anyway(&mut self, cx: &mut gpui::Context<Self>) {
        self.leave(cx);
    }

    /// Write what is still waiting on a debounce, then close the window.
    ///
    /// The page each venue is on, a camera, a rebind (session items), a
    /// sheet's trailing commit and a dragged Perform color are all written a
    /// moment after they change, by a timer that dies with the shell. The
    /// library's runtime finishes writes already started (for up to its
    /// shutdown grace), so starting them here is what keeps them; the
    /// session items are waited for too, because they are the ones a
    /// relaunch reads first.
    fn leave(&mut self, cx: &mut gpui::Context<Self>) {
        self.flush_owed_sheets(cx);
        self.flush_owed_colors(cx);
        let writes = self.flush_session();
        cx.spawn(async move |_, cx| {
            for write in writes {
                write.await;
            }
            cx.update(close_windows);
        })
        .detach();
    }
}

/// Changes, counted: "1 change", "3 changes".
fn changes(n: usize) -> String {
    if n == 1 {
        "1 change".into()
    } else {
        format!("{n} changes")
    }
}

/// What the dialog says and offers: draining while connected, a promise for
/// next time while offline.
struct Words {
    title: SharedString,
    body: SharedString,
    quit: SharedString,
    /// How far the drain has got, while there is one.
    progress: Option<f32>,
}

fn words(state: &QuitSync, status: &SyncStatus) -> Words {
    let left = status.unsynced();
    if status.connected {
        let peak = state.peak.max(left).max(1);
        Words {
            title: "Syncing your changes".into(),
            body: format!("{} left. Luma quits when they are uploaded.", changes(left)).into(),
            quit: "Quit Now".into(),
            progress: Some(1.0 - left as f32 / peak as f32),
        }
    } else {
        Words {
            title: "You are offline".into(),
            body: format!("{} will sync the next time you open Luma.", changes(left)).into(),
            quit: "Quit".into(),
            progress: None,
        }
    }
}

/// The dialog's card.
pub(crate) fn render(
    state: &QuitSync,
    status: &SyncStatus,
    app: &Entity<Luma>,
    cancel_focus: &FocusHandle,
    cancel_focused: bool,
    quit_focus: &FocusHandle,
    quit_focused: bool,
) -> AnyElement {
    let words = words(state, status);
    let cancel = app.clone();
    let quit = app.clone();
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
                        .child(words.title.clone())
                        .agent_node(Role::Text, words.title),
                )
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(glass::ink(0.6))
                        .child(words.body.clone())
                        .agent_node(Role::Text, words.body),
                )
                .children(words.progress.map(|done| {
                    luma_ui::progress::bar(done).agent_node(
                        Role::Text,
                        format!("Uploaded {}%", (done * 100.0).round() as u32),
                    )
                })),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .justify_end()
                .gap(px(8.))
                .child(
                    float::btn("Cancel", "quit-sync-cancel")
                        .id("quit-sync-cancel")
                        .track_focus(cancel_focus)
                        .tab_stop(true)
                        .on_click(move |_, _, cx| {
                            cancel.update(cx, |this, cx| this.close_overlay(cx));
                        })
                        .agent_node(Role::Button, "Cancel")
                        .agent_focused(cancel_focused),
                )
                .child(
                    float::btn_primary(words.quit.clone())
                        .id("quit-sync-quit")
                        .track_focus(quit_focus)
                        .tab_stop(true)
                        .on_click(move |_, _, cx| {
                            quit.update(cx, |this, cx| this.quit_anyway(cx));
                        })
                        .agent_node(Role::Button, words.quit)
                        .agent_focused(quit_focused),
                ),
        );
    morph::fixed_card(
        "Quit sync dialog",
        MorphSize::new(400.0, 188.0),
        card.into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn waiting(connected: bool, pending: usize) -> SyncStatus {
        SyncStatus {
            connected,
            pending_uploads: pending,
            ..SyncStatus::default()
        }
    }

    /// The bar fills as the queue drains against the most it has held.
    #[test]
    fn the_bar_fills_as_the_queue_drains() {
        let state = QuitSync { peak: 4 };
        let at = |left| words(&state, &waiting(true, left)).progress.unwrap();
        assert!(at(4) < at(2));
        assert!(at(2) < at(0));
        assert_eq!(at(0), 1.0);
    }

    /// Offline, nothing would drain: no bar, and the answer is a plain Quit.
    #[test]
    fn offline_promises_next_time_and_offers_quit() {
        let offline = words(&QuitSync::default(), &waiting(false, 3));
        assert!(offline.progress.is_none());
        assert_eq!(offline.quit.as_ref(), "Quit");
        assert!(offline.body.contains("3 changes"));
    }
}
