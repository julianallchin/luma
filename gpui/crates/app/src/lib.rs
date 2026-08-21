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

mod chrome;
mod graph;
mod library;
mod patterns;
mod settings;
mod tracks;
mod welcome;

pub use library::Library;

/// Everything the app's views need present in an `App` before a window opens:
/// gpui-component's theme (every `Icon` reads it) and Inter (not a system
/// font, so without it the text system silently picks another face).
///
/// Both hosts call this — the real binary and the automation harness — so a
/// screen cannot render differently depending on who opened the window.
pub fn init(cx: &mut App) {
    gpui_component::init(cx);
    fonts::install(cx);
}
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
    Graph(Box<graph::Editor>),
    /// Settings sits *over* another screen rather than beside it — the web
    /// app opens it as a modal dialog — so it carries the screen it covers
    /// and Back restores that one whole, without re-running its load.
    Settings {
        state: settings::Settings,
        previous: Box<Screen>,
    },
}

pub struct Luma {
    library: Library,
    screen: Screen,
}

impl Luma {
    /// The app opens on the venue grid, always. Every other screen is reached
    /// by pressing something on it — there is no second way in, which is what
    /// keeps "which screen am I on" answerable from the click history alone.
    pub fn new(library: Library, cx: &mut Context<Self>) -> Self {
        let mut app = Self {
            library,
            screen: Screen::Welcome {
                venues: Vec::new(),
                error: None,
            },
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
                        error: Some(error),
                    },
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
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
        let title = match &self.screen {
            Screen::Welcome { .. } => "Luma".to_string(),
            Screen::Tracks(state) => format!("Luma — {}", state.venue_name()),
            Screen::Patterns(_) => "Luma — Patterns".to_string(),
            Screen::Graph(state) => format!("Luma — {}", state.pattern_name()),
            Screen::Settings { .. } => "Luma — Settings".to_string(),
        };

        let body = match &self.screen {
            Screen::Welcome { venues, error } => {
                let opened = cx.entity();
                let patterns = cx.entity();
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
            Screen::Tracks(state) => tracks::tracks(state, &cx.entity(), window).into_any_element(),
            Screen::Patterns(state) => patterns::patterns(state, &cx.entity()).into_any_element(),
            Screen::Graph(state) => graph::graph(state, &cx.entity()).into_any_element(),
            Screen::Settings { state, .. } => {
                settings::settings(state, &cx.entity()).into_any_element()
            }
        };

        let this = cx.entity();
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(ladder::background())
            .font_family(fonts::FAMILY)
            .text_color(ladder::foreground())
            .child(chrome::titlebar(&title, move |_, cx| {
                this.update(cx, |this, cx| this.open_settings(cx));
            }))
            .child(div().flex_1().overflow_hidden().child(body))
    }
}
