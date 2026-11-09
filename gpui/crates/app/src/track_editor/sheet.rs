//! Pattern controls: Program's inspector under the visualizer, Direct's
//! graph page in the timeline's place, and the Perform tab's pattern editor.
//! In Program the inspector is always open and has its own scrolling
//! viewport, independent of timeline height.
//! With a subject it shows its name, selection, blend and graph (see
//! [`graph`]); with none it shows the preset browser. Selection retargets the
//! existing controls; graph writes keep their debounced history and
//! persistence path.
//!
//! The sheet is open in a [`Host`]: a score's track editor, whose subject is
//! its selected clips, or a Perform tab, whose subject is one pattern of its
//! set. Everything the sheet reads of its subject comes through
//! [`Host::subject`], and every edit through one funnel,
//! [`Luma::sheet_live`], which is where the two hosts part.

use luma_patterns::clip_graph::{ClipGraph, Input, Kind};
use luma_patterns::Selection;
use luma_ui::arg::arg_row;
use luma_ui::arg::color::{ColorArg, ColorArgEditor, ColorArgEvent};
use luma_ui::arg::expression::{ExpressionEvent, GroupExpressionEditor};
use luma_ui::arg::gradient::{Gradient, GradientStop, Light};
use luma_ui::arg::number::{DraftedNumber, NumberEvent};
use luma_ui::arg::select::luma_arg_select;
use luma_ui::float::MenuVisibility;
use luma_ui::text_input::{self, TextInput};
use luma_ui::CONTROL_HEIGHT;

use super::*;
use crate::perform::pattern_sheet::PatternSheet;

mod browser;
pub(super) mod graph;

pub(crate) use browser::Audition;
pub(super) use browser::{DropGhost, PresetDrag};

/// Air between one row and the next, and between the sheet's bands.
const ROW_GAP: f32 = 14.;

/// A full-bleed control's width inside the sheet.
const FIELD_W: f32 = luma_ui::sheet::CONTENT_WIDTH;

/// The expression field at its widest, and the least its suggestion menu is
/// wide; it shares its row with the fixture-picker chip.
const EXPR_W: f32 = FIELD_W - 62.;

/// The trailing edge a burst of live edits is committed on.
const ARG_FLUSH: Duration = Duration::from_millis(250);

// -- state --------------------------------------------------------------------

/// The sheet's own state, owned by its host.
pub(crate) struct State {
    /// The venue's group names, for the expression editor's autocomplete.
    groups: Groups,
    /// Controls and readings for the current selection.
    built: Option<Built>,
    /// The preset browser, shown while no clip is selected.
    pub(super) browser: browser::State,
    /// Which sheet-owned menu is open. One at a time — opening one closes the
    /// rest, which is what a single field states for free.
    open: Option<Menu>,
    /// What `open` was when the sheet last synced, so a close is seen.
    was_open: Option<Menu>,
    /// The menu that just closed, playing its exit.
    closing: Option<(Menu, MenuVisibility)>,
    /// A live burst is running: a trailing commit is owed (and in a track
    /// editor, its checkpoint is recorded).
    burst: bool,
    /// Debounce generation for the trailing commit; each live edit retires
    /// the timer before it.
    flush_gen: u64,
    /// The value an input held before it was wired, by node and input, so
    /// taking the wire out gives it back.
    last: HashMap<(String, String), Input>,
    /// Why the last graph edit was refused, in the checker's words.
    error: Option<String>,
    /// The graph canvas's view, gesture and unwired nodes.
    canvas: graph::canvas::State,
    /// The heads of the subject's selection, for the head marks of a strip
    /// across space.
    heads: Heads,
}

/// The heads a selection resolves to, asked once per venue, selection,
/// seed and laser element count. `cells` is `None` until the answer lands.
#[derive(Default)]
struct Heads {
    key: Option<(String, String, u64, Option<usize>)>,
    cells: Option<Rc<[luma_patterns::Cell]>>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            groups: Groups::NotAsked,
            built: None,
            browser: browser::State::default(),
            open: None,
            was_open: None,
            closing: None,
            burst: false,
            flush_gen: 0,
            last: HashMap::new(),
            error: None,
            canvas: graph::canvas::State::default(),
            heads: Heads::default(),
        }
    }
}

impl State {
    pub(super) fn invalidate(&mut self) {
        self.built = None;
    }

    /// Whether live edits are waiting on their trailing write.
    pub(crate) fn owes_write(&self) -> bool {
        self.burst
    }

    /// Close whichever menu the sheet has up, reporting whether there was one.
    pub(crate) fn dismiss_menu(&mut self) -> bool {
        self.open.take().is_some()
            | self.canvas.menu.take().is_some()
            | self.canvas.card_menu.take().is_some()
            | self.canvas.cancel()
    }
}

enum Groups {
    NotAsked,
    Loading,
    Ready(Rc<Vec<SharedString>>),
}

/// Which sheet-owned menu is up. The color editor's two menus are its own.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Menu {
    Blend,
    /// An input's source chip, by [`graph`]'s menu key.
    Source(usize),
    /// The same chip, listing the nodes it may share.
    Link(usize),
}

