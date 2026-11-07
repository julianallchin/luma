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
//! region under it, and the mode body's own context inside the workspace, so
//! `space` can mean playback in Arrange's timeline and mean nothing at all in
//! the agent panel beside it.
//!
//! Contexts **nest**: `Luma > Workspace > TrackEditor`. gpui evaluates a
//! binding's predicate over the whole focus path and resolves by specificity,
//! so a binding scoped to a body wins over a shell-wide one while that body
//! has the keyboard.
//!
//! # Why a text field can keep the space bar
//!
//! gpui matches bindings *before* it delivers key events, so a focused field
//! cannot out-run a binding by consuming the keystroke — by the time its
//! `on_key_down` runs, the action has already fired. The exclusion therefore
//! belongs on the binding: every binding whose key is a character a person
//! could be typing carries `&& !`[`context::TEXT_INPUT`], a predicate gpui
//! evaluates over the *whole* focus path. One rule, stated once, and it is
//! what keeps the song list's spacebar a space and its escape a cleared query.
//! [`Shortcut::bindings`] adds it to any row whose key types, so a person who
//! rebinds a command to a bare letter cannot take that letter from a field.
//!
//! # One table
//!
//! Every shortcut a person can see and change is a row of [`SHORTCUTS`]: its
//! action, a stable id, a title, the group Settings lists it under, its
//! default keys, where it is live and the menu it sits in. The keymap's
//! bindings, the menu bar and Settings → Shortcuts are all read off it, so the
//! three cannot disagree. A rebind is an [`Overrides`] entry (id → keys),
//! kept per machine in the session store; [`apply`] rebuilds the keymap and
//! the menus from it, live.
//!
//! What is *not* a row stays bound in [`init`] as before: Escape, which
//! dismisses whatever is innermost and is not a person's to move; the
//! insertion menu's arrows and Return; and every field's and navigator's own
//! keys (`luma_ui::text_input`, `float::Nav`, the venue picker's arrows).
//! Those are a widget's grammar, not commands.

use std::collections::BTreeMap;
use std::rc::Rc;

