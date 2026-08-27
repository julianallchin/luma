//! What the keyboard can ask for, and which keys ask for it.
//!
//! An action is the app's verb; a key is one way to say it and the automation
//! harness's `app.action("luma::PlayPause")` is another. Both land on the same
//! listener in [`crate::Luma::render`], so a script drives the app a person
//! could have driven, and neither can drift from the other.
//!
//! # Where a binding fires
//!
//! Scope comes from *key context*, not from a mode check inside the handler.
//! The shell declares [`context::ROOT`] at the window root, one context per
//! region under it, and the active tab's own context inside the workspace, so
//! `space` can mean playback in a track-editor tab and mean nothing at all in
//! the thread beside it.
//!
//! Contexts **nest**: `Luma > Workspace > TrackEditor`. gpui evaluates a
//! binding's predicate over the whole focus path and resolves by specificity,
//! which is what makes `secondary-w` a *scoped* binding rather than a runtime
//! branch — the tab's ⌘W wins while a tab is focused, and the window's works
//! everywhere else.
//!
//! # Why a text field can keep the space bar
//!
//! gpui matches bindings *before* it delivers key events, so a focused field
//! cannot out-run a binding by consuming the keystroke — by the time its
//! `on_key_down` runs, the action has already fired. The exclusion therefore
//! belongs on the binding: every binding whose key is a character a person
//! could be typing carries `&& !`[`context::TEXT_INPUT`], a predicate gpui
//! evaluates over the *whole* focus path. One rule, stated once, and it is
//! what keeps the sidebar's spacebar a space and its escape a cleared query.

use gpui::{actions, Action, App, KeyBinding};

/// The key contexts a dispatch path can carry: the window's, each region's,
/// each tab's, each overlay's, and one for a field taking typed text.
///
/// They live together because these names are only ever half of a pair — a
/// binding predicate here and a `key_context` on an element there — and a
/// name spelled differently in the two places is a binding that silently
/// never fires.
pub(crate) mod context {
    /// The window root. Everything Luma renders is under it, so a binding
    /// scoped here is app-wide without being global.
    pub const ROOT: &str = "Luma";

    // The three regions of the shell.
    pub const SIDEBAR: &str = "Sidebar";
    pub const THREAD: &str = "Thread";
    pub const WORKSPACE: &str = "Workspace";

    // Tab contexts, declared by the tab's own root *inside* `WORKSPACE`.
    pub const TRACK_EDITOR: &str = "TrackEditor";
    pub const GRAPH: &str = "Graph";
    pub const VISUALIZER: &str = "Visualizer";
    pub const UNIVERSE: &str = "Universe";

    // Overlay contexts. One overlay is up at a time, over all three regions.
    pub const VENUES: &str = "Venues";
    pub const PATTERNS: &str = "Patterns";
    pub const SETTINGS: &str = "Settings";
    pub const ADD_TRACKS: &str = "AddTracks";
    /// The chat-history picker.
    pub const CHAT_HISTORY: &str = "ChatHistory";
    /// The subagents dialog: the delegation list and one child's transcript.
    pub const SUBAGENTS: &str = "Subagents";
    /// Declared by a focused field that is taking typed text. Any binding on
    /// a key that field could be typing excludes it. Defined in `luma-ui`
    /// because the chat's composer, in another crate, declares the same
    /// context — see [`luma_ui::TEXT_INPUT`].
    pub use luma_ui::TEXT_INPUT;
}