/// What the entities were built for, the entities themselves, and every
/// reading the sheet draws — the render path may not go back to the
/// selection for them.
struct Built {
    /// The subject the controls read from: [`Subject::id`].
    primary: SharedString,
    /// How many bodies an edit writes.
    count: usize,
    /// The graph shape every body shares; `None` when they differ, which
    /// leaves only the name to edit.
    shape: Option<String>,
    /// The primary graph's [`graph::edit::layout`] the widgets were built for.
    layout: Option<String>,
    name: Entity<TextInput>,
    /// The name and placeholder the field was last pointed at.
    synced_name: (String, String),
    selection: Entity<GroupExpressionEditor>,
    synced_selection: String,
    /// The subject's blend mode and output kind.
    blend: BlendMode,
    output: Option<Kind>,
    graph: Option<ClipGraph>,
    controls: Option<graph::Controls>,
    _subs: Vec<Subscription>,
}

// -- host and subject ---------------------------------------------------------

/// Where the sheet is open.
#[derive(Clone, Copy)]
pub(crate) enum Host<'a> {
    /// A score's track editor. The subject is its selected clips, the first
    /// selected primary: the controls read it, and an edit writes them all.
    Track(&'a Editor),
    /// A Perform tab's pattern editor. The subject is the one pattern it
    /// holds; with none, it is choosing a preset to start one.
    Pattern(&'a PatternSheet),
}

/// [`Host`], to sync the sheet in.
pub(crate) enum HostMut<'a> {
    Track(&'a mut Editor),
    Pattern(&'a mut PatternSheet),
}

/// What the sheet edits, as its host holds it: the primary clip, or the
/// pattern.
pub(super) struct Subject<'a> {
    /// What the controls are built for.
    id: &'a str,
    name: &'a str,
    /// The graph's summary, where the host keeps one.
    summary: Option<&'a str>,
    selection: &'a Selection,
    seed: u64,
    /// The seed the selection resolves with.
    selection_seed: u64,
    graph: &'a ClipGraph,
    blend: BlendMode,
}

impl Subject<'_> {
    /// What the name field reads while it is empty, and what a cleared name
    /// becomes: the graph's summary.
    fn placeholder(&self) -> String {
        self.summary
            .map_or_else(|| self.graph.summary(), str::to_owned)
    }
}

/// Where the playhead is in the subject, in beats since its start, and how
/// long the subject lasts.
pub(super) struct Beats {
    /// The beat the subject starts on.
    pub(super) start: f64,
    pub(super) length: f64,
    pub(super) elapsed: f64,
    pub(super) playing: bool,
}

impl<'a> Host<'a> {
    /// The sheet open in `body`, if one is.
    pub(crate) fn of(body: &'a Body) -> Option<Self> {
        match body {
            Body::TrackEditor(editor) => Some(Self::Track(editor)),
            Body::Perform(perform) => perform.pattern_sheet.as_ref().map(Self::Pattern),
            Body::Patch(_) => None,
        }
    }

    pub(super) fn sheet(self) -> &'a State {
        match self {
            Self::Track(editor) => &editor.sheet,
            Self::Pattern(open) => &open.sheet,
        }
    }

    /// The tab the sheet is in, where the answer to a request it made lands.
    fn target(self) -> Target {
        match self {
            Self::Track(editor) => editor.target(),
            Self::Pattern(open) => open.target.clone(),
        }
    }

    fn venue(self) -> &'a str {
        match self {
            Self::Track(editor) => &editor.venue_id,
            Self::Pattern(open) => open.target.venue(),
        }
    }

    pub(super) fn subject(self) -> Option<Subject<'a>> {
        match self {
            Self::Track(editor) => {
                let clip = primary_clip(editor)?;
                let core = clip.core.as_ref()?;
                Some(Subject {
                    id: &clip.id,
                    name: &core.name,
                    summary: Some(&clip.summary),
                    selection: &core.selection,
                    seed: core.seed,
                    selection_seed: core.selection_seed.unwrap_or(core.seed),
                    graph: &core.graph,
                    blend: clip.blend,
                })
            }
            Self::Pattern(open) => {
                let pattern = open.pattern.as_ref()?;
                Some(Subject {
                    id: &pattern.id,
                    name: &pattern.name,
                    summary: None,
                    selection: &pattern.selection,
                    seed: pattern.seed,
                    selection_seed: pattern.seed,
                    graph: &pattern.graph,
                    blend: pattern.blend_mode,
                })
            }
        }
    }

    /// How many bodies an edit writes.
    fn count(self) -> usize {
        match self {
            Self::Track(editor) => editor.selected.len(),
            Self::Pattern(open) => usize::from(open.pattern.is_some()),
        }
    }

    /// The graph shape every body shares, or `None` when they differ.
    fn shape(self) -> Option<String> {
        match self {
            Self::Track(editor) => shared_shape(editor),
            Self::Pattern(open) => open
                .pattern
                .as_ref()
                .map(|pattern| graph::edit::shape(&pattern.graph)),
        }
    }

    /// Where the playhead is in the subject. A Perform pattern has no
    /// timeline to have one on.
    pub(super) fn beats(self) -> Option<Beats> {
        match self {
            Self::Track(editor) => {
                let clip = primary_clip(editor)?;
                let timeline = editor.beats.as_deref()?.timeline().ok()?;
                let start = timeline.beat_at(clip.start).ok()?;
                let now = timeline
                    .beat_at(f64::from(editor.transport.position))
                    .ok()?;
                Some(Beats {
                    start,
                    length: timeline.beat_at(clip.end).ok()? - start,
                    elapsed: now - start,
                    playing: editor.transport.playing,
                })
            }
            Self::Pattern(_) => None,
        }
    }
}

