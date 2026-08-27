//! A real text field: byte-offset selection, IME, undo, and a measured layout.
//!
//! Adapted from comet's `ComposerInput` (itself adapted from gpui's
//! `examples/input.rs`). It is here rather than in the chat crate because two
//! surfaces need the same editor — the composer, and the search field in a
//! picker — and a second copy is the one thing this codebase treats as a bug.
//!
//! # What makes it deep
//!
//! The interface is one entity that renders itself; everything a caller could
//! get wrong is inside. Every mutation — a keystroke, a paste, an undo,
//! an IME commit, a programmatic `set_text` — funnels through
//! [`EntityInputHandler::replace_text_in_range`], so there is exactly one place
//! that touches `content`, one that records undo, and one that moves the caret.
//! A second write path is how a text field grows a state that disagrees with
//! itself.
//!
//! # Contexts, and why they are a pair
//!
//! gpui matches key bindings *before* it delivers key events, so a focused
//! field cannot out-run a binding by consuming the keystroke. Two consequences:
//!
//! 1. Editing keys must be *bound*, not handled in `on_key_down`. [`init`]
//!    binds the whole set, twice — once under [`COMPOSER_CONTEXT`], once under
//!    [`SEARCH_CONTEXT`], which deliberately leaves the bare arrows and `enter`
//!    unbound so a picker's own frame still receives them.
//! 2. Every app binding on a key a person could be typing carries
//!    `&& !`[`crate::TEXT_INPUT`]. So a field declares *both* names: its own
//!    context, and `TextInput`. [`Mode::key_context`] is the one place that
//!    pairing is spelled.
//!
//! # What is not here
//!
//! No completion ghost, no `@`-mention chips, no attachment staging. Comet has
//! all three; luma has no consumer for them yet, and dead UI is worse than an
//! absent feature. Pasted images are dropped rather than inserted as the debug
//! form of an image, which is what "paste the text instead" would produce.

use std::ops::Range;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    actions, div, fill, point, prelude::*, px, relative, size, App, Bounds, ClipboardEntry,
    ClipboardItem, Context, CursorStyle, DispatchPhase, ElementInputHandler, Entity,
    EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId, Hsla, KeyBinding,
    LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point,
    ScrollWheelEvent, SharedString, Task, TextRun, TextStyle, UTF16Selection, UnderlineStyle,
    Window, WrappedLine,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::ladder;

// -- metrics -----------------------------------------------------------------

/// One wrapped line: comet's `text-[14px] leading-relaxed`, 14 × 1.625.
///
/// Its own metric, neither the instrument tier's body text nor the markdown
/// renderer's: this is the *field's* line box, and any grow range built on top
/// of it is expressed in whole multiples of it. A plate whose line height
/// disagreed with the field it holds would grow in fractions of a row.
pub const LINE_HEIGHT: f32 = 22.75;
/// The field's text size.
pub const TEXT_SIZE: f32 = 14.0;

/// Caret blink half-period — the standard textarea cadence.
pub const CARET_BLINK_MS: u64 = 500;

/// Drag-selection autoscroll cadence (60fps), live only while a drag is
/// actually past an edge.
const DRAG_SCROLL_FRAME_MS: u64 = 16;

/// How long a run of single-character edits keeps merging into one undo step.
/// A longer pause starts a fresh step, so undo rewinds in the bursts the user
/// actually typed rather than one character at a time.
const UNDO_COALESCE: Duration = Duration::from_millis(700);

/// Cap on retained undo steps — a long-lived field must not grow forever.
const UNDO_LIMIT: usize = 200;

/// Caret blink phase for a time since the last keystroke or caret move: solid
/// through the first half-period (a typing burst never blinks, because every
/// keystroke resets the anchor), alternating after.
#[must_use]
pub fn caret_visible(ms_since_activity: u64) -> bool {
    (ms_since_activity / CARET_BLINK_MS).is_multiple_of(2)
}

/// Content height for a wrapped-line count. The floor is one row: an empty
/// field still occupies a line.
#[must_use]
pub fn content_height(wrapped_lines: usize) -> f32 {
    wrapped_lines.max(1) as f32 * LINE_HEIGHT
}

/// How far the field can scroll inside itself before the content runs out.
fn max_scroll(content_height: f32, viewport_height: f32) -> f32 {
    (content_height - viewport_height).max(0.0)
}

/// Apply a wheel delta to a top-origin offset. Positive deltas scroll toward
/// the start, matching gpui's own list and div behaviour.
fn scroll_offset(current: f32, delta_y: f32, content: f32, viewport: f32) -> f32 {
    (current - delta_y).clamp(0.0, max_scroll(content, viewport))
}

/// Minimally adjust the viewport so the caret row is fully visible — never
/// recentres, so typing in the middle of a long draft does not jump.
fn scroll_offset_for_cursor(
    current: f32,
    cursor_top: f32,
    cursor_height: f32,
    content: f32,
    viewport: f32,
) -> f32 {
    let mut next = current;
    if cursor_top < next {
        next = cursor_top;
    } else if cursor_top + cursor_height > next + viewport {
        next = cursor_top + cursor_height - viewport;
    }
    next.clamp(0.0, max_scroll(content, viewport))
}

/// Flatten highlight spans into the contiguous run lengths the shaper needs:
/// every byte of `len` is covered exactly once, colored where a span claims it
/// and `None` (the style's own ink) in the gaps. Spans are clamped to `len`,
/// walked in start order, and a span overlapping one already emitted loses the
/// overlap — the shaper cannot color a byte twice, so first claim wins.
fn run_lengths(len: usize, spans: &[(Range<usize>, Hsla)]) -> Vec<(usize, Option<Hsla>)> {
    let mut sorted: Vec<(Range<usize>, Hsla)> = spans
        .iter()
        .map(|(range, color)| (range.start.min(len)..range.end.min(len), *color))
        .filter(|(range, _)| range.start < range.end)
        .collect();
    sorted.sort_by_key(|(range, _)| range.start);

    let mut runs = Vec::with_capacity(sorted.len() * 2 + 1);
    let mut at = 0;
    for (range, color) in sorted {
        if range.end <= at {
            continue;
        }
        let start = range.start.max(at);
        if start > at {
            runs.push((start - at, None));
        }
        runs.push((range.end - start, Some(color)));
        at = range.end;
    }
    if at < len {
        runs.push((len - at, None));
    }
    runs
}

/// Per-frame drag-selection scroll. Distance past the edge increases speed,
/// capped at one text row per frame so crossing the boundary never jumps.
fn drag_scroll_delta(pointer_y: f32, top: f32, bottom: f32, line_height: f32) -> f32 {
    let distance = if pointer_y < top {
        pointer_y - top
    } else if pointer_y > bottom {
        pointer_y - bottom
    } else {
        return 0.0;
    };
    distance.signum() * (distance.abs() * 0.2).clamp(1.0, line_height)
}

