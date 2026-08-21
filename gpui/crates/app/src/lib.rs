//! Luma, natively.
//!
//! A GPUI host over the same command surface the desktop app runs on: this
//! crate reads the real library through [`luma_lib::dispatch`] and renders it
//! with the design system in `luma-ui`.
//!
//! # Shape
//!
//! ```text
//! main         window + chrome, one root entity   (main.rs)
//!  └ Luma      which screen, and the data it is showing  (this file)
//!     ├ welcome    the venue grid
//!     ├ tracks     one venue's track table
//!     ├ patterns   the library's patterns
//!     ├ graph      one pattern's node graph, on a painted canvas
//!     ├ settings   the app's settings, over whichever screen opened them
//!     └ library    the only door to Luma's data
//! ```
//!
//! The view tree is a library rather than a binary's private module so that
//! `gpui-agent` can host the same [`Luma`] under a test platform. There is one
//! app: a harness that rebuilt these screens would be testing itself.
//!
//! [`Luma`] is the whole of the app's state: a screen and whatever that screen
//! has loaded. Navigation is a field assignment plus a spawned load — there is
//! no router, because a handful of screens do not need one, and a route table
//! that shipped before the destinations existed would be designed against a
//! guess. Each screen's transitions live with the screen (see
//! `settings::open_settings`), so this file stays the list of what exists.

use gpui::*;

mod agent;
mod chrome;
mod graph;
mod keymap;
mod library;
mod patterns;
mod settings;
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
use luma_lib::models::venues::Venue;
use luma_ui::{fonts, ladder};

/// Which screen is up, and what it has.
///
/// Loading is a variant of the data rather than a separate flag, so an
/// in-flight load cannot disagree with what is on screen: the fields are
/// replaced together when the load lands.
enum Screen {
    Welcome {
        venues: Vec<Venue>,
        error: Option<String>,
    },
    /// One venue's tracks. The variant carries the whole screen — its rows,
    /// its filters and its search — because those only mean anything together
    /// and only this screen reads them.
    Tracks(tracks::Tracks),
    /// Every pattern in the library. Not under a venue: `list_patterns` takes
    /// no venue, because a pattern belongs to the library and not to a room.
    Patterns(patterns::Patterns),
    /// One pattern's graph, on a canvas. Boxed because it is by far the
    /// largest screen — every other variant would otherwise pay its size.
    /// Carries the screen it was opened from — the patterns list, or a
    /// timeline whose clip was double-clicked — so Back restores that one
    /// whole, the same contract [`Screen::Settings`] keeps.
    Graph {
        state: Box<graph::Editor>,
        from: Box<Screen>,
    },
    /// One track's timeline: waveform, beat grid and clips, over the same
    /// transport the desktop app plays through. Boxed for the same reason as
    /// [`Screen::Graph`], and it carries the browser it was opened from so
    /// Back restores that venue's filters and search rather than re-running
    /// the query — settings does the same for the screen it covers.
    TrackEditor {
        state: Box<track_editor::Editor>,
        browser: Box<Screen>,
    },
    /// One venue's rig in 3D, over the screen it was opened from.
    ///
    /// The web has no visualizer *screen*: `<StageVisualizer>` is a pane four
    /// screens embed, and the closest of them is the track editor's centre
    /// pane. It is a screen here because the pane it would live in is one
    /// element away — `visualizer::visualizer` takes the state and a `Window`
    /// and nothing else — so the same module drops into that pane the day the
    /// timeline grows one, and until then this is the whole view rather than
    /// a third of it.
    Visualizer {
        state: Box<visualizer::Visualizer>,
        previous: Box<Screen>,
    },
    /// Settings sits *over* another screen rather than beside it — the web
    /// app opens it as a modal dialog — so it carries the screen it covers
    /// and Back restores that one whole, without re-running its load.
    Settings {
        state: settings::Settings,
        previous: Box<Screen>,
    },
}

