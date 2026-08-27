//! Luma, natively.
//!
//! A GPUI host over the same command surface the desktop app runs on: this
//! crate reads the real library through [`luma_lib::dispatch`] and renders it
//! with the design system in `luma-ui`.
//!
//! # Shape
//!
//! ```text
//! main         window + chrome, one root entity      (main.rs)
//!  └ Luma      the persistent shell                  (this file + shell.rs)
//!     ├ sidebar    the selected venue's tracks       (tracks.rs)
//!     ├ chat       the agent thread, the centre      (luma-chat)
//!     ├ workspace  tabs: track editor, graph, 3D     (tabs.rs + shell.rs)
//!     ├ overlay    venues / patterns / settings      (shell.rs)
//!     └ library    the only door to Luma's data
//! ```
//!
//! The view tree is a library rather than a binary's private module so that
//! `gpui-agent` can host the same [`Luma`] under a test platform. There is one
//! app: a harness that rebuilt these screens would be testing itself.
//!
//! There is no router and no Back: the shell is persistent, opening one thing
//! never destroys another, and each gesture's transition lives with the module
//! it belongs to (see `settings::open_settings`), so this file stays the list
//! of what exists. `docs/specs/comet-shell.md` is the contract.

// The hitch report's self-describing schema is one `serde_json::json!`
// literal, and it grows a line every time the instrument learns to see
// something new. Each key costs a macro recursion; the default 128 is reached
// well before the schema stops being worth extending.
#![recursion_limit = "256"]

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use std::time::Duration;

mod add_tracks;
mod agent;
mod chat_history;
mod chrome;
mod graph;
mod history;
mod keymap;
mod library;
mod patterns;
mod settings;
mod shell;
mod subagents;
mod tab_chrome;
mod tabs;
mod track_editor;
mod tracks;
mod universe;
mod visualizer;
mod welcome;
mod workspace;

pub use chrome::hide_native_window_buttons;
pub use graph::ViewData;
#[cfg(feature = "agent")]
pub use library::NavigationFixture;
pub use library::{
    Library, LibraryError, SourceLibrary, SourcePlaylist, SourceTrack, TrackImportRequest,
    TrackSource,
};
#[cfg(feature = "agent")]
pub use library::{SourceAdapterFixture, SourceSearchFixtureResponse};
pub use luma_lib::models::tracks::{TrackImportPhase, TrackImportProgress, TrackImportResult};

/// Everything the app's views need present in an `App` before a window opens:
/// gpui-component's theme (every `Icon` reads it), Inter (not a system font,
/// so without it the text system silently picks another face), and the
/// keymap.
///
/// Both hosts call this — the real binary and the automation harness — so a
/// screen cannot render or answer a key differently depending on who opened
/// the window.
pub fn init(cx: &mut App) {
    gpui_component::init(cx);
    fonts::install(cx);
    keymap::init(cx);
    // Both halves of the text-field keymap, and they must be bound together:
    // the app's bindings exclude `TEXT_INPUT`, and the field's own supply the
    // editing keys that exclusion leaves it. Registering one without the other
    // gives a field that either eats the app's shortcuts or cannot be typed in.
    text_input::init(cx);
    motion::init(cx);
}
use luma_chat::AgentChat;
use luma_ui::{fonts, ladder, motion, text_input};

use shell::{Body, FocusSlot, Overlay};
use tabs::Tabs;