// -- actions and bindings ----------------------------------------------------

actions!(
    luma_text_input,
    [
        Backspace,
        Delete,
        Left,
        Right,
        Up,
        Down,
        SelectLeft,
        SelectRight,
        SelectUp,
        SelectDown,
        SelectAll,
        Home,
        End,
        SelectHome,
        SelectEnd,
        DocStart,
        DocEnd,
        SelectDocStart,
        SelectDocEnd,
        WordLeft,
        WordRight,
        SelectWordLeft,
        SelectWordRight,
        DeleteWordLeft,
        DeleteWordRight,
        DeleteToLineStart,
        DeleteToLineEnd,
        Copy,
        Cut,
        Paste,
        Newline,
        Submit,
        Cancel,
        Undo,
        Redo,
    ]
);

/// The key context a multi-line field declares, and the one its bindings are
/// scoped to. Bare — the element pairs it with `TextInput` itself.
pub const COMPOSER_CONTEXT: &str = "Composer";
/// The key context a single-line search field declares. Its keymap binds
/// text-editing keys **only**: bare arrows and `enter` stay unbound so the
/// surrounding picker frame keeps its navigation.
pub const SEARCH_CONTEXT: &str = "PickerSearch";

/// Which keymap a field lives under, and whether `enter` inserts or submits.
///
/// One enum rather than two booleans: "multi-line" and "binds enter" are not
/// independent facts, and a field that was one without the other is a state
/// neither caller wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// A composer: grows, `shift-enter` inserts a newline, `enter` submits.
    Composer,
    /// A picker's filter: one line, and navigation keys bubble past it.
    Search,
}

impl Mode {
    /// The bare context its bindings are scoped to.
    #[must_use]
    pub fn context(self) -> &'static str {
        match self {
            Mode::Composer => COMPOSER_CONTEXT,
            Mode::Search => SEARCH_CONTEXT,
        }
    }

    /// What the element declares: its own context *and* [`crate::TEXT_INPUT`],
    /// so this field's bindings fire while every app binding excluded by
    /// `&& !TextInput` stays masked. The pairing is spelled once, here.
    #[must_use]
    pub fn key_context(self) -> &'static str {
        match self {
            Mode::Composer => "Composer TextInput",
            Mode::Search => "PickerSearch TextInput",
        }
    }
}

/// Bind the text-field keymap. Call once at app boot, before any field renders.
pub fn init(cx: &mut App) {
    // Word-level editing is Option on macOS, Ctrl elsewhere.
    let word = if cfg!(target_os = "macos") {
        "alt"
    } else {
        "ctrl"
    };

    // Shared by both contexts: nothing here is a key a picker navigates with.
    let editing = |ctx: Option<&'static str>| {
        let mut bindings = vec![
            KeyBinding::new("backspace", Backspace, ctx),
            KeyBinding::new("delete", Delete, ctx),
            KeyBinding::new("home", Home, ctx),
            KeyBinding::new("end", End, ctx),
            KeyBinding::new("shift-left", SelectLeft, ctx),
            KeyBinding::new("shift-right", SelectRight, ctx),
            KeyBinding::new("shift-home", SelectHome, ctx),
            KeyBinding::new("shift-end", SelectEnd, ctx),
            // A laptop keyboard has no home/end, so Cmd+arrow is the only way
            // to either edge. Modifier-qualified motion is safe even in the
            // search context: a picker navigates with the *bare* arrows.
            KeyBinding::new("cmd-left", Home, ctx),
            KeyBinding::new("cmd-right", End, ctx),
            KeyBinding::new("shift-cmd-left", SelectHome, ctx),
            KeyBinding::new("shift-cmd-right", SelectEnd, ctx),
            KeyBinding::new("cmd-backspace", DeleteToLineStart, ctx),
            KeyBinding::new("cmd-delete", DeleteToLineEnd, ctx),
            KeyBinding::new(&format!("{word}-backspace"), DeleteWordLeft, ctx),
            KeyBinding::new(&format!("{word}-delete"), DeleteWordRight, ctx),
            KeyBinding::new(&format!("{word}-left"), WordLeft, ctx),
            KeyBinding::new(&format!("{word}-right"), WordRight, ctx),
            KeyBinding::new(&format!("shift-{word}-left"), SelectWordLeft, ctx),
            KeyBinding::new(&format!("shift-{word}-right"), SelectWordRight, ctx),
        ];
        for prefix in ["cmd", "ctrl"] {
            bindings.push(KeyBinding::new(&format!("{prefix}-a"), SelectAll, ctx));
            bindings.push(KeyBinding::new(&format!("{prefix}-c"), Copy, ctx));
            bindings.push(KeyBinding::new(&format!("{prefix}-x"), Cut, ctx));
            bindings.push(KeyBinding::new(&format!("{prefix}-v"), Paste, ctx));
            bindings.push(KeyBinding::new(&format!("{prefix}-z"), Undo, ctx));
            bindings.push(KeyBinding::new(&format!("shift-{prefix}-z"), Redo, ctx));
        }
        bindings
    };

    let composer = Some(COMPOSER_CONTEXT);
    let mut bindings = editing(composer);
    bindings.extend([
        KeyBinding::new("enter", Submit, composer),
        KeyBinding::new("shift-enter", Newline, composer),
        KeyBinding::new("escape", Cancel, composer),
        KeyBinding::new("left", Left, composer),
        KeyBinding::new("right", Right, composer),
        KeyBinding::new("up", Up, composer),
        KeyBinding::new("down", Down, composer),
        KeyBinding::new("shift-up", SelectUp, composer),
        KeyBinding::new("shift-down", SelectDown, composer),
        KeyBinding::new("cmd-up", DocStart, composer),
        KeyBinding::new("cmd-down", DocEnd, composer),
        KeyBinding::new("shift-cmd-up", SelectDocStart, composer),
        KeyBinding::new("shift-cmd-down", SelectDocEnd, composer),
    ]);
    cx.bind_keys(editing(Some(SEARCH_CONTEXT)));
    cx.bind_keys(bindings);
}

// -- appearance --------------------------------------------------------------

/// What the field paints with. Four colours, passed in rather than read from a
/// theme, because this crate sits *under* every palette in the app — the chat's
/// roles and the instrument ladder both reach it, and a field that reached back
/// would invert the dependency.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Style {
    /// Typed text.
    pub text: Hsla,
    /// The placeholder, and the tone the field paints in while empty.
    pub placeholder: Hsla,
    /// The selection wash behind selected glyphs.
    pub selection: Hsla,
    /// The caret bar.
    pub caret: Hsla,
}

impl Default for Style {
    /// The instrument tier's own: the ladder's foreground over its muted grey.
    /// The chat overrides all four from its own palette.
    fn default() -> Self {
        let text: Hsla = ladder::foreground().into();
        let accent: Hsla = ladder::accent().into();
        Self {
            text,
            placeholder: ladder::muted_foreground().into(),
            selection: Hsla { a: 0.24, ..accent },
            caret: text,
        }
    }
}