use gpui::{
    actions, Action, App, DummyKeyboardMapper, KeyBinding, KeyBindingContextPredicate,
    KeyBindingMetaIndex, KeyContext, Keystroke, Menu, MenuItem,
};

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

    // The shell's regions: the agent panel, the content and the song list.
    pub const THREAD: &str = "Thread";
    pub const WORKSPACE: &str = "Workspace";
    pub const SONGS: &str = "Songs";

    // Body contexts, declared by the body's own root *inside* `WORKSPACE`.
    pub const TRACK_EDITOR: &str = "TrackEditor";
    /// The clip graph canvas in the score inspector: it keeps the score's
    /// undo while it has the keyboard.
    pub const CLIP_GRAPH: &str = "ClipGraph";
    pub const VISUALIZER: &str = "Visualizer";
    pub const VISUALIZER_FULLSCREEN: &str = "VisualizerFullscreen";
    pub const PATCH: &str = "Patch";
    pub const STAGE: &str = "Stage";
    pub const PERFORM: &str = "Perform";

    // Overlay contexts. One overlay is up at a time, over all three regions.
    pub const VENUES: &str = "Venues";
    /// The settings page. Not an overlay but the page's content; it owns the
    /// keyboard the way a dialog does, so it is listed with them.
    pub const SETTINGS: &str = "Settings";
    pub const ADD_TRACKS: &str = "AddTracks";
    /// The chat-history picker.
    pub const CHAT_HISTORY: &str = "ChatHistory";
    /// The subagents dialog: the delegation list and one child's transcript.
    pub const SUBAGENTS: &str = "Subagents";
    /// Picking a clip's fixtures off the room.
    pub const FIXTURE_PICKER: &str = "FixturePicker";
    pub const PATTERN_INSERT: &str = "PatternInsert";
    /// The one confirmation dialog — see [`crate::confirm`].
    pub const CONFIRM: &str = "Confirm";
    /// "An update is ready" — see [`crate::update`].
    pub const UPDATE_READY: &str = "UpdateReady";
    /// The "Export show" dialog — see [`crate::export_dialog`].
    pub const SHOW_EXPORT: &str = "ShowExport";
    /// The full-screen sign-in state. Not in [`DIALOGS`]: it is not a plane
    /// over the shell, it *is* the app while it is up, so the shell's bindings
    /// are absent because the shell is — not because a predicate excluded them.
    pub const SIGN_IN: &str = "SignIn";

    /// Every dialog context, once.
    ///
    /// Two predicates are built from this list — the shell's bindings are
    /// disabled inside any dialog, and Escape is enabled inside every one —
    /// and a context named in one but not the other is a dialog whose Escape
    /// or whose ⌘B is silently wrong. Naming them here is what keeps the two
    /// from drifting.
    pub const DIALOGS: [&str; 10] = [
        VENUES,
        SETTINGS,
        ADD_TRACKS,
        CHAT_HISTORY,
        SUBAGENTS,
        FIXTURE_PICKER,
        PATTERN_INSERT,
        CONFIRM,
        UPDATE_READY,
        SHOW_EXPORT,
    ];
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
        /// In the add-track source browser: preview the row under the cursor,
        /// or pause the one playing.
        ToggleSourcePreview,
        /// Move the playing source preview 10 s back / forward.
        SeekSourcePreviewBack,
        SeekSourcePreviewForward,
        /// Dismiss the overlay that is up, or the insertion menu inside the
        /// editor. Nothing with neither up: the shell is persistent, so there
        /// is no screen to leave.
        DismissOverlay,
        /// Show or hide the agent panel.
        ToggleAgent,
        /// Put a mode on screen: the room, the open score, the live show.
        ShowRig,
        ShowArrange,
        ShowLive,
        /// Open or close the song list page over Arrange.
        ToggleSongList,
        /// In Arrange, put the selected clip's graph where the timeline is —
        /// what a double-click on the clip does.
        OpenClipGraph,
        /// Open settings over the whole shell.
        OpenSettings,
        /// Give the stage pane's room back to the editor under it, or take it
        /// again. Not an "open": which room the stage shows is implied by the
        /// visible tab, so this is a preference about screen space and never a
        /// navigation.
        ToggleVisualizer,
        ToggleVisualizerFullscreen,
        DismissVisualizerFullscreen,
        Quit,
        /// Copy the stage builder's selected subtree onto the cursor. Scoped
        /// to the stage page, because ⌘D means nothing anywhere else and a
        /// shell-wide binding would take the chord from every tab that might
        /// one day want it.
        DuplicateSubtree,
        /// Put the builder's hand down without placing anything.
        CancelBuild,
        /// Which transform widget the stage's selection wears — three's own
        /// `W`/`E` pair, scoped to the stage page.
        GizmoTranslate,
        GizmoRotate,
        /// Delete the stage builder's selected subtree, its fixtures and
        /// their patch rows with it.
        DeleteStageElement,
        /// Open the stage builder's add-element dialog, caret in its search.
        AddStageElement,
        /// Restore the stage graph a verb replaced / step forward again.
        UndoStage,
        RedoStage,
        /// Zoom the stage camera a step in / out at the pane's centre — the
        /// wheel's verb on a key.
        ZoomStageIn,
        ZoomStageOut,
        /// Frame the stage selection: put the camera's target on it and dolly
        /// to a radius that fits it.
        FocusStageSelection,
        /// Write the stage camera, sun and shadow cascades of the next frame
        /// to a JSON file, for reproducing a view in a render test.
        ExportStageCamera,
        /// Keep the track editor's view centred on the playhead.
        FollowPlayhead,
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
        /// Return unbound everywhere else in the editor.
        NextInsertOption,
        PrevInsertOption,
        CommitInsertOption,
        /// The venue dialog's commit chord: from the list, open Create; on a
        /// form, submit it.
        CreateVenue,
        /// The venue dialog: go to Join with a code.
        JoinVenue,
        /// The venue dialog: share the venue under the cursor.
        ShareVenue,
        /// The venue dialog's share route: copy the code.
        CopyShareCode,
        /// The add-track dialog's commit chord: what the route's lit chip does.
        ImportTracks,
    ]
);