impl HostMut<'_> {
    fn host(&self) -> Host<'_> {
        match self {
            Self::Track(editor) => Host::Track(editor),
            Self::Pattern(open) => Host::Pattern(open),
        }
    }

    fn sheet(&mut self) -> &mut State {
        match self {
            Self::Track(editor) => &mut editor.sheet,
            Self::Pattern(open) => &mut open.sheet,
        }
    }
}

/// The sheet open in `body`, to change.
fn sheet_mut(body: &mut Body) -> Option<&mut State> {
    match body {
        Body::TrackEditor(editor) => Some(&mut editor.sheet),
        Body::Perform(perform) => perform.pattern_sheet.as_mut().map(|open| &mut open.sheet),
        Body::Patch(_) => None,
    }
}

impl Luma {
    /// Where the active tab's sheet is open, if it has one.
    pub(super) fn sheet_host(&self) -> Option<Host<'_>> {
        Host::of(self.workspace.active_body()?)
    }

    /// The active tab's sheet, read-only.
    pub(super) fn sheet_ref(&self) -> Option<&State> {
        self.sheet_host().map(Host::sheet)
    }

    /// Change the active tab's sheet, and redraw.
    pub(super) fn with_sheet(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut State)) {
        if let Some(sheet) = self.workspace.active_body_mut().and_then(sheet_mut) {
            edit(sheet);
            cx.notify();
        }
    }

    /// The sheet of tab `target`, shown or parked: where the answer to a
    /// request it made lands.
    fn sheet_at(&mut self, target: &Target) -> Option<&mut State> {
        sheet_mut(self.venues.body_mut(&mut self.workspace, target)?)
    }

    /// The subject's graph, as the canvas shows it.
    pub(super) fn shown_graph(&self) -> Option<ClipGraph> {
        Some(self.sheet_host()?.subject()?.graph.clone())
    }
}

fn primary_clip(editor: &Editor) -> Option<&Clip> {
    let id = editor.selected.first()?;
    editor.clips.iter().find(|clip| &clip.id == id)
}

fn selected_clips(editor: &Editor) -> impl Iterator<Item = &Clip> {
    editor
        .clips
        .iter()
        .filter(|clip| editor.selected.contains(&clip.id))
}

/// The graph shape every selected clip shares, or `None` when they differ.
fn shared_shape(editor: &Editor) -> Option<String> {
    let mut shapes = selected_clips(editor).map(|clip| {
        clip.core
            .as_ref()
            .map(|core| graph::edit::shape(&core.graph))
    });
    let first = shapes.next()??;
    shapes
        .all(|shape| shape.as_ref() == Some(&first))
        .then_some(first)
}

// -- sync ---------------------------------------------------------------------

/// Refresh controls from the subject without overwriting active drafts.
pub(crate) fn sync(mut host: HostMut, window: &mut Window, cx: &mut Context<Luma>) {
    tick_menus(host.sheet(), window, cx);
    ensure_groups(&mut host, cx);
    ensure_heads(&mut host, cx);
    let Some(primary) = host.host().subject().map(|subject| subject.id.to_owned()) else {
        let sheet = host.sheet();
        sheet.open = None;
        sheet.built = None;
        browser::sync(&mut host, cx);
        return;
    };
    browser::leave(host.sheet());
    let shape = host.host().shape();
    let count = host.host().count();
    let rebuild = match &host.sheet().built {
        Some(built) => built.primary.as_ref() != primary || built.count != count,
        None => true,
    };
    if rebuild {
        let built = build(host.host(), primary.into(), window, cx);
        let sheet = host.sheet();
        sheet.open = None;
        sheet.last.clear();
        sheet.error = None;
        sheet.canvas = graph::canvas::State::default();
        sheet.built = Some(built);
    }
    let sheet = host.sheet();
    if sheet.canvas.focus.is_none() {
        sheet.canvas.focus = Some(cx.focus_handle());
    }
    let graph = shape
        .as_ref()
        .and_then(|_| Some(host.host().subject()?.graph.clone()));
    let layout = graph.as_ref().map(graph::edit::layout);
    let reshaped = host
        .sheet()
        .built
        .as_ref()
        .is_some_and(|built| built.shape != shape || built.layout != layout);
    if reshaped {
        let controls = graph.as_ref().map(|graph| graph::build(graph, window, cx));
        if let Some(built) = host.sheet().built.as_mut() {
            built.shape = shape;
            built.layout = layout;
            built.graph = graph;
            built.controls = controls;
        }
    }
    resync(&mut host, window, cx);
}