// -- the entity --------------------------------------------------------------

/// What the field tells its host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The content changed, by any path.
    Edited,
    /// `enter` in a [`Mode::Composer`] field.
    Submitted,
    /// `escape` in a [`Mode::Composer`] field. The field does nothing with it
    /// itself — what escape *means* is the host's decision.
    Cancelled,
    /// The caret or the selection moved without an edit.
    CursorMoved,
    /// The field scrolled inside itself.
    ViewportChanged,
}

/// A restorable point in the field's history: text plus where the caret and
/// selection sat when the edit landed.
#[derive(Clone)]
struct EditSnapshot {
    content: String,
    selected_range: Range<usize>,
    selection_reversed: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EditKind {
    Insert,
    Delete,
}

/// Content, selection, IME marked text, and the measured layout that mouse
/// mapping and auto-grow are both read from.
pub struct TextInput {
    mode: Mode,
    style: Style,
    /// Content height past which the field scrolls inside itself instead of
    /// growing. Owned here rather than passed per-frame so the measured layout
    /// and the plate around it cannot disagree about where growth stops.
    max_content_height: f32,
    focus_handle: FocusHandle,
    content: String,
    placeholder: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    is_selecting: bool,
    drag_position: Option<Point<Pixels>>,
    /// Bumped on every press and release, so a stale autoscroll loop from a
    /// finished drag retires itself instead of fighting the live one.
    drag_generation: u64,
    drag_autoscroll_active: bool,
    /// Vertical scroll inside the field once content exceeds its box.
    scroll_top: f32,
    /// Normally keeps the caret visible through edits and rewraps. A manual
    /// wheel scroll pauses it until the next caret move or edit.
    follow_cursor: bool,
    // -- measured during layout/paint --
    last_lines: Vec<WrappedLine>,
    line_starts: Vec<usize>,
    last_bounds: Option<Bounds<Pixels>>,
    line_height: Pixels,
    measured_height: f32,
    max_line_width: f32,
    last_width: f32,
    /// Bumped once per layout pass. A host that flips its layout on a measured
    /// width uses this to apply at most one flip per measurement.
    layout_epoch: u64,
    showing_placeholder: bool,
    /// Blink anchor, reset on every keystroke and caret move.
    blink_anchor: Instant,
    /// Half-period repaint driver, alive only while focused.
    blink_task: Option<Task<()>>,
    undo_stack: Vec<EditSnapshot>,
    redo_stack: Vec<EditSnapshot>,
    /// Kind, trailing offset and time of the last edit — the merge test that
    /// decides whether the next edit extends the current undo step.
    last_edit: Option<(EditKind, usize, Instant)>,
    /// What Copy means when this field has no selection of its own.
    copy_fallback: Option<Rc<dyn Fn() -> Option<String>>>,
    /// Semantic coloring, asked of the host per layout — see [`set_highlight`].
    ///
    /// [`set_highlight`]: Self::set_highlight
    highlight: Option<Highlighter>,
}

/// The highlight hook's shape: the current text in, colored byte spans out.
type Highlighter = Rc<dyn Fn(&str) -> Vec<(Range<usize>, Hsla)>>;

impl TextInput {
    /// A multi-line composer field that grows to `max_content_height` and
    /// scrolls inside itself past it.
    pub fn composer(
        placeholder: impl Into<SharedString>,
        max_content_height: f32,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new(Mode::Composer, placeholder, max_content_height, cx)
    }

    /// A single-line picker filter, whose navigation keys bubble. One row, by
    /// construction — a search field that could grow would move the list under
    /// the pointer as the query is typed.
    pub fn search(placeholder: impl Into<SharedString>, cx: &mut Context<Self>) -> Self {
        Self::new(Mode::Search, placeholder, LINE_HEIGHT, cx)
    }

    fn new(
        mode: Mode,
        placeholder: impl Into<SharedString>,
        max_content_height: f32,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            mode,
            style: Style::default(),
            max_content_height,
            // A text field is a control, so its handle joins the tab ring. A
            // field reachable only by click strands a keyboard user in
            // whatever list it filters — and the stop has to be the field's
            // OWN element: a host that wrapped it in a stop of its own would
            // put the ring's entry on a different element than the one focus
            // lands on, and reverse-tab off the first stop stops wrapping.
            focus_handle: cx.focus_handle().tab_stop(true),
            content: String::new(),
            placeholder: placeholder.into(),
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            is_selecting: false,
            drag_position: None,
            drag_generation: 0,
            drag_autoscroll_active: false,
            scroll_top: 0.0,
            follow_cursor: true,
            last_lines: Vec::new(),
            line_starts: vec![0],
            last_bounds: None,
            line_height: px(LINE_HEIGHT),
            measured_height: LINE_HEIGHT,
            max_line_width: 0.0,
            last_width: 0.0,
            layout_epoch: 0,
            showing_placeholder: true,
            blink_anchor: Instant::now(),
            blink_task: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            last_edit: None,
            copy_fallback: None,
            highlight: None,
        }
    }

    /// What Copy should copy when this field holds no selection.
    ///
    /// The composer keeps focus while the reader is reading the transcript, so
    /// a bare ⌘C there means "copy what I selected in the thread" — but the
    /// field cannot know that, and reaching for the markdown crate to find out
    /// would point this crate at one that already depends on it. So the field
    /// asks outward and the host answers.
    pub fn set_copy_fallback(&mut self, ask: impl Fn() -> Option<String> + 'static) {
        self.copy_fallback = Some(Rc::new(ask));
    }

    /// Color spans of the content semantically — a group-expression field's
    /// tokens, say — without a second rendering path.
    ///
    /// The hook lives here rather than in the host because coloring happens at
    /// shaping time: the shaped lines are what the caret, the selection wash
    /// and mouse mapping all read, and a host that painted its own colored
    /// overlay would be maintaining a second layout that can disagree with the
    /// first (which is exactly the web implementation's transparent-input
    /// hack). The host stays the authority on *meaning*: it is asked for
    /// `(byte range, color)` spans against the current text on every layout.
    /// Uncovered bytes keep the style's own color; spans are clamped and, out
    /// of caution, ignored while an IME composition is marked — the underline
    /// run split matters more than a token color for those few keystrokes.
    pub fn set_highlight(
        &mut self,
        highlight: impl Fn(&str) -> Vec<(Range<usize>, Hsla)> + 'static,
        cx: &mut Context<Self>,
    ) {
        self.highlight = Some(Rc::new(highlight));
        cx.notify();
    }

    /// Paint the field with `style` instead of the instrument default.
    pub fn set_style(&mut self, style: Style, cx: &mut Context<Self>) {
        if self.style == style {
            return;
        }
        self.style = style;
        cx.notify();
    }

    // ---- what a host reads ----

    #[must_use]
    pub fn text(&self) -> &str {
        &self.content
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.content.is_empty()
    }

