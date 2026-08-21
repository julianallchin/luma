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

use gpui::{actions, Action, App, KeyBinding};

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
    pub const VISUALIZER: &str = "Visualizer";
    /// Declared by the workspace's DMX-patch tab. Named here with its siblings
    /// even though the tab that declares it is not mounted yet: this list is
    /// the vocabulary, and a context invented at the element instead would be
    /// the half of a pair that has no other half.
    pub const UNIVERSE: &str = "Universe";
    pub const SETTINGS: &str = "Settings";
    /// Declared by a focused field that is taking typed text. Any binding on
    /// a key that field could be typing excludes it. Defined in `luma-ui`
    /// because the chat panel's composer, in another crate, declares the same
    /// context — see [`luma_ui::TEXT_INPUT`].
    pub use luma_ui::TEXT_INPUT;
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
        /// Open the 3D stage view over the track editor or the track browser.
        OpenVisualizer,
        /// Keep the track editor's view centred on the playhead.
        FollowPlayhead,
        /// Show or hide the agent chat over whatever is showing.
        ToggleAgentChat,
        /// Undo / redo the track editor's last edit.
        UndoClips,
        RedoClips,
        /// Loop the track editor's cursor range, or clear the loop it already
        /// describes.
        ToggleLoopRegion,
        /// Clear the track editor's cursor region, or remove its selected
        /// clips when the cursor is a point.
        DeleteClips,
        /// Cut every clip the track editor's cursor crosses in two.
        SplitClips,
        CopyClips,
        CutClips,
        PasteClips,
        /// Lay a copy of the selection down immediately after it.
        DuplicateClips,
        /// Move the track editor's selection one lane up / down.
        MoveClipsUp,
        MoveClipsDown,
        /// Fit every lane on the track editor's canvas.
        FitLanes,
        /// Walk the insertion menu's active row, and put its pattern down.
        /// No-ops with no menu open, which is what leaves the bare arrows and
        /// Return unbound everywhere else in the editor, as on the web.
        NextInsertOption,
        PrevInsertOption,
        CommitInsertOption,
    ]
);

/// Bind the keys. Called from [`crate::init`], so the real binary and the
/// harness get the same keyboard.
pub(crate) fn init(cx: &mut App) {
    // `secondary-` is cmd on macOS and ctrl elsewhere: gpui resolves it per
    // platform, which is why there is no `cfg` here.
    let escape = format!("{} && !{}", context::ROOT, context::TEXT_INPUT);
    // Every track-editor binding shares one predicate: it means something on
    // that screen and nothing anywhere else, and a field taking typed text
    // out-ranks all of it — `f` is a letter, `delete` is a correction, and
    // `secondary-c` is the clipboard the field already owns.
    let editing = format!("{} && !{}", context::TRACK_EDITOR, context::TEXT_INPUT);
    let mut bindings = vec![
        KeyBinding::new("space", PlayPause, Some(&editing)),
        // `f` is a character a person could be typing, so it carries the same
        // text-input exclusion the space bar does.
        KeyBinding::new("f", FollowPlayhead, Some(&editing)),
        // The other letter, and the other escape hatch from a lane stack
        // taller than the canvas: `h` fits them all on it.
        KeyBinding::new("h", FitLanes, Some(&editing)),
        KeyBinding::new("delete", DeleteClips, Some(&editing)),
        KeyBinding::new("down", NextInsertOption, Some(&editing)),
        KeyBinding::new("up", PrevInsertOption, Some(&editing)),
        KeyBinding::new("enter", CommitInsertOption, Some(&editing)),
        KeyBinding::new("backspace", DeleteClips, Some(&editing)),
        KeyBinding::new("alt-up", MoveClipsUp, Some(&editing)),
        KeyBinding::new("alt-down", MoveClipsDown, Some(&editing)),
        KeyBinding::new("escape", Back, Some(&escape)),
        KeyBinding::new("secondary-[", Back, Some(context::ROOT)),
        KeyBinding::new("secondary-,", OpenSettings, Some(context::ROOT)),
        // Not a bare letter: the track editor's alphabet is already spoken
        // for, and this opens over that screen.
        KeyBinding::new("secondary-shift-v", OpenVisualizer, Some(context::ROOT)),
        // Not `secondary-l`: that letter is the track editor's loop region,
        // and a chord that means one thing on one screen and another
        // everywhere else is a chord nobody can learn.
        KeyBinding::new("secondary-shift-l", ToggleAgentChat, Some(context::ROOT)),
    ];
    chord(&mut bindings, "z", UndoClips, &editing);
    chord(&mut bindings, "shift-z", RedoClips, &editing);
    chord(&mut bindings, "e", SplitClips, &editing);
    chord(&mut bindings, "c", CopyClips, &editing);
    chord(&mut bindings, "x", CutClips, &editing);
    chord(&mut bindings, "v", PasteClips, &editing);
    chord(&mut bindings, "d", DuplicateClips, &editing);
    chord(&mut bindings, "l", ToggleLoopRegion, &editing);
    cx.bind_keys(bindings);
}

/// Bind one editing chord under both of its modifiers.
///
/// The web editor reads `metaKey || ctrlKey` for every one of these, so
/// control works on macOS too and the hand that learned the shortcut on one
/// platform keeps it on the next. On Linux and Windows `secondary-` already
/// *is* control and the two spellings resolve to the same binding.
fn chord<A: Action + Clone>(bindings: &mut Vec<KeyBinding>, key: &str, action: A, context: &str) {
    bindings.push(KeyBinding::new(
        &format!("secondary-{key}"),
        action.clone(),
        Some(context),
    ));
    bindings.push(KeyBinding::new(
        &format!("ctrl-{key}"),
        action,
        Some(context),
    ));
}