/// Start a closed menu's exit, and keep frames coming while it plays: a menu
/// leaves the way it came, whichever path closed it.
fn tick_menus(sheet: &mut State, window: &mut Window, cx: &mut Context<Luma>) {
    if sheet.was_open != sheet.open {
        if let Some(closed) = sheet.was_open {
            let mut exit = MenuVisibility::Open;
            exit.close();
            sheet.closing = Some((closed, exit));
        }
        sheet.was_open = sheet.open;
    }
    // Opened again before its exit finished: it is simply open.
    if sheet
        .closing
        .is_some_and(|(menu, _)| sheet.open == Some(menu))
    {
        sheet.closing = None;
    }
    if let Some((_, visibility)) = &mut sheet.closing {
        if visibility.tick_close(luma_ui::motion::reduced_motion(cx)) {
            window.request_animation_frame();
        } else {
            sheet.closing = None;
        }
    }
}

/// How `menu` shows: open, playing its exit, or closed.
fn menu_visibility(sheet: &State, menu: Menu) -> MenuVisibility {
    match sheet.closing {
        _ if sheet.open == Some(menu) => MenuVisibility::Open,
        Some((closing, visibility)) if closing == menu => visibility,
        _ => MenuVisibility::Closed,
    }
}

/// Ask for the venue's group names, once.
fn ensure_groups(host: &mut HostMut, cx: &mut Context<Luma>) {
    if !matches!(host.sheet().groups, Groups::NotAsked) {
        return;
    }
    host.sheet().groups = Groups::Loading;
    let (target, venue) = (host.host().target(), host.host().venue().to_owned());
    cx.spawn(async move |this, cx| {
        let Ok(pending) = this.update(cx, |this, _| this.library.venue_groups(&venue)) else {
            return;
        };
        let rows = pending.await;
        this.update(cx, |this, cx| {
            if let Some(sheet) = this.sheet_at(&target) {
                let names: Vec<SharedString> = rows
                    .into_iter()
                    .flatten()
                    .filter_map(|group| group.name)
                    .map(SharedString::from)
                    .collect();
                sheet.groups = Groups::Ready(Rc::new(names));
                cx.notify();
            }
        })
        .ok();
    })
    .detach();
}

/// Ask for the heads of the subject's selection when its graph lies across
/// space, once per venue, selection and seed.
fn ensure_heads(host: &mut HostMut, cx: &mut Context<Luma>) {
    let read = host.host();
    let Some(subject) = read.subject() else {
        return;
    };
    let graph = subject.graph;
    if !graph.nodes.values().any(|node| node.kind == Kind::Space) {
        return;
    }
    let seed = subject.selection_seed;
    let selection = subject.selection.clone();
    let elements = graph.laser().map(|laser| laser.count());
    let key = (
        read.venue().to_owned(),
        selection.to_value().to_string(),
        seed,
        elements,
    );
    let target = read.target();
    let sheet = host.sheet();
    if sheet.heads.key.as_ref() == Some(&key) {
        return;
    }
    sheet.heads = Heads {
        key: Some(key.clone()),
        cells: None,
    };
    cx.spawn(async move |this, cx| {
        let Ok(pending) = this.update(cx, |this, _| {
            this.library
                .selection_cells(&key.0, &selection, seed, elements)
        }) else {
            return;
        };
        let cells = pending.await;
        this.update(cx, |this, cx| {
            let Some(sheet) = this.sheet_at(&target) else {
                return;
            };
            if sheet.heads.key.as_ref() != Some(&key) {
                return;
            }
            // A selection that does not resolve has no heads to show.
            sheet.heads.cells = Some(cells.unwrap_or_default().into());
            cx.notify();
        })
        .ok();
    })
    .detach();
}

/// The name, selection and blend controls for one subject; the graph's are
/// built on the first sync after, from its shape.
fn build(host: Host, primary: SharedString, window: &mut Window, cx: &mut Context<Luma>) -> Built {
    let mut subs = Vec::new();
    let subject = host.subject();
    let name_text = subject
        .as_ref()
        .map_or_else(String::new, |subject| subject.name.to_owned());
    let placeholder = subject
        .as_ref()
        .map_or_else(String::new, Subject::placeholder);
    let name = cx.new(|cx| {
        let mut input = TextInput::search(placeholder.clone(), cx);
        input.set_text(name_text.clone(), cx);
        input
    });
    subs.push(cx.subscribe(
        &name,
        |this: &mut Luma, field, event: &text_input::Event, cx| {
            match event {
                text_input::Event::Submitted | text_input::Event::Blurred => {
                    let name = field.read(cx).text().trim().to_string();
                    this.rename_subject(name, cx);
                }
                // Escape puts the stored name back.
                text_input::Event::Cancelled => this.with_sheet(cx, |sheet| {
                    if let Some(built) = sheet.built.as_mut() {
                        built.synced_name = Default::default();
                    }
                }),
                _ => {}
            }
        },
    ));

    let groups: Vec<SharedString> = match &host.sheet().groups {
        Groups::Ready(names) => names.as_ref().clone(),
        _ => Vec::new(),
    };
    let expression = subject
        .as_ref()
        .map_or_else(String::new, |subject| subject.selection.expression.clone());
    let selection = cx.new(|cx| {
        GroupExpressionEditor::new(
            groups.iter().cloned(),
            expression.clone(),
            EXPR_W,
            window,
            cx,
        )
    });
    subs.push(cx.subscribe(
        &selection,
        move |this: &mut Luma, _, event: &ExpressionEvent, cx| {
            let ExpressionEvent::Committed(expression) = event.clone();
            this.selection_live(cx, |selection| selection.expression = expression.clone());
        },
    ));

    Built {
        primary,
        count: host.count(),
        shape: None,
        layout: None,
        name,
        synced_name: (name_text, placeholder),
        selection,
        synced_selection: expression,
        blend: subject
            .as_ref()
            .map_or(BlendMode::Replace, |subject| subject.blend),
        output: subject.and_then(|subject| subject.graph.output_kind()),
        graph: None,
        controls: None,
        _subs: subs,
    }
}

