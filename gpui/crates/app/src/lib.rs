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

use gpui::prelude::FluentBuilder as _;
use gpui::*;

mod agent;
mod chrome;
mod graph;
mod keymap;
mod library;
mod patterns;
mod settings;
mod shell;
mod tabs;
mod track_editor;
mod tracks;
mod visualizer;
mod welcome;

pub use chrome::hide_native_window_buttons;
pub use graph::ViewData;
pub use library::{Library, LibraryError};

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
}
use luma_chat::AgentChat;
use luma_ui::{fonts, ladder};

use shell::{Body, FocusSlot, Overlay};
use tabs::Tabs;

pub struct Luma {
    pub(crate) library: Library,
    /// The selected venue's track browser — the sidebar's body. `None` while
    /// no venue is selected, which is also when the venue picker overlay
    /// keeps itself open: the two states are one fact read twice.
    pub(crate) sidebar: Option<tracks::Tracks>,
    pub(crate) sidebar_hidden: bool,
    /// The workspace panel's open tabs. What each one shows *is* its identity
    /// — see [`tabs`].
    pub(crate) workspace: Tabs<Body>,
    pub(crate) workspace_hidden: bool,
    /// Whether an open workspace takes over everything right of the sidebar
    /// (this phase's default) or shares it with the thread column.
    pub(crate) expanded: bool,
    /// The one plane over the regions, or none — see [`shell::Overlay`].
    pub(crate) overlay: Option<Overlay>,
    /// The agent thread, the shell's centre. Built at first render (its
    /// composer needs a `Window`) and then kept for the app's life — it is a
    /// region, not a panel, and it cannot be closed.
    pub(crate) chat: Option<Entity<AgentChat>>,
    /// The keyboard's home: one handle, tracked at whichever element
    /// [`Luma::focus_slot`] names this frame, so actions always have a
    /// dispatch path and a binding can scope to the region it runs through.
    pub(crate) focus: FocusHandle,
    /// Which slot [`Self::focus`] was last taken for. A change of subject has
    /// to take the keyboard back; a field the user clicked into keeps it
    /// otherwise.
    pub(crate) focused_slot: FocusSlot,
}

impl Luma {
    /// The shell opens empty — no venue, no tabs — and the venue picker
    /// overlay opens itself over it. Every subject is reached by pressing
    /// something, which is what keeps "how did I get here" answerable from
    /// the click history alone.
    pub fn new(library: Library, cx: &mut Context<Self>) -> Self {
        let mut app = Self {
            library,
            sidebar: None,
            sidebar_hidden: false,
            workspace: Tabs::default(),
            workspace_hidden: false,
            expanded: true,
            overlay: None,
            chat: None,
            focus: cx.focus_handle(),
            focused_slot: FocusSlot::Shell,
        };
        app.show_venues(cx);
        app
    }

    /// Reveal the `index`th tab in strip order. ⌘1…⌘9; out of range is
    /// nothing, not a wrap.
    fn select_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        self.workspace.select_index(index);
        cx.notify();
    }

    /// The window title: the visible tab's subject, else the venue, else the
    /// app.
    fn title(&self) -> String {
        if let Some(Overlay::Settings(_)) = &self.overlay {
            return "Luma — Settings".to_string();
        }
        if let Some(Overlay::Patterns(_)) = &self.overlay {
            return "Luma — Patterns".to_string();
        }
        if let Some(body) = self.workspace.active_body() {
            return format!("Luma — {}", body.title());
        }
        match &self.sidebar {
            Some(browser) => format!("Luma — {}", browser.venue_name()),
            None => "Luma".to_string(),
        }
    }
}

impl Render for Luma {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The shell with nothing to show *is* the venue picker: keep the
        // overlay up while no venue is selected, nothing else is up, and no
        // tab is open. A workspace with tabs is a shell with something to
        // show — a pattern's graph needs no venue, and a picker that camped
        // over it would be the welcome screen refusing to leave.
        if self.sidebar.is_none() && self.overlay.is_none() && self.workspace.is_empty() {
            self.show_venues(cx);
        }
        self.sync_chat(window, cx);
        self.take_focus(window, cx);

        let title = self.title();
        let this = cx.entity();
        let root_holds_focus = self.overlay.is_none() && self.workspace.active().is_none();
        div()
            .size_full()
            .flex()
            .flex_col()
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
                cx.listener(|this, _: &keymap::OpenVisualizer, _, cx| this.open_visualizer(cx)),
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
            .child(chrome::titlebar(&title, move |_, cx| {
                this.update(cx, |this, cx| this.open_settings(cx));
            }))
            .child(shell::regions(self, window, cx))
    }
}