// -- the table ----------------------------------------------------------------

/// Where a row's binding is live: a predicate over the focus path. A row
/// whose key types also gets `!TextInput` from [`Shortcut::bindings`]; the
/// ones written here carry it only where a *chord* belongs to a field too
/// (⌘C is the clipboard the field owns).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Scope {
    /// Everywhere, over dialogs too.
    App,
    /// The shell: off inside any dialog and over the fullscreen room.
    Shell,
    /// Arrange's timeline.
    Timeline,
    /// The timeline and the clip graph page, which keeps the score's undo.
    Clips,
    /// The timeline or the room, wherever either is drawn.
    Transport,
    /// The stage builder, embedded or fullscreen.
    Stage,
    /// The room, wherever it is drawn.
    Room,
    /// The stage builder, or the fullscreen room of any tab.
    Framing,
    /// The venue dialog.
    Venues,
    /// The venue dialog away from a field.
    VenuesUntyped,
    /// The add-track dialog.
    AddTracks,
}

impl Scope {
    fn predicate(self) -> Option<String> {
        let no_text = |inner: &str| format!("{inner} && !{}", context::TEXT_INPUT);
        let either = |a: &str, b: &str| no_text(&format!("({a} || {b})"));
        Some(match self {
            Scope::App => return None,
            Scope::Shell => std::iter::once(context::ROOT.to_string())
                .chain(context::DIALOGS.iter().map(|name| format!("!{name}")))
                .chain(std::iter::once(format!(
                    "!{}",
                    context::VISUALIZER_FULLSCREEN
                )))
                .collect::<Vec<_>>()
                .join(" && "),
            // `secondary-c` is the clipboard a field already owns, so the
            // editor's chords leave fields alone as its letters do.
            Scope::Timeline => no_text(context::TRACK_EDITOR),
            Scope::Clips => either(context::TRACK_EDITOR, context::CLIP_GRAPH),
            Scope::Transport => either(context::TRACK_EDITOR, context::VISUALIZER),
            Scope::Stage => no_text(context::STAGE),
            Scope::Room => no_text(context::VISUALIZER),
            Scope::Framing => either(context::STAGE, context::VISUALIZER_FULLSCREEN),
            Scope::Venues => context::VENUES.to_string(),
            Scope::VenuesUntyped => no_text(context::VENUES),
            Scope::AddTracks => context::ADD_TRACKS.to_string(),
        })
    }
}

/// The menu-bar menu a row sits in, if any.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MenuSlot {
    App,
    View,
}

/// One shortcut a person can see and rebind.
pub(crate) struct Shortcut {
    /// Stable across releases: an [`Overrides`] key. Never rename one.
    pub(crate) id: &'static str,
    pub(crate) title: &'static str,
    /// The Settings → Shortcuts heading it is listed under.
    pub(crate) group: &'static str,
    /// The default keys, first the one a hint shows.
    pub(crate) keys: &'static [&'static str],
    pub(crate) scope: Scope,
    pub(crate) menu: Option<MenuSlot>,
    action: fn() -> Box<dyn Action>,
}

macro_rules! row {
    ($id:literal, $title:literal, $group:literal, [$($key:literal),*], $scope:ident, $menu:expr, $action:ident) => {
        Shortcut {
            id: $id,
            title: $title,
            group: $group,
            keys: &[$($key),*],
            scope: Scope::$scope,
            menu: $menu,
            action: || Box::new($action),
        }
    };
}

const APP: Option<MenuSlot> = Some(MenuSlot::App);
const VIEW: Option<MenuSlot> = Some(MenuSlot::View);