    #[must_use]
    pub fn has_newline(&self) -> bool {
        self.content.contains('\n')
    }

    /// Unwrapped width of the widest line — what a compact/expanded flip is
    /// decided on. Zero while the placeholder is showing: an empty field must
    /// not measure as wide as its own prompt.
    #[must_use]
    pub fn measured_text_width(&self) -> f32 {
        self.max_line_width
    }

    /// Shaped height of the content at the last measured width.
    #[must_use]
    pub fn measured_content_height(&self) -> f32 {
        self.measured_height
    }

    /// The width the field was last shaped at, or 0 before the first layout.
    #[must_use]
    pub fn measured_width(&self) -> f32 {
        self.last_width
    }

    /// Which layout pass produced the current measurements. Strictly
    /// increasing; a host compares it against the epoch of its last decision
    /// so it never re-decides on numbers it has already consumed.
    #[must_use]
    pub fn layout_epoch(&self) -> u64 {
        self.layout_epoch
    }

    pub fn set_placeholder(
        &mut self,
        placeholder: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        self.placeholder = placeholder.into();
        cx.notify();
    }

    /// Replace the whole document. Undo does not reach back past it: a draft
    /// load or a clear-on-submit is a *new* document, not an edit, and one that
    /// could be undone would restore text into a field that has moved on.
    pub fn set_text(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.content = text.into();
        let end = self.content.len();
        self.selected_range = end..end;
        self.selection_reversed = false;
        self.marked_range = None;
        self.scroll_top = 0.0;
        self.follow_cursor = true;
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.last_edit = None;
        self.reset_blink();
        cx.emit(Event::Edited);
        cx.notify();
    }

    // ---- blink ----

    fn reset_blink(&mut self) {
        self.blink_anchor = Instant::now();
    }

    /// Caret paint gate: focused, in an active window, in the "on" phase. Also
    /// arms the half-period repaint driver while focused and drops it on blur,
    /// so an unfocused field schedules no frames at all.
    fn caret_shown(&mut self, window: &Window, cx: &mut Context<Self>) -> bool {
        if !self.focus_handle.is_focused(window) || !window.is_window_active() {
            self.blink_task = None;
            return false;
        }
        if self.blink_task.is_none() {
            self.blink_task = Some(cx.spawn(async move |this, cx| loop {
                cx.background_executor()
                    .timer(Duration::from_millis(CARET_BLINK_MS))
                    .await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }));
        }
        caret_visible(self.blink_anchor.elapsed().as_millis() as u64)
    }

    // ---- undo ----

    fn snapshot(&self) -> EditSnapshot {
        EditSnapshot {
            content: self.content.clone(),
            selected_range: self.selected_range.clone(),
            selection_reversed: self.selection_reversed,
        }
    }

    fn push_undo(&mut self) {
        self.undo_stack.push(self.snapshot());
        if self.undo_stack.len() > UNDO_LIMIT {
            self.undo_stack.remove(0);
        }
        self.redo_stack.clear();
    }

    /// Called with the range about to be replaced, **before** the content
    /// changes, so the pushed snapshot is the pre-edit state.
    fn record_edit(&mut self, range: &Range<usize>, new_text: &str) {
        let kind = if new_text.is_empty() {
            EditKind::Delete
        } else {
            EditKind::Insert
        };
        // A run merges only while it stays single-character, contiguous with
        // the previous edit, of the same kind, and inside the idle window. A
        // pause, a word break, a paste or a caret jump all break the run, so
        // undo lands on a boundary the user recognises.
        let mergeable = match (kind, &self.last_edit) {
            (EditKind::Insert, Some((EditKind::Insert, at, when))) => {
                range.is_empty()
                    && range.start == *at
                    && new_text.chars().count() == 1
                    && !new_text.starts_with(['\n', ' ', '\t'])
                    && when.elapsed() < UNDO_COALESCE
            }
            (EditKind::Delete, Some((EditKind::Delete, at, when))) => {
                range.end == *at && when.elapsed() < UNDO_COALESCE
            }
            _ => false,
        };
        if mergeable {
            self.redo_stack.clear();
        } else {
            self.push_undo();
        }
        let tail = match kind {
            EditKind::Insert => range.start + new_text.len(),
            EditKind::Delete => range.start,
        };
        self.last_edit = Some((kind, tail, Instant::now()));
    }

    fn restore(&mut self, snapshot: EditSnapshot, cx: &mut Context<Self>) {
        self.content = snapshot.content;
        self.selected_range = snapshot.selected_range;
        self.selection_reversed = snapshot.selection_reversed;
        self.marked_range = None;
        self.follow_cursor = true;
        // Never merge a later edit into a step undo has just crossed.
        self.last_edit = None;
        self.reset_blink();
        cx.emit(Event::Edited);
        cx.notify();
    }