actions!(
    luma,
    [
        /// Start or stop the track editor's transport.
        PlayPause,
        /// Dismiss the overlay that is up, or the insertion menu inside the
        /// editor. Nothing with neither up: the shell is persistent, so there
        /// is no screen to leave.
        DismissOverlay,
        /// Show or hide the sidebar / the workspace panel.
        ToggleSidebar,
        ToggleWorkspace,
        /// Switch the workspace between taking over everything right of the
        /// sidebar and sharing it with the thread column.
        ToggleExpand,
        /// Close the focused tab.
        CloseTab,
        /// Toggle the workspace's new-tab menu.
        NewTab,
        /// Reveal the nth tab in strip order.
        SelectTab1,
        SelectTab2,
        SelectTab3,
        SelectTab4,
        SelectTab5,
        SelectTab6,
        SelectTab7,
        SelectTab8,
        SelectTab9,
        /// Open the pattern picker overlay.
        OpenPatterns,
        /// Open settings over the whole shell.
        OpenSettings,
        /// Give the stage pane's room back to the editor under it, or take it
        /// again. Not an "open": which room the stage shows is implied by the
        /// visible tab, so this is a preference about screen space and never a
        /// navigation.
        ToggleVisualizer,
        /// Keep the track editor's view centred on the playhead.
        FollowPlayhead,
        /// Undo / redo the track editor's last edit.
        UndoClips,
        RedoClips,
        /// Remove the graph editor's selected nodes.
        DeleteNodes,
        /// Undo / redo the graph editor's last edit.
        UndoGraph,
        RedoGraph,
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
    let shell = format!(
        "{} && !{} && !{} && !{} && !{} && !{} && !{}",
        context::ROOT,
        context::VENUES,
        context::PATTERNS,
        context::SETTINGS,
        context::ADD_TRACKS,
        context::CHAT_HISTORY,
        context::SUBAGENTS
    );
    // A dialog owns Escape even when its current child is a text field. Route
    // dismissal policy remains in `Luma::dismiss_overlay` (notably, the
    // required first-venue screen refuses to close).
    let dialog = format!(
        "{} || {} || {} || {} || {} || {}",
        context::VENUES,
        context::PATTERNS,
        context::SETTINGS,
        context::ADD_TRACKS,
        context::CHAT_HISTORY,
        context::SUBAGENTS
    );
    // Every track-editor binding shares one predicate: it means something in
    // that tab and nothing anywhere else, and a field taking typed text
    // out-ranks all of it — `f` is a letter, `delete` is a correction, and
    // `secondary-c` is the clipboard the field already owns.
    let editing = format!("{} && !{}", context::TRACK_EDITOR, context::TEXT_INPUT);
    // The graph editor's bindings carry the same exclusion for the same
    // reason: a promoted param widget (phase 3) is a text field, and `delete`
    // is a correction there, not a command.
    let graphing = format!("{} && !{}", context::GRAPH, context::TEXT_INPUT);
    let mut bindings = vec![
        KeyBinding::new("space", PlayPause, Some(&editing)),
        KeyBinding::new("delete", DeleteNodes, Some(&graphing)),
        KeyBinding::new("backspace", DeleteNodes, Some(&graphing)),
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
        KeyBinding::new("escape", DismissOverlay, Some(&escape)),
        KeyBinding::new("escape", DismissOverlay, Some(&dialog)),
        KeyBinding::new("secondary-b", ToggleSidebar, Some(&shell)),
        KeyBinding::new("secondary-shift-b", ToggleWorkspace, Some(&shell)),
        // Scoped to the panel, so the window's own ⌘W still closes the window
        // from anywhere else. gpui resolves by specificity — this is a
        // binding, not a branch, which is what keeps "which ⌘W did I get"
        // answerable from the focus path alone.
        KeyBinding::new("secondary-w", CloseTab, Some(context::WORKSPACE)),
        KeyBinding::new("secondary-t", NewTab, Some(&shell)),
        KeyBinding::new("secondary-p", OpenPatterns, Some(&shell)),
        KeyBinding::new("secondary-,", OpenSettings, Some(&shell)),
        // Not a bare letter: the track editor's alphabet is already spoken
        // for, and this reshapes the column that editor sits in.
        KeyBinding::new("secondary-shift-v", ToggleVisualizer, Some(&shell)),
        KeyBinding::new("secondary-1", SelectTab1, Some(&shell)),
        KeyBinding::new("secondary-2", SelectTab2, Some(&shell)),
        KeyBinding::new("secondary-3", SelectTab3, Some(&shell)),
        KeyBinding::new("secondary-4", SelectTab4, Some(&shell)),
        KeyBinding::new("secondary-5", SelectTab5, Some(&shell)),
        KeyBinding::new("secondary-6", SelectTab6, Some(&shell)),
        KeyBinding::new("secondary-7", SelectTab7, Some(&shell)),
        KeyBinding::new("secondary-8", SelectTab8, Some(&shell)),
        KeyBinding::new("secondary-9", SelectTab9, Some(&shell)),
    ];
    chord(&mut bindings, "z", UndoClips, &editing);
    chord(&mut bindings, "shift-z", RedoClips, &editing);
    chord(&mut bindings, "z", UndoGraph, &graphing);
    chord(&mut bindings, "shift-z", RedoGraph, &graphing);
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