/// Refresh every reading the sheet draws, and push externally moved values
/// into the widgets that show them.
///
/// "Externally" is anything that rewrote the subject — an undo, a lost write
/// reloading, another gesture — including this sheet's own edits, whose echo
/// the synced copies filter out: a value the sheet itself wrote comes back
/// equal, so a widget's in-progress state survives.
fn resync(host: &mut HostMut, window: &mut Window, cx: &mut Context<Luma>) {
    let Some(subject) = host.host().subject() else {
        return;
    };
    let name = (subject.name.to_owned(), subject.placeholder());
    let (blend, expression) = (subject.blend, subject.selection.expression.clone());
    let graph = subject.graph.clone();
    let Some(built) = host.sheet().built.as_mut() else {
        return;
    };
    let focused = built.name.read(cx).focus_handle(cx).is_focused(window);
    built.blend = blend;
    built.output = graph.output_kind();
    if built.synced_name != name && !focused {
        built.name.update(cx, |field, cx| {
            field.set_placeholder(name.1.clone(), cx);
            field.set_text(name.0.clone(), cx);
        });
        built.synced_name = name;
    }
    if built.synced_selection != expression {
        built
            .selection
            .update(cx, |field, cx| field.set_text(expression.clone(), cx));
        built.synced_selection = expression;
    }
    if built.shape.is_some() {
        if let Some(controls) = built.controls.as_mut() {
            graph::sync(controls, &graph, window, cx);
        }
        built.graph = Some(graph);
    }
}

// -- the write paths ----------------------------------------------------------

/// `body` with `edit` applied: `None` when that changed nothing, and the
/// checker's words when the result fails its checks.
fn edited(
    library: &luma_patterns::Library,
    id: &str,
    body: &luma_patterns::Clip,
    edit: &mut impl FnMut(&mut luma_patterns::Clip),
) -> Result<Option<luma_patterns::Clip>, String> {
    let mut edited = body.clone();
    edit(&mut edited);
    if edited == *body {
        return Ok(None);
    }
    luma_patterns::Score::validate_clip(library, id, &edited).map_err(|error| error.to_string())?;
    Ok(Some(edited))
}

/// A live edit of the selected clips: the working copy now, with the
/// [`History`] checkpoint on a burst's first edit. Returns the clips it
/// changed, whose previews are owed.
fn clips_live(
    editor: &mut Editor,
    edit: &mut impl FnMut(&mut luma_patterns::Clip),
) -> Vec<SharedString> {
    let mut touched: Vec<SharedString> = Vec::new();
    if !editor.writable() || editor.selected.is_empty() {
        return touched;
    }
    let library = luma_patterns::standard_library();
    let mut clips: Vec<Clip> = editor.clips.iter().cloned().collect();
    let mut refused = None;
    for clip in &mut clips {
        if !editor.selected.contains(&clip.id) {
            continue;
        }
        let Some(core) = clip.core.as_mut() else {
            continue;
        };
        match edited(&library, &clip.id, core, edit) {
            Ok(Some(edited)) => {
                *core = edited;
                clip.refresh();
                touched.push(clip.id.clone());
            }
            Ok(None) => {}
            Err(error) => refused = Some(error),
        }
    }
    editor.sheet.error = refused;
    if touched.is_empty() {
        return touched;
    }
    if !editor.sheet.burst {
        editor.checkpoint();
        editor.sheet.burst = true;
    }
    editor.replace_clips(clips);
    touched
}

/// A live edit of the open Perform pattern, checked as a clip: its copy now,
/// and a save owed. Returns whether it changed.
fn pattern_live(open: &mut PatternSheet, edit: &mut impl FnMut(&mut luma_patterns::Clip)) -> bool {
    let Some(pattern) = open.pattern.as_mut() else {
        return false;
    };
    let library = luma_patterns::standard_library();
    // Timed as the backend checks a pattern it saves.
    let result = edited(&library, &pattern.id, &pattern.as_clip(0., 1.), edit);
    open.sheet.error = result.as_ref().err().cloned();
    let Ok(Some(clip)) = result else {
        return false;
    };
    pattern.name = clip.name;
    pattern.selection = clip.selection;
    pattern.blend_mode = clip.blend_mode;
    pattern.seed = clip.seed;
    pattern.graph = clip.graph;
    open.sheet.burst = true;
    true
}

