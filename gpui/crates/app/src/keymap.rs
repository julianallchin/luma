//! What the keyboard can ask for, and which keys ask for it.
//!
//! An action is the app's verb; a key is one way to say it and the automation
//! harness's `app.action("luma::PlayPause")` is another. Both land on the same
//! listener in [`crate::Luma::render`], so a script drives the app a person
//! could have driven, and neither can drift from the other.
//!
//! # Where a binding fires
//!
//! Scope comes from *key context*, not from a screen check inside the
//! handler: `Luma::render` declares [`context::ROOT`] at the window root and
//! the screen's own context at the focused screen root, so `space` can mean
//! playback in the track editor and mean nothing at all on the venue grid.
//!
//! # Why a text field can keep the space bar
//!
//! gpui matches bindings *before* it delivers key events, so a focused field
//! cannot out-run a binding by consuming the keystroke — by the time its
//! `on_key_down` runs, the action has already fired. The exclusion therefore
//! belongs on the binding: every binding whose key is a character a person
//! could be typing carries `&& !`[`context::TEXT_INPUT`], a predicate gpui
//! evaluates over the *whole* focus path. One rule, stated once, and it is
//! what keeps the browser's spacebar a space and its escape a cleared query.

use gpui::{actions, App, KeyBinding};

/// The key contexts a dispatch path can carry: the window's, each screen's,
/// and one for a field that is taking typed text.
///
/// They live together because these names are only ever half of a pair — a
/// binding predicate here and a `key_context` on an element there — and a
/// name spelled differently in the two places is a binding that silently
/// never fires.
pub(crate) mod context {
    /// The window root. Everything Luma renders is under it, so a binding
    /// scoped here is app-wide without being global.
    pub const ROOT: &str = "Luma";
    pub const WELCOME: &str = "Welcome";
    pub const TRACKS: &str = "Tracks";
    pub const PATTERNS: &str = "Patterns";
    pub const GRAPH: &str = "Graph";
    pub const TRACK_EDITOR: &str = "TrackEditor";
    pub const SETTINGS: &str = "Settings";
    /// Declared by a focused field that is taking typed text. Any binding on
    /// a key that field could be typing excludes it.
    pub const TEXT_INPUT: &str = "TextInput";
}

actions!(
    luma,
    [
        /// Start or stop the track editor's transport.
        PlayPause,
        /// Leave the screen for the one it was opened from. A no-op on the
        /// venue grid, which was not opened from anywhere.
        Back,
        /// Open settings over whatever is showing.
        OpenSettings,
    ]
);

/// Bind the keys. Called from [`crate::init`], so the real binary and the
/// harness get the same keyboard.
pub(crate) fn init(cx: &mut App) {
    // `secondary-` is cmd on macOS and ctrl elsewhere: gpui resolves it per
    // platform, which is why there is no `cfg` here.
    let escape = format!("{} && !{}", context::ROOT, context::TEXT_INPUT);
    let play_pause = format!("{} && !{}", context::TRACK_EDITOR, context::TEXT_INPUT);
    cx.bind_keys([
        KeyBinding::new("space", PlayPause, Some(&play_pause)),
        KeyBinding::new("escape", Back, Some(&escape)),
        KeyBinding::new("secondary-[", Back, Some(context::ROOT)),
        KeyBinding::new("secondary-,", OpenSettings, Some(context::ROOT)),
    ]);
}