impl Screen {
    /// The key context this screen's root declares, so a binding can mean one
    /// thing here and nothing on the screen next door. See [`keymap`].
    fn key_context(&self) -> &'static str {
        match self {
            Self::Welcome { .. } => keymap::context::WELCOME,
            Self::Tracks(_) => keymap::context::TRACKS,
            Self::Patterns(_) => keymap::context::PATTERNS,
            Self::Graph { .. } => keymap::context::GRAPH,
            Self::TrackEditor { .. } => keymap::context::TRACK_EDITOR,
            Self::Visualizer { .. } => keymap::context::VISUALIZER,
            Self::Settings { .. } => keymap::context::SETTINGS,
        }
    }
}

pub struct Luma {
    library: Library,
    screen: Screen,
    /// The agent chat, once it has been opened. Orthogonal to [`Self::screen`]
    /// rather than a variant of it: chat happens *over* whatever is showing,
    /// and closing it is a width of zero — see `agent`.
    chat: Option<Entity<AgentChat>>,
    /// The keyboard's home: the handle the screen root is drawn around, and
    /// the node every action is dispatched from.
    ///
    /// One handle for the app rather than one per screen — "which screen is
    /// focused" is not a question anything asks, and a handle per screen would
    /// be a second copy of [`Self::screen`] that could disagree with it.
    focus: FocusHandle,
    /// Which screen [`Self::focus`] was last taken for. A screen change has to
    /// take the keyboard back: the browser's search field is *kept* when the
    /// track editor opens over it, so its focus handle outlives the screen it
    /// belongs to and would otherwise still be swallowing the space bar from
    /// inside the editor.
    focused_screen: std::mem::Discriminant<Screen>,
}

impl Luma {
    /// The app opens on the venue grid, always. Every other screen is reached
    /// by pressing something on it — there is no second way in, which is what
    /// keeps "which screen am I on" answerable from the click history alone.
    pub fn new(library: Library, cx: &mut Context<Self>) -> Self {
        let screen = Screen::Welcome {
            venues: Vec::new(),
            error: None,
        };
        let mut app = Self {
            library,
            focused_screen: std::mem::discriminant(&screen),
            screen,
            chat: None,
            focus: cx.focus_handle(),
        };
        app.show_venues(cx);
        app
    }