/// Every rebindable shortcut, in the order Settings and the menus list them.
///
/// An editing chord is listed under `secondary-` and `ctrl-` both: control
/// works on macOS too, so the hand that learned it on one platform keeps it
/// on the next. Elsewhere the two spellings are one binding.
#[rustfmt::skip]
pub(crate) const SHORTCUTS: &[Shortcut] = &[
    row!("app.settings", "Open settings", "General", ["secondary-,"], Shell, APP, OpenSettings),
    row!("app.quit", "Quit Luma", "General", ["secondary-q"], App, APP, Quit),
    row!("view.agent", "Show or hide the agent panel", "View", ["secondary-b"], Shell, VIEW, ToggleAgent),
    row!("view.song_list", "Song list", "View", ["secondary-p"], Shell, VIEW, ToggleSongList),
    // Not a bare letter: the track editor's alphabet is already spoken for,
    // and this reshapes the column that editor sits in.
    row!("view.visualizer", "Show or hide the visualizer", "View", ["secondary-shift-v"], Shell, VIEW, ToggleVisualizer),
    row!("view.fullscreen", "Fullscreen visualizer", "View", ["shift-f"], Room, VIEW, ToggleVisualizerFullscreen),
    row!("mode.rig", "Rig", "Modes", ["1"], Shell, VIEW, ShowRig),
    row!("mode.arrange", "Arrange", "Modes", ["2"], Shell, VIEW, ShowArrange),
    row!("mode.live", "Live", "Modes", ["3"], Shell, VIEW, ShowLive),
    row!("arrange.play", "Play or pause", "Arrange", ["space"], Transport, None, PlayPause),
    row!("arrange.follow", "Follow the playhead", "Arrange", ["f"], Timeline, None, FollowPlayhead),
    // The other escape hatch from a lane stack taller than the canvas.
    row!("arrange.fit", "Fit every lane", "Arrange", ["h"], Timeline, None, FitLanes),
    row!("arrange.undo", "Undo", "Arrange", ["secondary-z", "ctrl-z"], Clips, None, UndoClips),
    row!("arrange.redo", "Redo", "Arrange", ["secondary-shift-z", "ctrl-shift-z"], Clips, None, RedoClips),
    row!("arrange.loop", "Loop the selection", "Arrange", ["secondary-l", "ctrl-l"], Timeline, None, ToggleLoopRegion),
    row!("arrange.split", "Split at the cursor", "Arrange", ["secondary-e", "ctrl-e"], Timeline, None, SplitClips),
    row!("arrange.copy", "Copy", "Arrange", ["secondary-c", "ctrl-c"], Timeline, None, CopyClips),
    row!("arrange.cut", "Cut", "Arrange", ["secondary-x", "ctrl-x"], Timeline, None, CutClips),
    row!("arrange.paste", "Paste", "Arrange", ["secondary-v", "ctrl-v"], Timeline, None, PasteClips),
    row!("arrange.duplicate", "Duplicate", "Arrange", ["secondary-d", "ctrl-d"], Timeline, None, DuplicateClips),
    row!("arrange.delete", "Delete", "Arrange", ["delete", "backspace"], Timeline, None, DeleteClips),
    row!("arrange.move_up", "Move up a lane", "Arrange", ["alt-up"], Timeline, None, MoveClipsUp),
    row!("arrange.move_down", "Move down a lane", "Arrange", ["alt-down"], Timeline, None, MoveClipsDown),
    row!("rig.add", "Add an element", "Rig", ["a"], Stage, None, AddStageElement),
    row!("rig.duplicate", "Duplicate", "Rig", ["secondary-d"], Stage, None, DuplicateSubtree),
    row!("rig.delete", "Delete", "Rig", ["delete", "backspace"], Stage, None, DeleteStageElement),
    row!("rig.undo", "Undo", "Rig", ["secondary-z", "ctrl-z"], Stage, None, UndoStage),
    row!("rig.redo", "Redo", "Rig", ["secondary-shift-z", "ctrl-shift-z"], Stage, None, RedoStage),
    row!("rig.move", "Move tool", "Rig", ["w"], Stage, None, GizmoTranslate),
    row!("rig.rotate", "Rotate tool", "Rig", ["e"], Stage, None, GizmoRotate),
    row!("room.frame", "Frame the selection", "Visualizer", ["f"], Framing, None, FocusStageSelection),
    row!("room.zoom_in", "Zoom in", "Visualizer", ["="], Room, None, ZoomStageIn),
    row!("room.zoom_out", "Zoom out", "Visualizer", ["-"], Room, None, ZoomStageOut),
    row!("room.export_camera", "Export the camera", "Visualizer", ["secondary-shift-e"], Room, None, ExportStageCamera),
    row!("venues.create", "Create a venue", "Venues", ["secondary-enter"], Venues, None, CreateVenue),
    row!("venues.join", "Join with a code", "Venues", ["secondary-j"], Venues, None, JoinVenue),
    row!("venues.share", "Share a venue", "Venues", ["secondary-i"], Venues, None, ShareVenue),
    row!("venues.copy_code", "Copy the share code", "Venues", ["secondary-c"], VenuesUntyped, None, CopyShareCode),
    row!("add_tracks.import", "Import", "Add tracks", ["secondary-enter"], AddTracks, None, ImportTracks),
];