impl Luma {
    /// A blend pick from the sheet. In a track editor every selected clip
    /// whose output takes the mode takes it, in one committed write; a
    /// Perform pattern takes it as a live edit. An aim takes replace or
    /// offset; light takes the light modes.
    pub(crate) fn sheet_blend(&mut self, mode: BlendMode, cx: &mut Context<Self>) {
        if matches!(self.sheet_host(), Some(Host::Pattern(_))) {
            self.sheet_live(cx, |clip| {
                let output = clip.graph.output_kind().map_or("color", Kind::name);
                if blend_modes(output).contains(&mode) {
                    clip.blend_mode = mode;
                }
            });
            return;
        }
        self.track_command(
            move |editor| {
                if editor.selected.is_empty() {
                    return;
                }
                let mut clips: Vec<Clip> = editor.clips.iter().cloned().collect();
                for clip in &mut clips {
                    if editor.selected.contains(&clip.id)
                        && blend_modes(&clip.output).contains(&mode)
                    {
                        clip.blend = mode;
                    }
                }
                editor.replace_clips(clips);
            },
            cx,
        );
    }

    /// A name from the sheet, on every body of the subject: in a track
    /// editor in one committed write, on a Perform pattern as a live edit. A
    /// pattern needs a name: a cleared field names it after its summary.
    fn rename_subject(&mut self, name: String, cx: &mut Context<Self>) {
        let named = |summary: &str| {
            if name.is_empty() {
                summary.to_owned()
            } else {
                name.clone()
            }
        };
        let unchanged = match self.sheet_host() {
            Some(Host::Track(editor)) => selected_clips(editor).all(|clip| {
                clip.core
                    .as_ref()
                    .is_some_and(|core| core.name == named(&clip.summary))
            }),
            host => host
                .and_then(Host::subject)
                .is_none_or(|subject| subject.name == named(&subject.placeholder())),
        };
        if unchanged {
            // The field may still read empty; put the stored name back.
            self.with_sheet(cx, |sheet| {
                if let Some(built) = sheet.built.as_mut() {
                    built.synced_name = Default::default();
                }
            });
            return;
        }
        if matches!(self.sheet_host(), Some(Host::Pattern(_))) {
            self.sheet_live(cx, |clip| clip.name = named(&clip.graph.summary()));
            return;
        }
        self.track_command(
            |editor| {
                let mut clips: Vec<Clip> = editor.clips.iter().cloned().collect();
                for clip in &mut clips {
                    if !editor.selected.contains(&clip.id) {
                        continue;
                    }
                    let name = named(&clip.summary);
                    if let Some(core) = clip.core.as_mut() {
                        core.name = name;
                    }
                    clip.refresh();
                }
                editor.replace_clips(clips);
            },
            cx,
        );
    }

    /// The fast path, and the one funnel every live edit of the subject
    /// takes: apply `edit` to each of its bodies now, and owe the host one
    /// write on the trailing edge. A body `edit` leaves failing its checks
    /// keeps its old one, and the sheet says why.
    ///
    /// In a track editor the first edit of a burst records the [`History`]
    /// checkpoint and the clips' heatmap previews follow each edit; the
    /// flush closes the burst, so a whole picker drag is one undo step and
    /// one write. A Perform pattern's write is a save of the pattern, which
    /// the backend plays from.
    fn sheet_live(
        &mut self,
        cx: &mut Context<Self>,
        mut edit: impl FnMut(&mut luma_patterns::Clip),
    ) {
        let owed = match self.workspace.active_body_mut() {
            Some(Body::TrackEditor(editor)) => {
                let touched = clips_live(editor, &mut edit);
                let owed = !touched.is_empty();
                if owed {
                    self.refresh_clip_previews(touched, cx);
                }
                owed
            }
            Some(Body::Perform(perform)) => perform
                .pattern_sheet
                .as_mut()
                .is_some_and(|open| pattern_live(open, &mut edit)),
            _ => return,
        };
        cx.notify();
        if owed {
            self.schedule_arg_flush(cx);
        }
    }

    /// A graph edit from the sheet, on every body of the subject.
    pub(crate) fn graph_live(
        &mut self,
        cx: &mut Context<Self>,
        mut edit: impl FnMut(&mut ClipGraph),
    ) {
        self.sheet_live(cx, |clip| edit(&mut clip.graph));
    }

    /// A selection edit: the expression field and the fixture picker commit
    /// independently, and each writes the whole selection back.
    pub(crate) fn selection_live(
        &mut self,
        cx: &mut Context<Self>,
        mut edit: impl FnMut(&mut Selection),
    ) {
        self.sheet_live(cx, |clip| edit(&mut clip.selection));
    }

    /// Open the fixture picker on the subject's selection.
    ///
    /// The sheet reads out what the dialog cannot see for itself: what the
    /// selection currently says, and the venue's group vocabulary the
    /// expression field already loaded for its autocomplete — asking for it
    /// twice would be a second load of the same list.
    fn pick_fixtures(&mut self, cx: &mut Context<Self>) {
        let Some(host) = self.sheet_host() else {
            return;
        };
        let groups = match &host.sheet().groups {
            Groups::Ready(names) => names.as_ref().clone(),
            Groups::NotAsked | Groups::Loading => Vec::new(),
        };
        let selection = host
            .subject()
            .map_or_else(Selection::all, |subject| subject.selection.clone());
        let venue = host.venue().to_owned();
        self.open_fixture_picker(venue, groups, &selection, cx);
    }