    /// Show the venue grid and re-read it. The screen is replaced immediately
    /// so that leaving a venue cannot leave the old one on screen while the
    /// list loads.
    pub(crate) fn show_venues(&mut self, cx: &mut Context<Self>) {
        self.screen = Screen::Welcome {
            venues: Vec::new(),
            error: None,
        };
        cx.notify();
        let pending = self.library.venues();
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                this.screen = match result {
                    Ok(venues) => Screen::Welcome {
                        venues,
                        error: None,
                    },
                    Err(error) => Screen::Welcome {
                        venues: Vec::new(),
                        error: Some(error.to_string()),
                    },
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Leave for the screen this one was opened from. The one definition of
    /// what Back means: the key, the action and every screen's Back button all
    /// come through here, so they cannot come to disagree.
    ///
    /// The venue grid was not opened from anywhere, so Back there is nothing.
    pub(crate) fn back(&mut self, cx: &mut Context<Self>) {
        // Whatever the screen has open on top of itself goes first: Escape is
        // bound here, and a key that left the screen while a menu was still up
        // would be a key that skipped the thing the eye was looking at.
        if self.dismiss_insert_menu() {
            cx.notify();
            return;
        }
        match &self.screen {
            Screen::Welcome { .. } => {}
            Screen::Tracks(_) | Screen::Patterns(_) => self.show_venues(cx),
            Screen::Graph { .. } => self.close_graph(cx),
            Screen::TrackEditor { .. } => self.close_track_editor(cx),
            Screen::Visualizer { .. } => self.close_visualizer(cx),
            Screen::Settings { .. } => self.close_settings(cx),
        }
    }

    /// Give the keyboard to the screen that is up, so that actions dispatch
    /// along a path that runs through it and its bindings can be scoped to it.
    ///
    /// Done at draw rather than at every navigation because focusing needs a
    /// `&Window` and a navigation is a field assignment that has none — and
    /// because a screen that forgot to ask would then be a screen the keyboard
    /// does not reach. Focus is taken when the screen changed or when nothing
    /// holds it; a field the user clicked into keeps it otherwise.
    fn take_focus(&mut self, window: &mut Window, cx: &mut App) {
        let showing = std::mem::discriminant(&self.screen);
        if showing == self.focused_screen && window.focused(cx).is_some() {
            return;
        }
        self.focused_screen = showing;
        window.focus(&self.focus, cx);
    }

    /// The loaded venue a card click carries.
    fn find_venue(&self, id: &str) -> Option<Venue> {
        let Screen::Welcome { venues, .. } = &self.screen else {
            return None;
        };
        venues.iter().find(|venue| venue.id == id).cloned()
    }
}

impl Render for Luma {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.take_focus(window, cx);
        self.retire_agent_chat(window, cx);

        let title = match &self.screen {
            Screen::Welcome { .. } => "Luma".to_string(),
            Screen::Tracks(state) => format!("Luma — {}", state.venue_name()),
            Screen::Patterns(_) => "Luma — Patterns".to_string(),
            Screen::Graph { state, .. } => format!("Luma — {}", state.pattern_name()),
            Screen::TrackEditor { state, .. } => format!("Luma — {}", state.track_name()),
            Screen::Visualizer { state, .. } => format!("Luma — {}", state.venue_name()),
            Screen::Settings { .. } => "Luma — Settings".to_string(),
        };

        // Split the borrow rather than going through `&self.screen`: the 3D
        // view is the one screen whose element both mutates its state (a
        // lazily-acquired GPU, this frame's status) and reads the library
        // synchronously, and the two fields are disjoint.
        let entity = cx.entity();
        let Self {
            screen, library, ..
        } = self;
        let body = match screen {
            Screen::Welcome { venues, error } => {
                let opened = entity.clone();
                let patterns = entity.clone();
                welcome::welcome(
                    venues,
                    error.as_deref(),
                    move |id, _, cx| {
                        let id = id.to_string();
                        opened.update(cx, |this, cx| {
                            if let Some(venue) = this.find_venue(&id) {
                                this.open_venue(venue, cx);
                            }
                        });
                    },
                    move |_, cx| patterns.update(cx, |this, cx| this.show_patterns(cx)),
                )
                .into_any_element()
            }
            Screen::Tracks(state) => tracks::tracks(state, &entity, window).into_any_element(),
            Screen::Patterns(state) => patterns::patterns(state, &entity).into_any_element(),
            Screen::Graph { state, .. } => graph::graph(state, &entity).into_any_element(),
            Screen::TrackEditor { state, .. } => {
                track_editor::track_editor(state, &entity).into_any_element()
            }
            Screen::Visualizer { state, .. } => {
                visualizer::visualizer(state, &entity, library, window).into_any_element()
            }
            Screen::Settings { state, .. } => settings::settings(state, &entity).into_any_element(),
        };

        let this = cx.entity();
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(ladder::background())
            .font_family(fonts::FAMILY)
            .text_color(ladder::foreground())
            // The app's verbs, listened for above every screen: an action
            // dispatched at the focused screen root bubbles to here, and each
            // handler is a no-op on a screen it does not apply to.
            .key_context(keymap::context::ROOT)
            .on_action(cx.listener(|this, _: &keymap::Back, _, cx| this.back(cx)))
            .on_action(cx.listener(|this, _: &keymap::OpenSettings, _, cx| this.open_settings(cx)))
            .on_action(
                cx.listener(|this, _: &keymap::OpenVisualizer, _, cx| this.open_visualizer(cx)),
            )
            .on_action(cx.listener(|this, _: &keymap::PlayPause, _, cx| this.toggle_playback(cx)))
            .on_action(
                cx.listener(|this, _: &keymap::FollowPlayhead, _, cx| this.toggle_follow(cx)),
            )
            .on_action(
                cx.listener(|this, _: &keymap::ToggleAgentChat, window, cx| {
                    this.toggle_agent_chat(window, cx)
                }),
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
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_row()
                    .child(
                        div()
                            .flex_1()
                            .overflow_hidden()
                            .track_focus(&self.focus)
                            .key_context(self.screen.key_context())
                            .child(body),
                    )
                    .children(self.chat.clone()),
            )
    }
}
