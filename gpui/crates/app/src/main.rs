//! Luma, natively.
//!
//! A GPUI host over the same command surface the desktop app runs on: this
//! binary reads the real library through [`luma_lib::dispatch`] and renders it
//! with the design system in `luma-ui`. v0 is read-only and two screens deep —
//! the venue grid and one venue's tracks.
//!
//! # Shape
//!
//! ```text
//! main         window + chrome, one root entity
//!  └ Luma      which screen, and the data it is showing  (this file)
//!     ├ welcome   the venue grid
//!     ├ tracks    one venue's track table
//!     └ library   the only door to Luma's data
//! ```
//!
//! [`Luma`] is the whole of the app's state: a screen and whatever that screen
//! has loaded. Navigation is a field assignment plus a spawned load — there is
//! no router, because two screens do not need one, and a router that shipped
//! before a third screen existed would be designed against a guess.

use gpui::*;
use gpui_component::Root;

mod chrome;
mod library;
mod tracks;
mod welcome;

use library::Library;
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

struct Luma {
    library: Library,
    screen: Screen,
    /// A venue named on the command line, opened as soon as the venue list
    /// lands. Taken, not cloned — it applies once, at startup, and a later
    /// refresh must not yank the user back to it.
    open_on_start: Option<String>,
}

impl Luma {
    fn new(library: Library, open_on_start: Option<String>, cx: &mut Context<Self>) -> Self {
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

fn main() {
    // `luma-app [--venue <name|id>]` — opening straight to a venue is how the
    // track browser is reachable without a pointer, which a headless capture
    // or a shell alias both want.
    let mut args = std::env::args().skip(1);
    let mut open_on_start = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--venue" => open_on_start = args.next(),
            other => {
                eprintln!("usage: luma-app [--venue <name|id>]  (unexpected `{other}`)");
                std::process::exit(2);
            }
        }
    }

    let library = match Library::open() {
        Ok(library) => library,
        Err(error) => {
            eprintln!("[luma] could not open the library: {error}");
            std::process::exit(1);
        }
    };

    // Icons are SVGs embedded by gpui-component's assets crate; without an
    // asset source every `Icon` silently renders nothing.
    let app = gpui_platform::application().with_assets(gpui_component_assets::Assets);
    app.run(move |cx| {
        gpui_component::init(cx);
        fonts::install(cx);

        let options = WindowOptions {
            // No native chrome anywhere: `chrome::titlebar` draws it, the same
            // choice `decorations: false` makes for the Tauri window.
            titlebar: None,
            window_decorations: Some(WindowDecorations::Client),
            app_owns_titlebar_drag: true,
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: point(px(120.), px(120.)),
                size: size(px(1200.), px(800.)),
            })),
            ..Default::default()
        };

        cx.open_window(options, |window, cx| {
            let luma = cx.new(|cx| Luma::new(library, open_on_start, cx));
            cx.new(|cx| Root::new(luma, window, cx).bordered(false))
        })
        .expect("failed to open the Luma window");
        cx.activate(true);
    });
}
