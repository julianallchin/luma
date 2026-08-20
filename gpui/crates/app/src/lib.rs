//! Luma, natively.
//!
//! A GPUI host over the same command surface the desktop app runs on: this
//! crate reads the real library through [`luma_lib::dispatch`] and renders it
//! with the design system in `luma-ui`. v0 is read-only and two screens deep —
//! the venue grid and one venue's tracks.
//!
//! # Shape
//!
//! ```text
//! main         window + chrome, one root entity   (main.rs)
//!  └ Luma      which screen, and the data it is showing  (this file)
//!     ├ welcome   the venue grid
//!     ├ tracks    one venue's track table
//!     └ library   the only door to Luma's data
//! ```
//!
//! The view tree is a library rather than a binary's private module so that
//! `gpui-agent` can host the same [`Luma`] under a test platform. There is one
//! app: a harness that rebuilt these screens would be testing itself.
//!
//! [`Luma`] is the whole of the app's state: a screen and whatever that screen
//! has loaded. Navigation is a field assignment plus a spawned load — there is
//! no router, because two screens do not need one, and a router that shipped
//! before a third screen existed would be designed against a guess.

use gpui::*;

mod chrome;
mod library;
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
use luma_lib::models::tracks::TrackBrowserRow;
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
    Tracks {
        /// The venue's name, not the venue: the screen shows the name and the
        /// query it was opened with has already been issued, so carrying the
        /// whole record would be state with no reader.
        venue_name: String,
        rows: Vec<TrackBrowserRow>,
        error: Option<String>,
    },
}

pub struct Luma {
    library: Library,
    screen: Screen,
    /// A venue named on the command line, opened as soon as the venue list
    /// lands. Taken, not cloned — it applies once, at startup, and a later
    /// refresh must not yank the user back to it.
    open_on_start: Option<String>,
}

impl Luma {
    pub fn new(library: Library, open_on_start: Option<String>, cx: &mut Context<Self>) -> Self {
        let app = Self {
            library,
            screen: Screen::Welcome {
                venues: Vec::new(),
                error: None,
            },
            open_on_start,
        };
        app.load_venues(cx);
        app
    }

    fn load_venues(&self, cx: &mut Context<Self>) {
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
                if let Some(wanted) = this.open_on_start.take() {
                    if let Some(venue) = this.find_venue(&wanted) {
                        this.open_venue(venue, cx);
                    } else {
                        eprintln!("[luma] no venue matching `{wanted}`");
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// Navigate to a venue's tracks. The table renders immediately, empty,
    /// and fills in when the query lands — the venue is already known, so
    /// there is nothing to wait for before drawing the screen.
    fn open_venue(&mut self, venue: Venue, cx: &mut Context<Self>) {
        let pending = self.library.tracks(&venue.id);
        self.screen = Screen::Tracks {
            venue_name: venue.name,
            rows: Vec::new(),
            error: None,
        };
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = pending.await;
            this.update(cx, |this, cx| {
                if let Screen::Tracks { rows, error, .. } = &mut this.screen {
                    match result {
                        Ok(loaded) => *rows = loaded,
                        Err(message) => *error = Some(message),
                    }
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Look up a loaded venue by id, or failing that by name — the id is what
    /// a card click carries, the name is what a person types.
    fn find_venue(&self, needle: &str) -> Option<Venue> {
        let Screen::Welcome { venues, .. } = &self.screen else {
            return None;
        };
        venues
            .iter()
            .find(|venue| venue.id == needle)
            .or_else(|| {
                venues
                    .iter()
                    .find(|venue| venue.name.eq_ignore_ascii_case(needle))
            })
            .cloned()
    }
}

impl Render for Luma {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = match &self.screen {
            Screen::Welcome { .. } => "Luma".to_string(),
            Screen::Tracks { venue_name, .. } => format!("Luma — {venue_name}"),
        };

        let body = match &self.screen {
            Screen::Welcome { venues, error } => {
                let this = cx.entity();
                welcome::welcome(venues, error.as_deref(), move |id, _, cx| {
                    let id = id.to_string();
                    this.update(cx, |this, cx| {
                        if let Some(venue) = this.find_venue(&id) {
                            this.open_venue(venue, cx);
                        }
                    });
                })
                .into_any_element()
            }
            Screen::Tracks {
                venue_name,
                rows,
                error,
            } => {
                let this = cx.entity();
                tracks::tracks(venue_name, rows, error.as_deref(), move |_, cx| {
                    this.update(cx, |this, cx| this.load_venues(cx));
                })
                .into_any_element()
            }
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(ladder::background())
            .font_family(fonts::FAMILY)
            .text_color(ladder::foreground())
            .child(chrome::titlebar(&title))
            .child(div().flex_1().overflow_hidden().child(body))
    }
}
