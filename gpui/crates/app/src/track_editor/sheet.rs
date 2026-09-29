//! Clip controls in the dedicated editing area, beside the visualizer.
//! The inspector is always open and has its own scrolling viewport,
//! independent of timeline height. With a clip selected it shows the clip's
//! name, selection, blend and graph (see [`graph`]); with none it shows the
//! preset browser. Selection retargets the existing controls; graph writes
//! keep their debounced history and persistence path.

use luma_patterns::clip_graph::{ClipGraph, Input, Kind};
use luma_patterns::Selection;
use luma_ui::arg::arg_row;
use luma_ui::arg::color::{ColorArg, ColorArgEditor, ColorArgEvent};
use luma_ui::arg::expression::{ExpressionEvent, GroupExpressionEditor};
use luma_ui::arg::gradient::{Gradient, GradientStop, Light};
use luma_ui::arg::number::{DraftedNumber, NumberEvent};
use luma_ui::arg::select::{luma_arg_select, MenuVisibility};
use luma_ui::text_input::{self, TextInput};
use luma_ui::CONTROL_HEIGHT;

use super::*;

mod browser;
pub(super) mod graph;

pub(crate) use browser::Audition;
pub(super) use browser::{DropGhost, PresetDrag};

/// Air between one row and the next, and between the sheet's bands.
const ROW_GAP: f32 = 14.;

/// A full-bleed control's width inside the sheet.
const FIELD_W: f32 = luma_ui::sheet::CONTENT_WIDTH;

/// The expression field, which shares its row with the fixture-picker chip.
const EXPR_W: f32 = FIELD_W - 62.;

/// The trailing edge a burst of live edits is committed on.
const ARG_FLUSH: Duration = Duration::from_millis(250);

// -- state --------------------------------------------------------------------

/// The sheet's own state, owned by the [`Editor`].
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
    /// A live burst is running: its checkpoint is recorded and a trailing
    /// commit is owed.
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
    /// The heads of the primary clip's selection, for the head marks of a
    /// strip across space.
    heads: Heads,
}

/// The heads a selection resolves to, asked once per venue, selection and
/// seed. `cells` is `None` until the answer lands.
#[derive(Default)]
struct Heads {
    key: Option<(String, String, u64)>,
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