    fn undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        let Some(previous) = self.undo_stack.pop() else {
            return;
        };
        self.redo_stack.push(self.snapshot());
        self.restore(previous, cx);
    }

    fn redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
        let Some(next) = self.redo_stack.pop() else {
            return;
        };
        self.undo_stack.push(self.snapshot());
        self.restore(next, cx);
    }

    // ---- caret and selection ----

    #[must_use]
    pub fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.selected_range = offset..offset;
        self.follow_cursor = true;
        self.reset_blink();
        cx.emit(Event::CursorMoved);
        cx.notify();
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        if self.selection_reversed {
            self.selected_range.start = offset;
        } else {
            self.selected_range.end = offset;
        }
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        self.follow_cursor = true;
        self.reset_blink();
        cx.emit(Event::CursorMoved);
        cx.notify();
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(ix, _)| (ix < offset).then_some(ix))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(ix, _)| (ix > offset).then_some(ix))
            .unwrap_or(self.content.len())
    }

    fn previous_word_boundary(&self, offset: usize) -> usize {
        self.content
            .split_word_bound_indices()
            .rev()
            .find_map(|(ix, word)| (ix < offset && !word.trim().is_empty()).then_some(ix))
            .unwrap_or(0)
    }

    fn next_word_boundary(&self, offset: usize) -> usize {
        self.content
            .split_word_bound_indices()
            .find_map(|(ix, word)| {
                let end = ix + word.len();
                (end > offset && !word.trim().is_empty()).then_some(end)
            })
            .unwrap_or(self.content.len())
    }

    /// Byte range of the logical line containing `offset`.
    fn line_range_at(&self, offset: usize) -> Range<usize> {
        let start = self.content[..offset].rfind('\n').map_or(0, |i| i + 1);
        let end = self.content[offset..]
            .find('\n')
            .map_or(self.content.len(), |i| offset + i);
        start..end
    }

    /// Offset one wrapped row above or below the caret, keeping its x column.
    /// Clamps to the document edges, which is the platform's behaviour on the
    /// first and last line.
    fn vertical_target(&self, dir: f32) -> Option<usize> {
        let current = self.point_for_index(self.cursor_offset())?;
        let target_y = f32::from(current.y) + dir * f32::from(self.line_height);
        if target_y < 0.0 {
            return Some(0);
        }
        if target_y >= self.measured_height {
            return Some(self.content.len());
        }
        Some(self.index_for_point(point(current.x, px(target_y))))
    }

    // ---- editing ops ----

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let prev = self.previous_boundary(self.cursor_offset());
            if self.cursor_offset() == prev {
                return;
            }
            self.select_to(prev, cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let next = self.next_boundary(self.cursor_offset());
            if self.cursor_offset() == next {
                return;
            }
            self.select_to(next, cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let prev = self.previous_boundary(self.cursor_offset());
            self.move_to(prev, cx);
        } else {
            self.move_to(self.selected_range.start, cx);
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let next = self.next_boundary(self.selected_range.end);
            self.move_to(next, cx);
        } else {
            self.move_to(self.selected_range.end, cx);
        }
    }

    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.vertical_target(-1.0) {
            self.move_to(ix, cx);
        }
    }

    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.vertical_target(1.0) {
            self.move_to(ix, cx);
        }
    }

    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.vertical_target(-1.0) {
            self.select_to(ix, cx);
        }
    }

    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.vertical_target(1.0) {
            self.select_to(ix, cx);
        }
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx);
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        let line = self.line_range_at(self.cursor_offset());
        self.move_to(line.start, cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        let line = self.line_range_at(self.cursor_offset());
        self.move_to(line.end, cx);
    }

    fn select_home(&mut self, _: &SelectHome, _: &mut Window, cx: &mut Context<Self>) {
        let line = self.line_range_at(self.cursor_offset());
        self.select_to(line.start, cx);
    }

    fn select_end(&mut self, _: &SelectEnd, _: &mut Window, cx: &mut Context<Self>) {
        let line = self.line_range_at(self.cursor_offset());
        self.select_to(line.end, cx);
    }

    fn doc_start(&mut self, _: &DocStart, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
    }

    fn doc_end(&mut self, _: &DocEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), cx);
    }

    fn select_doc_start(&mut self, _: &SelectDocStart, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(0, cx);
    }

    fn select_doc_end(&mut self, _: &SelectDocEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.content.len(), cx);
    }

    fn word_left(&mut self, _: &WordLeft, _: &mut Window, cx: &mut Context<Self>) {
        let prev = self.previous_word_boundary(self.cursor_offset());
        self.move_to(prev, cx);
    }

    fn word_right(&mut self, _: &WordRight, _: &mut Window, cx: &mut Context<Self>) {
        let next = self.next_word_boundary(self.cursor_offset());
        self.move_to(next, cx);
    }

    fn select_word_left(&mut self, _: &SelectWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        let prev = self.previous_word_boundary(self.cursor_offset());
        self.select_to(prev, cx);
    }

    fn select_word_right(&mut self, _: &SelectWordRight, _: &mut Window, cx: &mut Context<Self>) {
        let next = self.next_word_boundary(self.cursor_offset());
        self.select_to(next, cx);
    }

    /// The Opt/Cmd + Delete family. With a live selection these delete the
    /// selection only, which is the platform's behaviour — the extend runs off
    /// the caret, not off the selection edge.
    fn delete_to(&mut self, offset: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            if self.cursor_offset() == offset {
                return;
            }
            self.select_to(offset, cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete_word_left(
        &mut self,
        _: &DeleteWordLeft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let prev = self.previous_word_boundary(self.cursor_offset());
        self.delete_to(prev, window, cx);
    }

    fn delete_word_right(
        &mut self,
        _: &DeleteWordRight,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let next = self.next_word_boundary(self.cursor_offset());
        self.delete_to(next, window, cx);
    }

    fn delete_to_line_start(
        &mut self,
        _: &DeleteToLineStart,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let start = self.line_range_at(self.cursor_offset()).start;
        self.delete_to(start, window, cx);
    }

    fn delete_to_line_end(
        &mut self,
        _: &DeleteToLineEnd,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let end = self.line_range_at(self.cursor_offset()).end;
        self.delete_to(end, window, cx);
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
            return;
        }
        // Nothing selected here — ask the host what Copy means instead. See
        // `set_copy_fallback` for why the question goes outward.
        if let Some(text) = self.copy_fallback.as_ref().and_then(|ask| ask()) {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
            self.replace_text_in_range(None, "", window, cx);
        }
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = cx.read_from_clipboard() else {
            return;
        };
        // Images and copied file paths are dropped rather than inserted: their
        // textual form is a debug rendering nobody meant to type. Staging them
        // is comet's attachment feature, which luma has no consumer for — when
        // it grows one, the branch belongs here, not at the call site.
        if item
            .entries
            .iter()
            .any(|entry| matches!(entry, ClipboardEntry::Image(_)))
        {
            return;
        }
        if let Some(text) = item.text() {
            self.replace_text_in_range(None, &text, window, cx);
        }
    }

    fn newline(&mut self, _: &Newline, window: &mut Window, cx: &mut Context<Self>) {
        self.replace_text_in_range(None, "\n", window, cx);
    }

    fn submit(&mut self, _: &Submit, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(Event::Submitted);
    }

    fn cancel(&mut self, _: &Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(Event::Cancelled);
    }

    // ---- geometry ----

    /// Content-local point for a byte index; y grows down from the content top.
    fn point_for_index(&self, index: usize) -> Option<Point<Pixels>> {
        for (line_ix, line) in self.last_lines.iter().enumerate() {
            let line_start = *self.line_starts.get(line_ix)?;
            if index < line_start {
                continue;
            }
            if index <= line_start + line.len() {
                let local = line.position_for_index(index - line_start, self.line_height)?;
                let y_offset: f32 = self
                    .last_lines
                    .iter()
                    .take(line_ix)
                    .map(|l| f32::from(l.size(self.line_height).height))
                    .sum();
                return Some(point(local.x, local.y + px(y_offset)));
            }
        }
        None
    }

    /// Byte index closest to a content-local point.
    fn index_for_point(&self, position: Point<Pixels>) -> usize {
        if self.showing_placeholder {
            return 0;
        }
        let mut y = f32::from(position.y);
        if y < 0.0 {
            return 0;
        }
        for (line_ix, line) in self.last_lines.iter().enumerate() {
            let height = f32::from(line.size(self.line_height).height);
            let line_start = self.line_starts.get(line_ix).copied().unwrap_or(0);
            if y < height || line_ix + 1 == self.last_lines.len() {
                let local = point(position.x, px(y.min(height - 1.0).max(0.0)));
                let ix = line
                    .closest_index_for_position(local, self.line_height)
                    .unwrap_or_else(|ix| ix);
                return (line_start + ix).min(self.content.len());
            }
            y -= height;
        }
        self.content.len()
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        let Some(bounds) = self.last_bounds else {
            return 0;
        };
        self.index_for_point(point(
            position.x - bounds.left(),
            position.y - bounds.top() + px(self.scroll_top),
        ))
    }

    // ---- mouse ----

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        self.is_selecting = true;
        self.drag_position = Some(event.position);
        self.drag_generation = self.drag_generation.wrapping_add(1);
        self.drag_autoscroll_active = false;
        let index = self.index_for_mouse_position(event.position);
        if event.modifiers.shift {
            self.select_to(index, cx);
        } else {
            self.move_to(index, cx);
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
        self.drag_position = None;
        self.drag_generation = self.drag_generation.wrapping_add(1);
        self.drag_autoscroll_active = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if !self.is_selecting {
            return;
        }
        self.drag_position = Some(event.position);
        let position = self.clamp_to_bounds(event.position);
        self.select_to(self.index_for_mouse_position(position), cx);
        if self.drag_scroll_delta(event.position) != 0.0 && !self.drag_autoscroll_active {
            self.start_drag_autoscroll(cx);
        }
    }

    fn clamp_to_bounds(&self, position: Point<Pixels>) -> Point<Pixels> {
        let Some(bounds) = self.last_bounds else {
            return position;
        };
        point(
            position.x.clamp(bounds.left(), bounds.right() - px(0.5)),
            position.y.clamp(bounds.top(), bounds.bottom() - px(0.5)),
        )
    }

    fn drag_scroll_delta(&self, position: Point<Pixels>) -> f32 {
        let Some(bounds) = self.last_bounds else {
            return 0.0;
        };
        drag_scroll_delta(
            f32::from(position.y),
            f32::from(bounds.top()),
            f32::from(bounds.bottom()),
            f32::from(self.line_height),
        )
    }

    fn start_drag_autoscroll(&mut self, cx: &mut Context<Self>) {
        self.drag_autoscroll_active = true;
        let generation = self.drag_generation;
        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(DRAG_SCROLL_FRAME_MS))
                .await;
            let keep_going = this
                .update(cx, |input, cx| input.drag_autoscroll_tick(generation, cx))
                .unwrap_or(false);
            if !keep_going {
                break;
            }
        })
        .detach();
    }

    fn drag_autoscroll_tick(&mut self, generation: u64, cx: &mut Context<Self>) -> bool {
        if !self.is_selecting || self.drag_generation != generation {
            return false;
        }
        let (Some(position), Some(bounds)) = (self.drag_position, self.last_bounds) else {
            self.drag_autoscroll_active = false;
            return false;
        };
        let delta = self.drag_scroll_delta(position);
        if delta == 0.0 {
            self.drag_autoscroll_active = false;
            return false;
        }
        let next = (self.scroll_top + delta).clamp(
            0.0,
            max_scroll(self.measured_height, f32::from(bounds.size.height)),
        );
        if next == self.scroll_top {
            self.drag_autoscroll_active = false;
            return false;
        }
        self.scroll_top = next;
        let edge = self.clamp_to_bounds(position);
        self.select_to(self.index_for_mouse_position(edge), cx);
        // Selection motion normally resumes caret following; during an edge
        // drag the autoscroll loop owns the viewport instead.
        self.follow_cursor = false;
        true
    }

    fn on_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(bounds) = self.last_bounds else {
            return;
        };
        let viewport = f32::from(bounds.size.height);
        let delta_y = f32::from(event.delta.pixel_delta(self.line_height).y);
        let next = scroll_offset(self.scroll_top, delta_y, self.measured_height, viewport);
        if next == self.scroll_top {
            // Overscroll containment: while the field itself is scrollable,
            // swallow the wheel even at its boundary, so a fast flick inside a
            // long draft never chains into the transcript behind it.
            if delta_y != 0.0 && max_scroll(self.measured_height, viewport) > 0.0 {
                cx.stop_propagation();
            }
            return;
        }
        self.scroll_top = next;
        self.follow_cursor = false;
        cx.stop_propagation();
        cx.emit(Event::ViewportChanged);
        cx.notify();
    }

    // ---- utf16 mapping, for the platform's IME ----

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8 = 0;
        let mut utf16 = 0;
        for ch in self.content.chars() {
            if utf16 >= offset {
                break;
            }
            utf16 += ch.len_utf16();
            utf8 += ch.len_utf8();
        }
        utf8
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16 = 0;
        let mut utf8 = 0;
        for ch in self.content.chars() {
            if utf8 >= offset {
                break;
            }
            utf8 += ch.len_utf8();
            utf16 += ch.len_utf16();
        }
        utf16
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range.start)..self.offset_from_utf16(range.end)
    }

    // ---- measurement ----

    /// Shape the text at a width, store the measured layout, and return the
    /// content height. Called from the element's measured-layout closure — the
    /// one place shaping happens, so mouse mapping and auto-grow can never read
    /// two different layouts.
    fn layout_text(&mut self, width: Pixels, style: &TextStyle, window: &mut Window) -> f32 {
        let (display, is_placeholder) = if self.content.is_empty() {
            (self.placeholder.clone(), true)
        } else {
            (SharedString::from(self.content.clone()), false)
        };
        let font_size = style.font_size.to_pixels(window.rem_size());
        self.line_height = px(LINE_HEIGHT);

        let run_for = |len: usize, underline: bool| TextRun {
            len,
            font: style.font(),
            color: style.color,
            background_color: None,
            underline: underline.then_some(UnderlineStyle {
                color: Some(style.color),
                thickness: px(1.0),
                wavy: false,
            }),
            strikethrough: None,
        };
        let colored_run = |len: usize, color: Hsla| TextRun {
            color,
            ..run_for(len, false)
        };
        // The IME's in-composition run is underlined; everything else is one
        // run. Filtering empties matters — a zero-length run makes the shaper
        // disagree with the byte offsets every other method here uses.
        let runs: Vec<TextRun> = match self.marked_range.as_ref() {
            Some(marked) if !is_placeholder => [
                run_for(marked.start, false),
                run_for(marked.end.saturating_sub(marked.start), true),
                run_for(display.len().saturating_sub(marked.end), false),
            ]
            .into_iter()
            .filter(|run| run.len > 0)
            .collect(),
            None if !is_placeholder && self.highlight.is_some() => {
                let spans = (self.highlight.as_ref().unwrap())(&display);
                run_lengths(display.len(), &spans)
                    .into_iter()
                    .map(|(len, color)| match color {
                        Some(color) => colored_run(len, color),
                        None => run_for(len, false),
                    })
                    .collect()
            }
            _ => vec![run_for(display.len(), false)],
        };

        let lines = window
            .text_system()
            .shape_text(display, font_size, &runs, Some(width), None)
            .map(|shaped| shaped.into_vec())
            .unwrap_or_default();

        // One shaped line per `\n`-split logical line; the `+ 1` is the newline
        // the split consumed.
        let mut line_starts = Vec::with_capacity(lines.len());
        let mut at = 0usize;
        for line in &lines {
            line_starts.push(at);
            at += line.len() + 1;
        }
        if line_starts.is_empty() {
            line_starts.push(0);
        }

        let height: f32 = lines
            .iter()
            .map(|l| f32::from(l.size(self.line_height).height))
            .sum();
        let widest: f32 = lines
            .iter()
            .map(|l| f32::from(l.unwrapped_layout.width))
            .fold(0.0, f32::max);

        self.showing_placeholder = is_placeholder;
        self.last_lines = lines;
        self.line_starts = line_starts;
        self.measured_height = height.max(LINE_HEIGHT);
        self.max_line_width = if is_placeholder { 0.0 } else { widest };
        self.last_width = f32::from(width);
        self.layout_epoch += 1;
        self.measured_height
    }

    /// Keep the caret visible when the content exceeds the element's height.
    /// Returns whether the viewport actually moved.
    fn clamp_scroll(&mut self, element_height: f32) -> bool {
        let previous = self.scroll_top;
        if self.follow_cursor {
            if let Some(cursor) = self.point_for_index(self.cursor_offset()) {
                self.scroll_top = scroll_offset_for_cursor(
                    self.scroll_top,
                    f32::from(cursor.y),
                    f32::from(self.line_height),
                    self.measured_height,
                    element_height,
                );
            }
        }
        self.scroll_top = self
            .scroll_top
            .clamp(0.0, max_scroll(self.measured_height, element_height));
        self.scroll_top != previous
    }
}