/// What this machine has rebound: a row's id to the keys it has instead of
/// its defaults. Not synced — a keyboard is a machine's.
pub(crate) type Overrides = BTreeMap<String, Vec<String>>;

/// The session-store key [`Overrides`] live under.
pub(crate) const OVERRIDES_KEY: &str = "shortcuts";

/// Marks a binding as the table's, so [`apply`] can replace exactly those.
const TABLE: KeyBindingMetaIndex = KeyBindingMetaIndex(0x4c75_6d61);

impl Shortcut {
    pub(crate) fn action(&self) -> Box<dyn Action> {
        (self.action)()
    }

    pub(crate) fn by_id(id: &str) -> Option<&'static Shortcut> {
        SHORTCUTS.iter().find(|row| row.id == id)
    }

    /// The row whose action `action` is.
    fn of(action: &dyn Action) -> Option<&'static Shortcut> {
        SHORTCUTS
            .iter()
            .find(|row| row.action().name() == action.name())
    }

    /// The keys it answers to under `overrides`.
    pub(crate) fn keys<'a>(&'a self, overrides: &'a Overrides) -> Vec<&'a str> {
        match overrides.get(self.id) {
            Some(keys) => keys.iter().map(String::as_str).collect(),
            None => self.keys.to_vec(),
        }
    }

    /// One binding for `key`, or `None` for a key gpui cannot parse (a stored
    /// rebind from a build that spelled keys differently binds nothing).
    fn binding(&self, key: &str) -> Option<KeyBinding> {
        let stroke = Keystroke::parse(key).ok()?;
        let no_text = format!("!{}", context::TEXT_INPUT);
        let predicate = match (self.scope.predicate(), types(&stroke)) {
            (predicate, false) => predicate,
            (Some(p), true) if p.contains(&no_text) => Some(p),
            (Some(p), true) => Some(format!("({p}) && {no_text}")),
            (None, true) => Some(no_text),
        };
        let predicate = match predicate {
            Some(source) => Some(Rc::new(KeyBindingContextPredicate::parse(&source).ok()?)),
            None => None,
        };
        let binding = KeyBinding::load(
            key,
            self.action(),
            predicate,
            false,
            None,
            &DummyKeyboardMapper,
        )
        .ok()?;
        Some(binding.with_meta(TABLE))
    }

    /// Its bindings under `overrides`, one per distinct keystroke.
    pub(crate) fn bindings(&self, overrides: &Overrides) -> Vec<KeyBinding> {
        let mut out: Vec<KeyBinding> = Vec::new();
        for binding in self
            .keys(overrides)
            .into_iter()
            .filter_map(|key| self.binding(key))
        {
            if !out
                .iter()
                .any(|kept| kept.keystrokes() == binding.keystrokes())
            {
                out.push(binding);
            }
        }
        out
    }
}

/// Whether `stroke` is one a field could be typing: no chord modifier.
fn types(stroke: &Keystroke) -> bool {
    let m = &stroke.modifiers;
    !(m.control || m.alt || m.platform || m.function)
}

/// Every table binding under `overrides`.
fn table_bindings(overrides: &Overrides) -> Vec<KeyBinding> {
    SHORTCUTS
        .iter()
        .flat_map(|row| row.bindings(overrides))
        .collect()
}

/// Whether `binding` is one of the table's, as opposed to a field's or a
/// widget's own key.
pub(crate) fn from_table(binding: &KeyBinding) -> bool {
    binding.meta() == Some(TABLE)
}