    /// Close whichever menu the sheet has up, reporting whether there was one.
    pub(crate) fn dismiss_menu(&mut self) -> bool {
        self.open.take().is_some() | self.canvas.menu.take().is_some() | self.canvas.cancel()
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
    /// The primary (first selected) clip the controls read from.
    primary: SharedString,
    /// How many clips are selected.
    count: usize,
    /// The graph shape every selected clip shares; `None` when they differ,
    /// which leaves only the name to edit.
    shape: Option<String>,
    /// The primary graph's [`graph::edit::layout`] the widgets were built for.
    layout: Option<String>,
    name: Entity<TextInput>,
    /// The name and placeholder the field was last pointed at.
    synced_name: (String, String),
    selection: Entity<GroupExpressionEditor>,
    synced_selection: String,
    /// The primary clip's blend mode and output kind.
    blend: BlendMode,
    output: Option<Kind>,
    graph: Option<ClipGraph>,
    controls: Option<graph::Controls>,
    _subs: Vec<Subscription>,
}

// -- subject ------------------------------------------------------------------

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

/// Refresh controls from the selected clip without overwriting active drafts.
pub(super) fn sync(editor: &mut Editor, window: &mut Window, cx: &mut Context<Luma>) {
    tick_menus(&mut editor.sheet, window, cx);
    ensure_groups(editor, cx);
    ensure_heads(editor, cx);
    let Some(primary) = primary_clip(editor).map(|clip| clip.id.clone()) else {
        editor.sheet.open = None;
        editor.sheet.built = None;
        browser::sync(editor, cx);
        return;
    };
    browser::leave(editor);
    let shape = shared_shape(editor);
    let count = editor.selected.len();
    let rebuild = match &editor.sheet.built {
        Some(built) => built.primary != primary || built.count != count,
        None => true,
    };
    if rebuild {
        editor.sheet.open = None;
        editor.sheet.last.clear();
        editor.sheet.error = None;
        editor.sheet.canvas = graph::canvas::State::default();
        let built = build(editor, primary, window, cx);
        editor.sheet.built = Some(built);
    }
    if editor.sheet.canvas.focus.is_none() {
        editor.sheet.canvas.focus = Some(cx.focus_handle());
    }
    let graph = shape
        .as_ref()
        .and_then(|_| primary_clip(editor)?.core.as_ref())
        .map(|core| core.graph.clone());
    let layout = graph.as_ref().map(graph::edit::layout);
    let reshaped = editor
        .sheet
        .built
        .as_ref()
        .is_some_and(|built| built.shape != shape || built.layout != layout);
    if reshaped {
        let controls = graph.as_ref().map(|graph| graph::build(graph, window, cx));
        if let Some(built) = editor.sheet.built.as_mut() {
            built.shape = shape;
            built.layout = layout;
            built.graph = graph;
            built.controls = controls;
        }
    }
    resync(editor, window, cx);
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
fn menu_visibility(state: &Editor, menu: Menu) -> MenuVisibility {
    match state.sheet.closing {
        _ if state.sheet.open == Some(menu) => MenuVisibility::Open,
        Some((closing, visibility)) if closing == menu => visibility,
        _ => MenuVisibility::Closed,
    }
}

/// Ask for the venue's group names, once.
fn ensure_groups(editor: &mut Editor, cx: &mut Context<Luma>) {
    if !matches!(editor.sheet.groups, Groups::NotAsked) {
        return;
    }
    editor.sheet.groups = Groups::Loading;
    let venue = editor.venue_id.clone();
    cx.spawn(async move |this, cx| {
        let Ok(pending) = this.update(cx, |this, _| this.library.venue_groups(&venue)) else {
            return;
        };
        let rows = pending.await;
        this.update(cx, |this, cx| {
            this.with_track_editor(cx, |editor| {
                if editor.venue_id != venue {
                    return;
                }
                let names: Vec<SharedString> = rows
                    .into_iter()
                    .flatten()
                    .filter_map(|group| group.name)
                    .map(SharedString::from)
                    .collect();
                editor.sheet.groups = Groups::Ready(Rc::new(names));
            });
        })
        .ok();
    })
    .detach();
}

/// Ask for the heads of the primary clip's selection when its graph lies
/// across space, once per venue, selection and seed.
fn ensure_heads(editor: &mut Editor, cx: &mut Context<Luma>) {
    let Some(core) = primary_clip(editor).and_then(|clip| clip.core.as_ref()) else {
        return;
    };
    if !core
        .graph
        .nodes
        .values()
        .any(|node| node.kind == Kind::Space)
    {
        return;
    }
    let seed = core.selection_seed.unwrap_or(core.seed);
    let selection = core.selection.clone();
    let key = (
        editor.venue_id.clone(),
        selection.to_value().to_string(),
        seed,
    );
    if editor.sheet.heads.key.as_ref() == Some(&key) {
        return;
    }
    editor.sheet.heads = Heads {
        key: Some(key.clone()),
        cells: None,
    };
    cx.spawn(async move |this, cx| {
        let Ok(pending) = this.update(cx, |this, _| {
            this.library.selection_cells(&key.0, &selection, seed)
        }) else {
            return;
        };
        let cells = pending.await;
        this.update(cx, |this, cx| {
            this.with_track_editor(cx, |editor| {
                if editor.sheet.heads.key.as_ref() != Some(&key) {
                    return;
                }
                // A selection that does not resolve has no heads to show.
                editor.sheet.heads.cells = Some(cells.unwrap_or_default().into());
            });
        })
        .ok();
    })
    .detach();
}

/// The name, selection and blend controls for one subject; the graph's are
/// built on the first sync after, from its shape.
fn build(
    editor: &Editor,
    primary: SharedString,
    window: &mut Window,
    cx: &mut Context<Luma>,
) -> Built {
    let mut subs = Vec::new();
    let clip = primary_clip(editor);
    let core = clip.and_then(|clip| clip.core.as_ref());
    let name_text = core.map_or_else(String::new, |core| core.name.clone());
    let placeholder = clip.map_or_else(String::new, |clip| clip.summary.to_string());
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
                    this.rename_clips(name, cx);
                }
                // Escape puts the stored name back.
                text_input::Event::Cancelled => this.with_track_editor(cx, |editor| {
                    if let Some(built) = editor.sheet.built.as_mut() {
                        built.synced_name = Default::default();
                    }
                }),
                _ => {}
            }
        },
    ));

    let groups: Vec<SharedString> = match &editor.sheet.groups {
        Groups::Ready(names) => names.as_ref().clone(),
        _ => Vec::new(),
    };
    let expression = core.map_or_else(String::new, |core| core.selection.expression.clone());
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
        count: editor.selected.len(),
        shape: None,
        layout: None,
        name,
        synced_name: (name_text, placeholder),
        selection,
        synced_selection: expression,
        blend: clip.map_or(BlendMode::Replace, |clip| clip.blend),
        output: core.and_then(|core| core.graph.output_kind()),
        graph: None,
        controls: None,
        _subs: subs,
    }
}