    /// Arm (or re-arm) the trailing commit. A track editor's is serialized
    /// under [`Luma::commit_clips`]'s own in-flight discipline, so a burst
    /// that outruns a slow write queues exactly one follow-up.
    fn schedule_arg_flush(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.workspace.active().cloned() else {
            return;
        };
        let Some(sheet) = self.sheet_at(&target) else {
            return;
        };
        sheet.flush_gen += 1;
        let generation = sheet.flush_gen;
        let pending = self.library.debounce(ARG_FLUSH);
        cx.spawn(async move |this, cx| {
            pending.await;
            this.update(cx, |this, cx| {
                let due = this.sheet_at(&target).is_some_and(|sheet| {
                    let due = sheet.flush_gen == generation;
                    if due {
                        sheet.burst = false;
                    }
                    due
                });
                if due {
                    cx.notify();
                    this.flush_sheet(&target, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Write what the live edits of tab `target`'s sheet owe its host: the
    /// score, or the Perform pattern.
    pub(crate) fn flush_sheet(&mut self, target: &Target, cx: &mut Context<Self>) {
        match self.venues.body_mut(&mut self.workspace, target) {
            Some(Body::TrackEditor(editor)) => {
                if editor.graph_score.is_some() {
                    self.commit_graph_score_for(target.clone(), cx);
                } else if self.workspace.active() == Some(target) {
                    self.commit_clips(cx);
                }
            }
            Some(Body::Perform(perform)) => {
                // What the editor edits, onto the set's own copy: its place
                // and exclusive set may have changed since the editor opened.
                let pattern = perform
                    .pattern_sheet
                    .as_ref()
                    .and_then(|open| open.pattern.as_ref())
                    .map(|edited| {
                        match perform
                            .patterns
                            .iter()
                            .flatten()
                            .find(|p| p.id == edited.id)
                        {
                            Some(current) => luma_lib::models::perform::PerformPattern {
                                name: edited.name.clone(),
                                selection: edited.selection.clone(),
                                blend_mode: edited.blend_mode,
                                graph: edited.graph.clone(),
                                ..current.clone()
                            },
                            None => edited.clone(),
                        }
                    });
                if let Some(pattern) = pattern {
                    self.store_perform_pattern(target, pattern, cx);
                }
            }
            _ => {}
        }
    }
}

/// The blend modes an output takes: an aim blends as replace or offset,
/// light as the light modes.
pub(super) fn blend_modes(output: &str) -> &'static [BlendMode] {
    if output == Kind::Aim.name() {
        &[BlendMode::Replace, BlendMode::Offset]
    } else {
        &BlendMode::LIGHT
    }
}

// -- rendering ----------------------------------------------------------------

/// The inspector: the subject's controls, or the preset browser, filling
/// the whole box it is given — Program's under the stage, Direct's graph page
/// or the Perform tab's. `lead` opens the header strip: a page's way back.
pub(crate) fn panel(host: Host, app: &Entity<Luma>, lead: Option<AnyElement>) -> AnyElement {
    let sheet = host.sheet();
    let (label, body) = match sheet.built.as_ref() {
        Some(built) => ("Pattern graph", body(sheet, built, app, lead)),
        None => (
            "Presets",
            div()
                .size_full()
                .min_h_0()
                .flex()
                .flex_col()
                .children(lead.map(|lead| strip().child(lead)))
                .child(div().flex_1().min_h_0().child(browser::body(host, app)))
                .into_any_element(),
        ),
    };
    div()
        .id("clip-inspector")
        .size_full()
        .min_h_0()
        .overflow_hidden()
        .bg(ladder::background())
        .child(body)
        .agent_node(Role::Card, label)
        .into_any_element()
}

/// A page's way back, at the head of its strip: a chevron and where it goes.
pub(crate) fn back(
    id: &'static str,
    label: &'static str,
    agent: &'static str,
    app: &Entity<Luma>,
    close: impl Fn(&mut Luma, &mut Context<Luma>) + 'static,
) -> AnyElement {
    let app = app.clone();
    luma_ui::button("", Enabled::Yes)
        .id(id)
        .pl(px(6.))
        .pr(px(10.))
        .gap(px(4.))
        .child(gpui_component::Icon::new(luma_ui::icons::IconName::ChevronLeft).size(px(14.)))
        .child(label)
        .on_click(move |_, _, cx| app.update(cx, |this, cx| close(this, cx)))
        .agent_node(Role::Button, agent)
        .into_any_element()
}

/// The clip name field's width in the header strip.
const NAME_W: f32 = 140.;

/// The selection group — label, expression and pick chip — from the least
/// it takes before the strip wraps to the most it grows to.
const SELECTION_MIN_W: f32 = 200.;
const SELECTION_MAX_W: f32 = 120. + EXPR_W;

/// The header strip every inspector body opens with: one row of compact
/// controls over a hairline. It wraps rather than overlapping when the pane
/// is too narrow for one row.
fn strip() -> Div {
    div()
        .flex_none()
        .w_full()
        .flex()
        .flex_row()
        .flex_wrap()
        .items_center()
        .gap_x(px(10.))
        .gap_y(px(6.))
        .px(px(12.))
        .py(px(8.))
        .border_b_1()
        .border_color(ladder::trim())
}

/// The sheet's content: one header strip — the way back, the name, the
/// selection and blend — then the graph canvas filling the rest.
fn body(sheet: &State, built: &Built, app: &Entity<Luma>, lead: Option<AnyElement>) -> AnyElement {
    let header = strip()
        .children(lead)
        .when(built.count > 1, |header| {
            let count = format!("{} patterns", built.count);
            header.child(
                div()
                    .flex_none()
                    .text_size(px(12.))
                    .text_color(ladder::muted_foreground())
                    .child(count.clone())
                    .agent_node(Role::Text, count),
            )
        })
        .child(
            div()
                .flex_none()
                .w(px(NAME_W))
                .key_context(text_input::DRAFT_CONTEXT)
                .child(built.name.clone())
                .agent_node(Role::Input, "Name"),
        )
        .when(built.shape.is_some(), |header| {
            header
                .child(labelled("Selection", selection_row(built, app), |row| {
                    row.flex_1()
                        .min_w(px(SELECTION_MIN_W))
                        .max_w(px(SELECTION_MAX_W))
                }))
                .child(labelled("Blend", blend_select(sheet, built, app), |row| {
                    row.flex_none()
                }))
        });
    let error = sheet
        .error
        .as_ref()
        .filter(|_| built.shape.is_some())
        .map(|error| {
            // Validation as the graph is edited: why the edit did not take,
            // under the header it was made in.
            div()
                .flex_none()
                .px(px(12.))
                .py(px(6.))
                .child(luma_ui::notice::line(error.clone()))
        });
    let graph = match (&built.shape, &built.controls, &built.graph) {
        (Some(_), Some(controls), Some(shown)) => Some(graph::editor(sheet, controls, shown, app)),
        _ => None,
    };
    div()
        .size_full()
        .min_h_0()
        .flex()
        .flex_col()
        .child(header)
        .children(error)
        .children(graph.map(|graph| div().flex_1().min_h_0().flex().flex_col().child(graph)))
        .into_any_element()
}

/// A header control with its name beside it, muted, rather than over it.
fn labelled(label: &str, control: Div, size: impl FnOnce(Div) -> Div) -> AnyElement {
    size(
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.))
            .child(luma_ui::caption(label.to_string()))
            .child(control),
    )
    .agent_node(Role::Row, label.to_string())
    .into_any_element()
}

/// The selection row: the expression field, and a chip that opens the
/// fixture picker. Both write the same value through `selection_live`. The
/// field takes what the row has to spare, and gives it up first.
fn selection_row(built: &Built, app: &Entity<Luma>) -> Div {
    let opened = app.clone();
    let pick_chip = luma_ui::float::chip()
        .id("selection:pick")
        .child("Pick")
        .on_click(move |_, _, cx| {
            opened.update(cx, |this, cx| this.pick_fixtures(cx));
        })
        .agent_node(Role::Button, "Pick fixtures");
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.))
        .child(div().flex_1().min_w_0().child(built.selection.clone()))
        .child(pick_chip)
}

/// The blend row: the modes the output takes, applied to the whole
/// selection on pick.
fn blend_select(sheet: &State, built: &Built, app: &Entity<Luma>) -> Div {
    let modes = blend_modes(built.output.map_or("color", Kind::name));
    let names: Vec<&str> = modes.iter().map(|mode| mode.label()).collect();
    let open = menu_visibility(sheet, Menu::Blend);
    let toggle = app.clone();
    let pick = app.clone();
    luma_arg_select(
        "blend",
        built.blend.label(),
        &names,
        open,
        move |_, cx| {
            toggle.update(cx, |this, cx| {
                this.with_sheet(cx, |sheet| {
                    sheet.open = match sheet.open {
                        Some(Menu::Blend) => None,
                        _ => Some(Menu::Blend),
                    };
                });
            });
        },
        move |index, _, cx| {
            pick.update(cx, |this, cx| {
                this.with_sheet(cx, |sheet| sheet.open = None);
                this.sheet_blend(modes[index], cx);
            });
        },
    )
}

/// Air between a row's header line and its control.
const LABEL_GAP: f32 = 6.;

/// Every row of the sheet has one shape. A header line one control tall
/// carries the label, sentence case, and on its right anything that changes
/// how the value is read (a source chip). Under it, the value control spans
/// the column, whatever kind it is.
fn sheet_row(label: &str, accessories: Vec<AnyElement>, control: Div) -> AnyElement {
    let label = sentence_case(label);
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(luma_ui::rpx(LABEL_GAP))
        .child(
            div()
                .relative()
                .w_full()
                .h(luma_ui::rpx(CONTROL_HEIGHT))
                .flex()
                .flex_row()
                .items_center()
                .gap(luma_ui::rpx(6.))
                .child(luma_ui::caption(label.clone()))
                .child(div().flex_1())
                .children(accessories),
        )
        .child(div().w_full().flex().flex_col().child(control))
        .agent_node(Role::Row, label)
        .into_any_element()
}

/// First letter up, the rest as written: "how many" reads "How many".
fn sentence_case(label: &str) -> String {
    let mut chars = label.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}