impl EventEmitter<Event> for TextInput {}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content.get(range)?.to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .or_else(|| self.marked_range.clone())
            .unwrap_or_else(|| self.selected_range.clone());
        // An IME commit is the tail of a composition whose pre-composition
        // snapshot was already taken below; recording here would pin undo to
        // the half-composed text instead of to the text before it existed.
        if self.marked_range.is_none() {
            self.record_edit(&range, new_text);
        }
        self.content =
            self.content[..range.start].to_owned() + new_text + &self.content[range.end..];
        let cursor = range.start + new_text.len();
        self.selected_range = cursor..cursor;
        self.selection_reversed = false;
        self.marked_range.take();
        self.follow_cursor = true;
        self.reset_blink();
        cx.emit(Event::Edited);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .or_else(|| self.marked_range.clone())
            .unwrap_or_else(|| self.selected_range.clone());
        // First keystroke of a composition: snapshot the text as it stood
        // before any of it existed, so one undo drops the whole composition.
        if self.marked_range.is_none() {
            self.push_undo();
            self.last_edit = None;
        }
        self.content =
            self.content[..range.start].to_owned() + new_text + &self.content[range.end..];
        self.marked_range =
            (!new_text.is_empty()).then(|| range.start..range.start + new_text.len());
        self.selected_range = new_selected_range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .map_or_else(
                || range.start + new_text.len()..range.start + new_text.len(),
                |r| r.start + range.start..r.end + range.start,
            );
        self.follow_cursor = true;
        self.reset_blink();
        cx.emit(Event::Edited);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let range = self.range_from_utf16(&range_utf16);
        let start = self.point_for_index(range.start)?;
        Some(Bounds::new(
            point(
                bounds.left() + start.x,
                bounds.top() + start.y - px(self.scroll_top),
            ),
            size(px(2.0), self.line_height),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point_in_window: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.offset_to_utf16(self.index_for_mouse_position(point_in_window)))
    }
}