/// Refresh every reading the sheet draws, and push externally moved values
/// into the widgets that show them.
///
/// "Externally" is anything that rewrote the working copy — an undo, a lost
/// write reloading, another gesture — including this sheet's own edits, whose
/// echo the synced copies filter out: a value the sheet itself wrote comes
/// back equal, so a widget's in-progress state survives.
fn resync(editor: &mut Editor, window: &mut Window, cx: &mut Context<Luma>) {
    let Some(clip) = primary_clip(editor).cloned() else {
        return;
    };
    let focused = editor
        .sheet
        .built
        .as_ref()
        .is_some_and(|built| built.name.read(cx).focus_handle(cx).is_focused(window));
    let Some(built) = editor.sheet.built.as_mut() else {
        return;
    };
    built.blend = clip.blend;
    let Some(core) = clip.core.as_ref() else {
        return;
    };
    built.output = core.graph.output_kind();
    let name = (core.name.clone(), clip.summary.to_string());
    if built.synced_name != name && !focused {
        built.name.update(cx, |field, cx| {
            field.set_placeholder(name.1.clone(), cx);
            field.set_text(name.0.clone(), cx);
        });
        built.synced_name = name;
    }
    if built.synced_selection != core.selection.expression {
        let expression = core.selection.expression.clone();
        built
            .selection
            .update(cx, |field, cx| field.set_text(expression.clone(), cx));
        built.synced_selection = expression;
    }
    if built.shape.is_some() {
        if let Some(controls) = built.controls.as_mut() {
            graph::sync(controls, &core.graph, window, cx);
        }
        built.graph = Some(core.graph.clone());
    }
}

// -- the write paths ----------------------------------------------------------