pub struct Luma {
    pub(crate) library: Library,
    pub(crate) track_import: Option<add_tracks::TrackImportActivity>,
    pub(crate) next_track_import: u64,
    /// The selected venue's track browser — the sidebar's body. `None` while
    /// no venue is selected, which is also when the venue picker overlay
    /// keeps itself open: the two states are one fact read twice.
    pub(crate) sidebar: Option<tracks::Tracks>,
    pub(crate) sidebar_hidden: bool,
    /// The sidebar's live width — the slide [`sidebar_hidden`](Self::sidebar_hidden)
    /// asks for. Intent and geometry are kept apart because only one of them
    /// is true mid-slide: the flag says where the region is going, this says
    /// where it is.
    pub(crate) sidebar_width: luma_ui::pane::PaneWidth,
    /// The workspace panel's open tabs — **the set on screen**. What each one
    /// shows *is* its identity, see [`tabs`].
    pub(crate) workspace: Tabs<Body>,
    /// Every other subject's remembered tabs. The strip belongs to whatever is
    /// picked in the sidebar, and [`Luma::sync_workspace_scope`] swaps this
    /// set for that one — see [`workspace`].
    pub(crate) parked: workspace::ParkedTabs<Body>,
    /// Visual-only state for keyed chip reflow and the floating `+` menu.
    /// Logical tab identity and teardown remain owned by `workspace`.
    pub(crate) tab_chrome: tab_chrome::TabChrome,
    /// The most recently opened subjects seed the `+` menu's editor choices.
    pub(crate) selected_track: Option<String>,
    pub(crate) selected_pattern: Option<luma_lib::models::patterns::PatternSummary>,
    pub(crate) workspace_hidden: bool,
    /// The workspace panel's live width — what the open/close toggle tweens.
    /// Where it rests when open is *derived* from the split below rather than
    /// stored, so the two can never disagree about how wide "open" is.
    pub(crate) workspace_width: luma_ui::pane::PaneWidth,
    /// How the room left of the panel divides between the thread and the panel.
    ///
    /// A proportion, not a width, so the sidebar opening or the window resizing
    /// takes its space from both in the ratio they were already at.
    /// Session-lived: a split chosen for one window is not a preference about
    /// every future window.
    pub(crate) workspace_split: luma_ui::split::SplitFraction,
    /// Whether an open workspace takes over everything right of the sidebar
    /// (this phase's default) or shares it with the thread column.
    pub(crate) expanded: bool,
    /// The stage above the editors, when the visible tab is about a room.
    ///
    /// Not a tab and not keyed like one: it is a *view of whatever is below
    /// it*, derived from the workspace every frame by
    /// [`Luma::sync_visualizer`]. `None` is both "no room to show" and the
    /// off switch for its redraw loop — see [`visualizer::visualizer`].
    pub(crate) visualizer: Option<visualizer::Visualizer>,
    /// Whether the stage pane is suppressed by hand. Kept apart from
    /// [`visualizer`](Self::visualizer) the way `sidebar_hidden` is from
    /// `sidebar`: one says what there is to show, the other whether the room
    /// for it was given away.
    pub(crate) visualizer_hidden: bool,
    /// How the workspace column divides between the stage and the editor.
    /// Session-lived, like `workspace_split`: a split chosen for one window is
    /// not a preference about every future window.
    pub(crate) visualizer_split: luma_ui::split::SplitFraction,
    /// The one plane over the regions, or none — see [`shell::Overlay`].
    /// The dialog on screen, and — for the frames after it is dismissed —
    /// the one leaving. See [`luma_ui::dialog::Popup`]: gpui unmounts an
    /// element the frame its state drops, so an exit animation needs the state
    /// held alive while it plays.
    pub(crate) overlay: luma_ui::dialog::Popup<Overlay>,
    /// The agent thread, the shell's centre. Built at first render (its
    /// composer needs a `Window`) and then kept for the app's life — it is a
    /// region, not a panel, and it cannot be closed.
    pub(crate) chat: Option<Entity<AgentChat>>,
    /// Alive exactly as long as the chat it listens to — see `sync_chat`.
    pub(crate) chat_subscription: Option<gpui::Subscription>,
    /// The keyboard's home: one handle, tracked at whichever element
    /// [`Luma::focus_slot`] names this frame, so actions always have a
    /// dispatch path and a binding can scope to the region it runs through.
    pub(crate) focus: FocusHandle,
    /// The modal plane's own focus target. Keeping this distinct from the
    /// shell handle lets dismissal return to the exact field/button that
    /// opened the dialog instead of merely focusing its containing region.
    pub(crate) dialog_focus: FocusHandle,
    /// Stable handle for the first reachable dialog control. Besides making
    /// initial traversal deterministic, this lets the harness prove the trap
    /// moves and wraps rather than merely retaining the container focus.
    pub(crate) dialog_first_focus: FocusHandle,
    pub(crate) dialog_last_focus: FocusHandle,
    pub(crate) overlay_return_focus: Option<WeakFocusHandle>,
    /// Which slot [`Self::focus`] was last taken for. A change of subject has
    /// to take the keyboard back; a field the user clicked into keeps it
    /// otherwise.
    pub(crate) focused_slot: FocusSlot,
    /// Correlates venue catalogue reloads and startup restoration. An answer
    /// belongs to the picker instance that requested it, never merely to
    /// whichever venue overlay happens to be visible when it lands.
    pub(crate) venue_picker_generation: u64,
    /// Correlates a history read with the dialog that asked for it — a slow
    /// list arriving after the reader reopened the picker is a stale answer.
    pub(crate) chat_history_generation: u64,
    /// Correlates per-venue track reads. Venue identity is checked as well;
    /// the generation distinguishes reopening the same venue twice.
    pub(crate) venue_selection_generation: u64,
}