impl Render for TextInput {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let color = if self.content.is_empty() {
            self.style.placeholder
        } else {
            self.style.text
        };
        let max_content_height = self.max_content_height;
        div()
            .key_context(self.mode.key_context())
            .track_focus(&self.focus_handle)
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::select_home))
            .on_action(cx.listener(Self::select_end))
            .on_action(cx.listener(Self::doc_start))
            .on_action(cx.listener(Self::doc_end))
            .on_action(cx.listener(Self::select_doc_start))
            .on_action(cx.listener(Self::select_doc_end))
            .on_action(cx.listener(Self::word_left))
            .on_action(cx.listener(Self::word_right))
            .on_action(cx.listener(Self::select_word_left))
            .on_action(cx.listener(Self::select_word_right))
            .on_action(cx.listener(Self::delete_word_left))
            .on_action(cx.listener(Self::delete_word_right))
            .on_action(cx.listener(Self::delete_to_line_start))
            .on_action(cx.listener(Self::delete_to_line_end))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::newline))
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            .w_full()
            .text_size(px(TEXT_SIZE))
            .line_height(px(LINE_HEIGHT))
            .text_color(color)
            .child(TextElement {
                input: cx.entity(),
                max_content_height,
            })
    }
}

// -- the measured element ----------------------------------------------------

/// Measured auto-grow layout plus shaped-line painting.
///
/// A hand-rolled element rather than a `div` full of text because the caret,
/// the selection wash and the mouse mapping all need the *same* shaped lines,
/// and the only way to have one shaping is to own the layout pass.
struct TextElement {
    input: Entity<TextInput>,
    /// Content height past which the field scrolls inside itself instead of
    /// growing.
    max_content_height: f32,
}

struct TextPrepaint {
    caret: Option<PaintQuad>,
    selection: Vec<PaintQuad>,
}

impl IntoElement for TextElement {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl gpui::Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = TextPrepaint;

    fn id(&self) -> Option<gpui::ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        _: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = gpui::Style::default();
        style.size.width = relative(1.0).into();
        let input = self.input.clone();
        let text_style = window.text_style();
        let max_content = self.max_content_height;
        let layout_id =
            window.request_measured_layout(style, move |known, available, window, cx| {
                let width = known.width.unwrap_or(match available.width {
                    gpui::AvailableSpace::Definite(width) => width,
                    _ => px(320.0),
                });
                let height =
                    input.update(cx, |input, _| input.layout_text(width, &text_style, window));
                size(width, px(height.min(max_content)))
            });
        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        self.input.update(cx, |input, cx| {
            if input.clamp_scroll(f32::from(bounds.size.height)) {
                cx.emit(Event::ViewportChanged);
            }
            input.last_bounds = Some(bounds);
        });
        let input = self.input.read(cx);
        let origin = point(bounds.left(), bounds.top() - px(input.scroll_top));
        let line_height = input.line_height;