/// The menu bar, from the table: each row with a menu, in table order, a
/// rule between groups. gpui shows each item's key from the keymap.
fn menus() -> Vec<Menu> {
    let menu = |name: &'static str, slot: MenuSlot| {
        let mut items = Vec::new();
        let mut group = None;
        for row in SHORTCUTS.iter().filter(|row| row.menu == Some(slot)) {
            if group.is_some_and(|group| group != row.group) {
                items.push(MenuItem::separator());
            }
            group = Some(row.group);
            items.push(MenuItem::Action {
                name: row.title.into(),
                action: row.action(),
                os_action: None,
                checked: false,
                disabled: false,
            });
        }
        Menu::new(name).items(items)
    };
    vec![menu("Luma", MenuSlot::App), menu("View", MenuSlot::View)]
}

/// Put the table's bindings for `overrides` in place of the ones bound now,
/// and rebuild the menu bar, whose key equivalents are read off the keymap.
///
/// gpui has no "remove these bindings", so the keymap is cleared and bound
/// again: everything not the table's — the fields', the chat's, the
/// component library's — goes back in its own order, the table's block where
/// it was. Registration order is precedence at equal depth, so it is kept.
pub(crate) fn apply(overrides: &Overrides, cx: &mut App) {
    let bound: Vec<KeyBinding> = cx.key_bindings().borrow().bindings().cloned().collect();
    let mut fresh = Some(table_bindings(overrides));
    let mut next = Vec::with_capacity(bound.len());
    for binding in bound {
        if from_table(&binding) {
            next.extend(fresh.take().into_iter().flatten());
        } else {
            next.push(binding);
        }
    }
    next.extend(fresh.into_iter().flatten());
    cx.clear_key_bindings();
    cx.bind_keys(next);
    cx.set_menus(menus());
}

/// The command that already answers `key` wherever `row` would, if any, by
/// the name a person knows it by. Every binding in the keymap counts — the
/// table's and the fields' alike — except `row`'s own.
///
/// "Wherever `row` would" is decided against [`situations`], the focus paths
/// the app can be in: two bindings collide when some path makes both live.
pub(crate) fn holder(row: &Shortcut, key: &str, cx: &App) -> Option<String> {
    let mine = row.binding(key)?;
    let stroke = Keystroke::parse(key).ok()?;
    let own = row.action();
    let keymap = cx.key_bindings();
    let keymap = keymap.borrow();
    let paths = situations();
    let held = keymap.bindings().find(|other| {
        other.action().name() != own.name()
            && other.match_keystrokes(std::slice::from_ref(&stroke)) == Some(false)
            && overlap(&mine, other, &paths)
    })?;
    Some(match Shortcut::of(held.action()) {
        Some(owner) => owner.title.to_string(),
        None => humanize(held.action().name()),
    })
}

/// Whether some focus path makes both bindings live.
fn overlap(a: &KeyBinding, b: &KeyBinding, paths: &[Vec<KeyContext>]) -> bool {
    let live = |binding: &KeyBinding, path: &[KeyContext]| {
        binding
            .predicate()
            .is_none_or(|predicate| predicate.depth_of(path).is_some())
    };
    paths.iter().any(|path| live(a, path) && live(b, path))
}