impl Luma {
    /// The shell opens empty — no venue, no tabs — and the venue picker
    /// overlay opens itself over it. Every subject is reached by pressing
    /// something, which is what keeps "how did I get here" answerable from
    /// the click history alone.
    pub fn new(library: Library, cx: &mut Context<Self>) -> Self {
        let mut app = Self {
            library,
            track_import: None,
            next_track_import: 0,
            sidebar: None,
            sidebar_hidden: false,
            // Both regions start closed and slide open when they first have
            // something to show, so first paint is one gesture rather than a
            // window that assembles itself.
            sidebar_width: luma_ui::pane::PaneWidth::new(0.0),
            workspace: Tabs::default(),
            parked: workspace::ParkedTabs::default(),
            tab_chrome: tab_chrome::TabChrome::default(),
            selected_track: None,
            selected_pattern: None,
            workspace_hidden: false,
            workspace_width: luma_ui::pane::PaneWidth::new(0.0),
            workspace_split: shell::workspace_split(),
            expanded: false,
            visualizer: None,
            visualizer_hidden: false,
            visualizer_split: shell::visualizer_split(),
            overlay: luma_ui::dialog::Popup::default(),
            chat: None,
            chat_subscription: None,
            chat_history_generation: 0,
            focus: cx.focus_handle(),
            dialog_focus: cx.focus_handle(),
            // Explicit tracked handles keep their own tab-stop bit in current
            // GPUI; the element's `.tab_stop(true)` only decorates implicitly
            // created handles. Mark these stable sentinels at their owner.
            dialog_first_focus: cx.focus_handle().tab_stop(true),
            dialog_last_focus: cx.focus_handle().tab_stop(true),
            overlay_return_focus: None,
            focused_slot: FocusSlot::Shell,
            venue_picker_generation: 0,
            venue_selection_generation: 0,
        };
        app.restore_venue(cx);
        app.auto_repro(cx);
        app
    }

    /// Drive the app into one reproduction state at launch.
    ///
    /// **A diagnostic instrument, not a feature, and env-gated so it does not
    /// exist unless asked for.** `main` deliberately takes no flags because
    /// every screen should be reachable by pressing something — this does not
    /// change that. It exists because one bug appears only in a real window
    /// presenting real surfaces to the compositor, which is exactly the
    /// configuration the offscreen harness cannot produce, and reaching the
    /// state by hand every time is how a measurement goes unrepeated.
    ///
    /// `LUMA_AUTOREPRO` is a substring of the track title to open;
    /// `LUMA_AUTOREPRO_ZOOM` is how many dolly steps to take once the stage is
    /// live (default 8, which reaches the near clamp on any rig).
    ///
    /// Polls rather than chains callbacks: the venue restore and the track
    /// open are each several awaited reads, and a poll that gives up after a
    /// bounded number of tries cannot hang a launch that has nothing to open.
    fn auto_repro(&mut self, cx: &mut Context<Self>) {
        let Ok(wanted) = std::env::var("LUMA_AUTOREPRO") else {
            return;
        };
        let steps = std::env::var("LUMA_AUTOREPRO_ZOOM")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(8);
        let tick = || self.library.transport_after(Duration::from_millis(250));
        let mut pending = tick();
        cx.spawn(async move |this, cx| {
            let mut opened = false;
            for _ in 0..240 {
                let _ = pending.await;
                let outcome = this.update(cx, |this, cx| {
                    if !opened {
                        let found = this
                            .sidebar
                            .as_ref()
                            .and_then(|browser| browser.find_titled(&wanted));
                        if let Some(row) = found {
                            this.open_track(&row.id, cx);
                            opened = true;
                        }
                        return false;
                    }
                    // Only once the stage exists; before that there is no
                    // camera to move and the gesture would be lost.
                    match this.visualizer_mut() {
                        Some(stage) => {
                            stage.dolly_in(steps);
                            cx.notify();
                            // Seek into lit material and start the transport:
                            // a stage with nothing lit is not the state the
                            // report is about, and t=0 on this score is dark.
                            let seek = std::env::var("LUMA_AUTOREPRO_SEEK")
                                .ok()
                                .and_then(|value| value.parse().ok())
                                .unwrap_or(50.0);
                            drop(this.library.seek(seek));
                            drop(this.library.play());
                            true
                        }
                        None => false,
                    }
                });
                match outcome {
                    Ok(true) | Err(_) => return,
                    Ok(false) => {}
                }
                let next = this.read_with(cx, |this, _| {
                    this.library.transport_after(Duration::from_millis(250))
                });
                match next {
                    Ok(next) => pending = next,
                    Err(_) => return,
                }
            }
        })
        .detach();
    }