        let mut selection = Vec::new();
        let mut caret = None;
        if input.selected_range.is_empty() || input.showing_placeholder {
            caret = input
                .point_for_index(input.cursor_offset())
                .map(|p| point(origin.x + p.x, origin.y + p.y))
                .or_else(|| input.showing_placeholder.then_some(origin))
                .map(|at| {
                    fill(
                        Bounds::new(at, size(px(2.0), line_height)),
                        input.style.caret,
                    )
                });
        } else if let (Some(start), Some(end)) = (
            input.point_for_index(input.selected_range.start),
            input.point_for_index(input.selected_range.end),
        ) {
            let wash = input.style.selection;
            if start.y == end.y {
                selection.push(fill(
                    Bounds::from_corners(
                        point(origin.x + start.x, origin.y + start.y),
                        point(origin.x + end.x, origin.y + start.y + line_height),
                    ),
                    wash,
                ));
            } else {
                // First visual row, the full rows between, then the last.
                selection.push(fill(
                    Bounds::from_corners(
                        point(origin.x + start.x, origin.y + start.y),
                        point(bounds.right(), origin.y + start.y + line_height),
                    ),
                    wash,
                ));
                if end.y > start.y + line_height {
                    selection.push(fill(
                        Bounds::from_corners(
                            point(origin.x, origin.y + start.y + line_height),
                            point(bounds.right(), origin.y + end.y),
                        ),
                        wash,
                    ));
                }
                selection.push(fill(
                    Bounds::from_corners(
                        point(origin.x, origin.y + end.y),
                        point(origin.x + end.x, origin.y + end.y + line_height),
                    ),
                    wash,
                ));
            }
        }
        TextPrepaint { caret, selection }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        let input = self.input.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
            if phase == DispatchPhase::Bubble {
                input.update(cx, |input, cx| input.on_mouse_move(event, cx));
            }
        });

        // `WrappedLine` is not `Clone`: take the shaped lines out of the entity
        // for the paint, then put them back for the next frame's mouse mapping.
        let (lines, line_height, scroll) = self.input.update(cx, |input, _| {
            (
                std::mem::take(&mut input.last_lines),
                input.line_height,
                input.scroll_top,
            )
        });

        window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
            for quad in prepaint.selection.drain(..) {
                window.paint_quad(quad);
            }
            let mut y = bounds.top() - px(scroll);
            for line in &lines {
                let height = line.size(line_height).height;
                let _ = line.paint(
                    point(bounds.left(), y),
                    line_height,
                    gpui::TextAlign::Left,
                    Some(bounds),
                    window,
                    cx,
                );
                y += height;
            }
            if self
                .input
                .update(cx, |input, cx| input.caret_shown(window, cx))
            {
                if let Some(caret) = prepaint.caret.take() {
                    window.paint_quad(caret);
                }
            }
        });
        self.input.update(cx, |input, _| input.last_lines = lines);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Solid through the first half-period, then alternating. A typing burst
    /// resets the anchor, so this is also "typing never blinks".
    #[test]
    fn the_caret_is_solid_for_one_half_period_then_alternates() {
        assert!(caret_visible(0));
        assert!(caret_visible(CARET_BLINK_MS - 1));
        assert!(!caret_visible(CARET_BLINK_MS));
        assert!(caret_visible(2 * CARET_BLINK_MS));
    }

    /// An empty field still occupies a line — a zero-height text box would
    /// collapse the plate around it.
    #[test]
    fn content_height_floors_at_one_row() {
        assert_eq!(content_height(0), LINE_HEIGHT);
        assert_eq!(content_height(1), LINE_HEIGHT);
        assert_eq!(content_height(3), 3.0 * LINE_HEIGHT);
    }

    /// Content that fits cannot scroll, so the field never traps a wheel event
    /// it had no use for.
    #[test]
    fn a_field_that_fits_has_no_scroll_range() {
        assert_eq!(max_scroll(40.0, 100.0), 0.0);
        assert_eq!(max_scroll(140.0, 100.0), 40.0);
        assert_eq!(scroll_offset(0.0, -50.0, 40.0, 100.0), 0.0);
    }

    /// Caret following is *minimal*: a caret already in view does not move the
    /// viewport, and one just outside moves it exactly far enough.
    #[test]
    fn following_the_caret_moves_the_viewport_as_little_as_possible() {
        assert_eq!(
            scroll_offset_for_cursor(20.0, 30.0, 20.0, 300.0, 100.0),
            20.0
        );
        assert_eq!(
            scroll_offset_for_cursor(20.0, 10.0, 20.0, 300.0, 100.0),
            10.0
        );
        assert_eq!(
            scroll_offset_for_cursor(0.0, 110.0, 20.0, 300.0, 100.0),
            30.0
        );
    }

    /// Inside the box nothing scrolls; outside it, speed grows with distance
    /// but never exceeds one row per frame.
    #[test]
    fn drag_autoscroll_is_capped_at_one_row_per_frame() {
        assert_eq!(drag_scroll_delta(50.0, 0.0, 100.0, 22.75), 0.0);
        assert_eq!(drag_scroll_delta(-1000.0, 0.0, 100.0, 22.75), -22.75);
        assert_eq!(drag_scroll_delta(1000.0, 0.0, 100.0, 22.75), 22.75);
        assert_eq!(drag_scroll_delta(-1.0, 0.0, 100.0, 22.75), -1.0);
    }

    /// Highlight spans flatten into runs that tile the string exactly: gaps
    /// fill with the default ink, spans clamp to the text, and an overlap
    /// loses to the earlier claim instead of coloring a byte twice — the
    /// shaper's preconditions, which is what this helper exists to hold.
    #[test]
    fn highlight_runs_tile_the_text() {
        let red = gpui::red();
        let blue = gpui::blue();
        let runs = run_lengths(10, &[(2..4, red), (7..9, blue)]);
        assert_eq!(
            runs,
            vec![
                (2, None),
                (2, Some(red)),
                (3, None),
                (2, Some(blue)),
                (1, None)
            ]
        );
        assert_eq!(runs.iter().map(|(len, _)| len).sum::<usize>(), 10);

        // Out of range clamps; fully out of range vanishes.
        assert_eq!(
            run_lengths(4, &[(2..99, red)]),
            vec![(2, None), (2, Some(red))]
        );
        assert_eq!(run_lengths(4, &[(9..12, red)]), vec![(4, None)]);
        // Overlap: first claim wins, the rest of the later span survives.
        assert_eq!(
            run_lengths(6, &[(0..4, red), (2..6, blue)]),
            vec![(4, Some(red)), (2, Some(blue))]
        );
        // No spans is one plain run; empty text is no runs at all.
        assert_eq!(run_lengths(5, &[]), vec![(5, None)]);
        assert_eq!(
            run_lengths(0, &[(0..3, red)]),
            Vec::<(usize, Option<Hsla>)>::new()
        );
    }

    /// The two contexts are a pair, and both halves have to be spelled: the
    /// field's own name so its bindings fire, and `TextInput` so every app
    /// binding excluded by `&& !TextInput` stays masked while typing.
    #[test]
    fn every_mode_declares_both_its_own_context_and_the_text_input_guard() {
        for mode in [Mode::Composer, Mode::Search] {
            let declared = mode.key_context();
            assert!(
                declared.split(' ').any(|name| name == mode.context()),
                "{declared} does not declare {}",
                mode.context()
            );
            assert!(
                declared.split(' ').any(|name| name == crate::TEXT_INPUT),
                "{declared} does not mask app bindings"
            );
        }
    }
}