impl Luma {
    /// A blend pick from the sheet: every selected clip whose output takes
    /// the mode takes it, in one committed write. An aim takes replace or
    /// offset; light takes the light modes.
    pub(crate) fn sheet_blend(&mut self, mode: BlendMode, cx: &mut Context<Self>) {
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

    /// A name from the sheet, on every selected clip, in one committed write.
    /// A clip needs a name: a cleared field names each clip after its
    /// summary.
    fn rename_clips(&mut self, name: String, cx: &mut Context<Self>) {
        let named = |clip: &Clip| {
            if name.is_empty() {
                clip.summary.to_string()
            } else {
                name.clone()
            }
        };
        let unchanged = self.track_editor_ref().is_none_or(|editor| {
            selected_clips(editor).all(|clip| {
                clip.core
                    .as_ref()
                    .is_some_and(|core| core.name == named(clip))
            })
        });
        if unchanged {
            // The field may still read empty; put the stored name back.
            self.with_track_editor(cx, |editor| {
                if let Some(built) = editor.sheet.built.as_mut() {
                    built.synced_name = Default::default();
                }
            });
            return;
        }
        self.track_command(
            |editor| {
                let mut clips: Vec<Clip> = editor.clips.iter().cloned().collect();
                for clip in &mut clips {
                    if !editor.selected.contains(&clip.id) {
                        continue;
                    }
                    let name = named(clip);
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

    /// The fast path: apply `edit` to every selected clip now — working copy
    /// and heatmap previews — and owe the seam one write on the trailing
    /// edge. A clip `edit` leaves failing its checks keeps its old body, and
    /// the sheet says why.
    ///
    /// The first edit of a burst records the [`History`] checkpoint; the
    /// flush closes the burst, so a whole picker drag is one undo step and
    /// one write.
    fn clips_live(
        &mut self,
        cx: &mut Context<Self>,
        mut edit: impl FnMut(&mut luma_patterns::Clip),
    ) {
        let mut touched: Vec<SharedString> = Vec::new();
        self.with_track_editor(cx, |editor| {
            if !editor.writable() || editor.selected.is_empty() {
                return;
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
                let mut edited = core.clone();
                edit(&mut edited);
                if edited == *core {
                    continue;
                }
                if let Err(error) = luma_patterns::Score::validate_clip(&library, &clip.id, &edited)
                {
                    refused = Some(error.to_string());
                    continue;
                }
                *core = edited;
                clip.refresh();
                touched.push(clip.id.clone());
            }
            editor.sheet.error = refused;
            if touched.is_empty() {
                return;
            }
            if !editor.sheet.burst {
                editor.checkpoint();
                editor.sheet.burst = true;
            }
            editor.replace_clips(clips);
        });
        if touched.is_empty() {
            return;
        }
        for id in touched {
            self.refresh_clip_preview(id, cx);
        }
        self.schedule_arg_flush(cx);
    }

    /// A graph edit from the sheet or the timeline, on every selected clip.
    pub(crate) fn graph_live(
        &mut self,
        cx: &mut Context<Self>,
        mut edit: impl FnMut(&mut ClipGraph),
    ) {
        self.clips_live(cx, |clip| edit(&mut clip.graph));
    }

    /// A selection edit: the expression field and the fixture picker commit
    /// independently, and each writes the whole selection back.
    pub(crate) fn selection_live(
        &mut self,
        cx: &mut Context<Self>,
        mut edit: impl FnMut(&mut Selection),
    ) {
        self.clips_live(cx, |clip| edit(&mut clip.selection));
    }

    /// Open the fixture picker on the selected clips' selection.
    ///
    /// The sheet reads out what the dialog cannot see for itself: what the
    /// selection currently says, and the venue's group vocabulary the
    /// expression field already loaded for its autocomplete — asking for it
    /// twice would be a second load of the same list.
    fn pick_fixtures(&mut self, cx: &mut Context<Self>) {
        let mut opened = None;
        self.with_track_editor(cx, |editor| {
            let groups = match &editor.sheet.groups {
                Groups::Ready(names) => names.as_ref().clone(),
                Groups::NotAsked | Groups::Loading => Vec::new(),
            };
            let selection = primary_clip(editor)
                .and_then(|clip| clip.core.as_ref())
                .map_or_else(Selection::all, |core| core.selection.clone());
            opened = Some((editor.venue_id.clone(), groups, selection));
        });
        if let Some((venue, groups, selection)) = opened {
            self.open_fixture_picker(venue, groups, &selection, cx);
        }
    }

    /// The open track editor, read-only.
    fn track_editor_ref(&self) -> Option<&Editor> {
        match self.workspace.active_body() {
            Some(Body::TrackEditor(editor)) => Some(editor),
            _ => None,
        }
    }

    /// Arm (or re-arm) the trailing commit. Serialized under
    /// [`Luma::commit_clips`]'s own in-flight discipline, so a burst that
    /// outruns a slow write queues exactly one follow-up.
    fn schedule_arg_flush(&mut self, cx: &mut Context<Self>) {
        let Some(Body::TrackEditor(editor)) = self.workspace.active_body_mut() else {
            return;
        };
        let Some(score_id) = editor.score.as_ref().map(|score| score.id.clone()) else {
            return;
        };
        let target = Target::TrackEditor {
            track: editor.track_id.to_string(),
            venue: editor.venue_id.clone(),
        };
        editor.sheet.flush_gen += 1;
        let generation = editor.sheet.flush_gen;
        let pending = self.library.debounce(ARG_FLUSH);
        cx.spawn(async move |this, cx| {
            pending.await;
            this.update(cx, |this, cx| {
                let mut flush = false;
                let mut graph = false;
                this.edit_track_tab(&target, cx, |editor| {
                    if editor.score.as_ref().map(|score| &score.id) != Some(&score_id) {
                        return;
                    }
                    if editor.sheet.flush_gen == generation {
                        editor.sheet.burst = false;
                        flush = true;
                        graph = editor.graph_score.is_some();
                    }
                });
                if flush {
                    if graph {
                        this.commit_graph_score_for(target, cx);
                    } else if this.workspace.active() == Some(&target) {
                        this.commit_clips(cx);
                    }
                }
            })
            .ok();
        })
        .detach();
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

/// The inspector: the selected clip's controls, or the preset browser. It
/// is always open: a column beside the stage, as wide as the shell gives it
/// (widened for the graph when `wide`), or, with `fill` (split view), the
/// whole box it is given under the stage.
pub(super) fn panel(state: &Editor, app: &Entity<Luma>, fill: bool, wide: bool) -> AnyElement {
    let (label, body) = match state.sheet.built.as_ref() {
        Some(built) => (
            "Clip graph",
            body(state, built, app, (!fill).then_some(wide)),
        ),
        None => ("Presets", browser::body(state, app)),
    };
    div()
        .id("clip-inspector")
        .size_full()
        .min_h_0()
        .overflow_hidden()
        .bg(ladder::background())
        // Beside the stage its trailing edge is a rule; under it, the seam
        // above is the only rule it needs.
        .when(!fill, |content| {
            content.border_r_1().border_color(ladder::trim())
        })
        .child(body)
        .agent_node(Role::Card, label)
        .into_any_element()
}

/// The sheet's content: the name, the selection and blend rows, then the
/// graph canvas filling the rest.
fn body(state: &Editor, built: &Built, app: &Entity<Luma>, wide: Option<bool>) -> AnyElement {
    let pad = px(luma_ui::sheet::PAD);
    let mut rows: Vec<AnyElement> = Vec::new();
    let mut graph = None;
    if built.shape.is_some() {
        rows.push(named("Selection", selection_row(built, app)));
        rows.push(named("Blend", blend_select(state, built, app)));
        if let Some(error) = &state.sheet.error {
            rows.push(
                div()
                    .text_size(px(12.))
                    .text_color(ladder::danger())
                    .child(error.clone())
                    .agent_node(Role::Text, error.clone())
                    .into_any_element(),
            );
        }
        if let (Some(controls), Some(shown)) = (&built.controls, &built.graph) {
            graph = Some(graph::editor(state, controls, shown, app, wide));
        }
    }
    let header = div()
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(6.))
        .px(pad)
        .pt(pad)
        .pb(px(12.))
        .when(built.count > 1, |header| {
            let count = format!("{} clips", built.count);
            header.child(
                div()
                    .text_size(px(13.))
                    .text_color(ladder::foreground())
                    .child(count.clone())
                    .agent_node(Role::Text, count),
            )
        })
        .child(
            div()
                .w(px(FIELD_W))
                .key_context(text_input::DRAFT_CONTEXT)
                .child(built.name.clone())
                .agent_node(Role::Input, "Name"),
        );
    div()
        .size_full()
        .min_h_0()
        .flex()
        .flex_col()
        .child(header)
        .child(luma_ui::float::divider())
        .child(
            div()
                .id("args-sheet-fields")
                .flex_none()
                .w(px(FIELD_W + 2. * luma_ui::sheet::PAD))
                .px(pad)
                .py(px(12.))
                .flex()
                .flex_col()
                .gap(px(ROW_GAP))
                .children(rows),
        )
        .children(graph.map(|graph| {
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .border_t_1()
                .border_color(ladder::trim())
                .child(graph)
        }))
        .into_any_element()
}

/// The selection row: the expression field, and a chip that opens the
/// fixture picker. Both write the same value through `selection_live`.
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
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.))
        .child(built.selection.clone())
        .child(pick_chip)
}

/// The blend row: the modes the output takes, applied to the whole
/// selection on pick.
fn blend_select(state: &Editor, built: &Built, app: &Entity<Luma>) -> Div {
    let modes = blend_modes(built.output.map_or("color", Kind::name));
    let names: Vec<&str> = modes.iter().map(|mode| mode.label()).collect();
    let open = menu_visibility(state, Menu::Blend);
    let toggle = app.clone();
    let pick = app.clone();
    luma_arg_select(
        "blend",
        built.blend.label(),
        &names,
        open,
        move |_, cx| {
            toggle.update(cx, |this, cx| {
                this.with_track_editor(cx, |editor| {
                    editor.sheet.open = match editor.sheet.open {
                        Some(Menu::Blend) => None,
                        _ => Some(Menu::Blend),
                    };
                });
            });
        },
        move |index, _, cx| {
            pick.update(cx, |this, cx| {
                this.with_track_editor(cx, |editor| editor.sheet.open = None);
                this.sheet_blend(modes[index], cx);
            });
        },
    )
}

/// Air between a row's header line and its control.
const LABEL_GAP: f32 = 6.;

/// One labelled row, named for the agent tree.
fn named(label: &str, control: Div) -> AnyElement {
    sheet_row(label, Vec::new(), control)
}

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