/// `luma::DismissOverlay` as a person would say it: "Dismiss overlay".
fn humanize(name: &str) -> String {
    let verb = name.rsplit("::").next().unwrap_or(name);
    let mut out = String::new();
    for (index, ch) in verb.chars().enumerate() {
        if ch.is_uppercase() && index > 0 {
            out.push(' ');
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// The focus paths the app can be in, as gpui sees them: a shell region or
/// a body under the root, a dialog, the fullscreen room — each with and
/// without a field taking typed text. This is the conflict check's one piece
/// of hand-kept knowledge: a new body or region that nests its contexts
/// differently belongs here (`shell.rs` declares them).
fn situations() -> Vec<Vec<KeyContext>> {
    use context::*;
    let search = "PickerSearch TextInput";
    let draft = "FieldDraft";
    let rig = "Stage Patch Visualizer";
    let workspace = |inner: &[&'static str]| {
        let mut path = vec![ROOT, WORKSPACE];
        path.extend_from_slice(inner);
        path
    };
    let mut paths: Vec<Vec<&str>> = vec![
        workspace(&[TRACK_EDITOR]),
        workspace(&[TRACK_EDITOR, draft, search]),
        workspace(&[CLIP_GRAPH]),
        workspace(&[CLIP_GRAPH, draft, search]),
        workspace(&[PATCH]),
        workspace(&[PERFORM]),
        workspace(&[PERFORM, draft, search]),
        workspace(&[rig]),
        workspace(&[VISUALIZER, VISUALIZER]),
        workspace(&[VISUALIZER, rig]),
        vec![ROOT, THREAD],
        vec![ROOT, THREAD, "Composer TextInput"],
        vec![ROOT, SONGS],
        vec![ROOT, SONGS, search],
        vec![ROOT, VISUALIZER_FULLSCREEN, VISUALIZER],
        vec![ROOT, VISUALIZER_FULLSCREEN, rig],
    ];
    for dialog in DIALOGS {
        paths.push(vec![ROOT, dialog]);
        paths.push(vec![ROOT, dialog, search]);
    }
    paths
        .into_iter()
        .map(|path| {
            path.into_iter()
                .map(|entry| KeyContext::parse(entry).expect("a context path entry"))
                .collect()
        })
        .collect()
}

/// Bindings that must out-rank a text field's own: call after
/// [`luma_ui::text_input::init`], never before.
///
/// gpui ranks bindings that match at the same depth by registration order,
/// later first, and these match *at the field* (`AddTracks > PickerSearch`).
/// Registered earlier, `shift-left` would always select text. Each handler
/// propagates when the key is not its business — space while typing a query,
/// shift-arrows with nothing previewing — and gpui then tries the field's own
/// binding, or types the character, exactly as if this one did not exist.
pub(crate) fn init_field_overrides(cx: &mut App) {
    let source_search = format!(
        "{} > {}",
        context::ADD_TRACKS,
        luma_ui::text_input::SEARCH_CONTEXT
    );
    cx.bind_keys([
        KeyBinding::new("space", ToggleSourcePreview, Some(&source_search)),
        KeyBinding::new("shift-left", SeekSourcePreviewBack, Some(&source_search)),
        KeyBinding::new(
            "shift-right",
            SeekSourcePreviewForward,
            Some(&source_search),
        ),
    ]);
}

/// Bind the keys: the ones outside the table, then the table's defaults.
/// Called from [`crate::init`], so the real binary and the harness get the
/// same keyboard; a machine's rebinds land later, through [`apply`].
pub(crate) fn init(cx: &mut App) {
    let escape = format!(
        "{} && !{} && !{}",
        context::ROOT,
        context::TEXT_INPUT,
        context::VISUALIZER_FULLSCREEN
    );
    // A dialog owns Escape even when its current child is a text field. Route
    // dismissal policy remains in `Luma::dismiss_overlay` (notably, the
    // required first-venue screen refuses to close).
    let dialog = context::DIALOGS.join(" || ");
    // The insertion menu's walk, in its own context and in the editor.
    let editing = format!("{} && !{}", context::TRACK_EDITOR, context::TEXT_INPUT);
    let mut bindings = vec![
        KeyBinding::new("down", NextInsertOption, Some(context::PATTERN_INSERT)),
        KeyBinding::new("up", PrevInsertOption, Some(context::PATTERN_INSERT)),
        KeyBinding::new("enter", CommitInsertOption, Some(context::PATTERN_INSERT)),
        KeyBinding::new("escape", DismissOverlay, Some(context::PATTERN_INSERT)),
        KeyBinding::new("down", NextInsertOption, Some(&editing)),
        KeyBinding::new("up", PrevInsertOption, Some(&editing)),
        KeyBinding::new("enter", CommitInsertOption, Some(&editing)),
        KeyBinding::new("escape", DismissOverlay, Some(&escape)),
        KeyBinding::new("escape", DismissOverlay, Some(&dialog)),
        KeyBinding::new("escape", CancelBuild, Some(context::STAGE)),
        KeyBinding::new(
            "escape",
            DismissVisualizerFullscreen,
            Some(context::VISUALIZER_FULLSCREEN),
        ),
    ];
    bindings.extend(table_bindings(&Overrides::new()));
    cx.bind_keys(bindings);
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.set_menus(menus());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn ids_and_actions_are_each_used_once() {
        let mut ids = HashSet::new();
        let mut actions = HashSet::new();
        for row in SHORTCUTS {
            assert!(ids.insert(row.id), "two rows are {}", row.id);
            assert!(
                actions.insert(row.action().name()),
                "two rows bind {}",
                row.action().name()
            );
            assert!(!row.keys.is_empty(), "{} has no default key", row.id);
        }
    }

    #[test]
    fn every_default_key_parses_and_binds() {
        for row in SHORTCUTS {
            for key in row.keys {
                assert!(row.binding(key).is_some(), "{} cannot bind {key}", row.id);
            }
        }
    }

    /// The table's promise: no key means two commands anywhere a person can
    /// be. A row holding a key in two places is one command, not a clash.
    #[test]
    fn no_default_key_is_held_twice_where_both_are_live() {
        let paths = situations();
        let rows: Vec<(&Shortcut, KeyBinding)> = SHORTCUTS
            .iter()
            .flat_map(|row| {
                row.bindings(&Overrides::new())
                    .into_iter()
                    .map(move |binding| (row, binding))
            })
            .collect();
        for (i, (a, x)) in rows.iter().enumerate() {
            for (b, y) in &rows[i + 1..] {
                if a.id != b.id && x.keystrokes() == y.keystrokes() {
                    assert!(
                        !overlap(x, y, &paths),
                        "{} and {} both answer {} somewhere",
                        a.id,
                        b.id,
                        x.keystrokes()[0].unparse()
                    );
                }
            }
        }
    }

    /// The check above is not vacuous: one key in two rows is kept apart
    /// only by where the rows live.
    #[test]
    fn a_shared_key_collides_only_where_both_rows_live() {
        let paths = situations();
        let f = |id: &str| Shortcut::by_id(id).unwrap().binding("f").unwrap();
        assert!(!overlap(&f("arrange.follow"), &f("room.frame"), &paths));
        assert!(overlap(&f("arrange.follow"), &f("arrange.play"), &paths));
        assert!(overlap(&f("room.frame"), &f("view.fullscreen"), &paths));
    }

    /// A key that types is never taken from a field, whatever row it is
    /// bound to; a chord on a shell row is.
    #[test]
    fn a_typing_key_leaves_fields_alone() {
        let field: Vec<KeyContext> = [context::ROOT, context::SONGS, "PickerSearch TextInput"]
            .into_iter()
            .map(|entry| KeyContext::parse(entry).unwrap())
            .collect();
        let live = |id: &str, key: &str| {
            let binding = Shortcut::by_id(id).unwrap().binding(key).unwrap();
            binding.predicate().unwrap().depth_of(&field).is_some()
        };
        assert!(live("view.song_list", "secondary-p"));
        assert!(!live("view.song_list", "p"));
        assert!(!live("app.quit", "q"));
    }

    #[test]
    fn an_override_replaces_the_defaults() {
        let undo = Shortcut::by_id("arrange.undo").unwrap();
        assert_eq!(undo.keys(&Overrides::new()), ["secondary-z", "ctrl-z"]);
        let mut overrides = Overrides::new();
        overrides.insert("arrange.undo".into(), vec!["alt-u".into()]);
        let bound = undo.bindings(&overrides);
        assert_eq!(bound.len(), 1);
        assert_eq!(bound[0].keystrokes()[0].key(), "u");
        overrides.insert("arrange.undo".into(), vec!["ctrl-u-v".into()]);
        assert!(undo.bindings(&overrides).is_empty());
    }

    #[test]
    fn humanized_names_read_as_words() {
        assert_eq!(humanize("luma::DismissOverlay"), "Dismiss overlay");
        assert_eq!(humanize("text_input::SelectAll"), "Select all");
    }
}