    /// Reveal the `index`th tab in strip order. ⌘1…⌘9; out of range is
    /// nothing, not a wrap.
    fn select_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        self.workspace.select_index(index);
        cx.notify();
    }
}

impl Render for Luma {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The shell with nothing to show *is* the venue picker: keep the
        // overlay up while no venue is selected, nothing else is up, and no
        // tab is open. A workspace with tabs is a shell with something to
        // show — a pattern's graph needs no venue, and a picker that camped
        // over it would be the welcome screen refusing to leave.
        if self.sidebar.is_none() && self.overlay.get().is_none() && self.workspace.is_empty() {
            self.show_venues(cx);
        }
        self.sync_workspace_scope(cx);
        self.sync_chat(window, cx);
        self.sync_visualizer(cx);
        self.take_focus(window, cx);

        let root_holds_focus = matches!(self.focus_slot(), FocusSlot::Shell);
        let root = div()
            .size_full()
            .flex()
            .flex_col()
            // The floor. Every region either leaves it showing (the thread
            // column, the workspace panel) or raises itself off it (the
            // sidebar) — see `shell::regions`. Opaque, and from the ladder
            // rather than the glass tier, because a plane whose brightness is
            // decided by the desktop behind the window has no rung.
            .bg(ladder::background())
            .font_family(fonts::FAMILY)
            .text_color(ladder::foreground())
            .when(root_holds_focus, |root| root.track_focus(&self.focus))
            // The app's verbs, listened for above every region: an action
            // dispatched at the focused element bubbles to here, and each
            // handler is a no-op wherever it does not apply.
            .key_context(keymap::context::ROOT)
            .on_action(
                cx.listener(|this, _: &keymap::DismissOverlay, _, cx| this.dismiss_overlay(cx)),
            )
            .on_action(cx.listener(|this, _: &keymap::OpenSettings, _, cx| this.open_settings(cx)))
            .on_action(cx.listener(|this, _: &keymap::OpenPatterns, _, cx| this.show_patterns(cx)))
            .on_action(
                cx.listener(|this, _: &keymap::ToggleVisualizer, _, cx| this.toggle_visualizer(cx)),
            )
            .on_action(cx.listener(|this, _: &keymap::ToggleSidebar, _, cx| {
                this.sidebar_hidden = !this.sidebar_hidden;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &keymap::ToggleWorkspace, _, cx| {
                this.workspace_hidden = !this.workspace_hidden;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &keymap::ToggleExpand, _, cx| {
                this.expanded = !this.expanded;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &keymap::CloseTab, _, cx| this.close_active_tab(cx)))
            .on_action(cx.listener(|this, _: &keymap::NewTab, _, cx| {
                // ⌘T means "show me the ways to open a tab", and where those
                // live depends on whether any exist yet: the `+` menu hangs
                // off the strip, and with no tabs there is no strip to hang
                // it off — the panel's own empty state is the offer instead.
                // Either way the panel comes up, because both live inside it.
                this.workspace_hidden = false;
                if !this.workspace.is_empty() {
                    this.tab_chrome.toggle_menu();
                }
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &keymap::SelectTab1, _, cx| this.select_tab(0, cx)))
            .on_action(cx.listener(|this, _: &keymap::SelectTab2, _, cx| this.select_tab(1, cx)))
            .on_action(cx.listener(|this, _: &keymap::SelectTab3, _, cx| this.select_tab(2, cx)))
            .on_action(cx.listener(|this, _: &keymap::SelectTab4, _, cx| this.select_tab(3, cx)))
            .on_action(cx.listener(|this, _: &keymap::SelectTab5, _, cx| this.select_tab(4, cx)))
            .on_action(cx.listener(|this, _: &keymap::SelectTab6, _, cx| this.select_tab(5, cx)))
            .on_action(cx.listener(|this, _: &keymap::SelectTab7, _, cx| this.select_tab(6, cx)))
            .on_action(cx.listener(|this, _: &keymap::SelectTab8, _, cx| this.select_tab(7, cx)))
            .on_action(cx.listener(|this, _: &keymap::SelectTab9, _, cx| this.select_tab(8, cx)))
            .on_action(cx.listener(|this, _: &keymap::PlayPause, _, cx| this.toggle_playback(cx)))
            .on_action(
                cx.listener(|this, _: &keymap::FollowPlayhead, _, cx| this.toggle_follow(cx)),
            )
            .on_action(cx.listener(|this, _: &keymap::UndoClips, _, cx| this.undo_clips(cx)))
            .on_action(cx.listener(|this, _: &keymap::RedoClips, _, cx| this.redo_clips(cx)))
            .on_action(cx.listener(|this, _: &keymap::DeleteNodes, _, cx| this.graph_delete(cx)))
            .on_action(cx.listener(|this, _: &keymap::UndoGraph, _, cx| this.graph_undo(cx)))
            .on_action(cx.listener(|this, _: &keymap::RedoGraph, _, cx| this.graph_redo(cx)))
            .on_action(
                cx.listener(|this, _: &keymap::ToggleLoopRegion, _, cx| {
                    this.toggle_loop_region(cx)
                }),
            )
            .on_action(cx.listener(|this, _: &keymap::DeleteClips, _, cx| this.delete_clips(cx)))
            .on_action(cx.listener(|this, _: &keymap::SplitClips, _, cx| this.split_clips(cx)))
            .on_action(cx.listener(|this, _: &keymap::CopyClips, _, cx| this.copy_clips(cx)))
            .on_action(cx.listener(|this, _: &keymap::CutClips, _, cx| this.cut_clips(cx)))
            .on_action(cx.listener(|this, _: &keymap::PasteClips, _, cx| this.paste_clips(cx)))
            .on_action(
                cx.listener(|this, _: &keymap::DuplicateClips, _, cx| this.duplicate_clips(cx)),
            )
            .on_action(
                cx.listener(|this, _: &keymap::MoveClipsUp, _, cx| this.move_clips_lane(false, cx)),
            )
            .on_action(
                cx.listener(|this, _: &keymap::MoveClipsDown, _, cx| {
                    this.move_clips_lane(true, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &keymap::FitLanes, _, cx| this.fit_lanes(cx)))
            .on_action(cx.listener(|this, _: &keymap::NextInsertOption, _, cx| {
                this.step_insert_menu(true, cx)
            }))
            .on_action(cx.listener(|this, _: &keymap::PrevInsertOption, _, cx| {
                this.step_insert_menu(false, cx)
            }))
            .on_action(cx.listener(|this, _: &keymap::CommitInsertOption, _, cx| {
                this.commit_insert_menu(cx)
            }))
            .child(shell::regions(self, window, cx));
        // The once-per-frame hover tick, at the tail so it runs after every
        // row above has read its blend. Without it a hover wash is evaluated
        // exactly once — on the frame the pointer's own `refresh` produced —
        // and then freezes part-way until something unrelated invalidates the
        // window. That is what "hover feels laggy" is, and it is also why a
        // wash could appear to stop halfway through.
        //
        // The tick doubles as the staleness sweep: a row that unmounts mid-
        // hover never gets its leave event, and this is what drops its entry.
        if motion::hover_fades_active() {
            window.request_animation_frame();
        }
        root
    }
}
