//! The args inspector: a sheet over the right edge of the timeline.
//!
//! A selected clip slides one in; clearing the selection slides it out;
//! selecting a *different* clip retargets the one that is up, without a
//! close-and-reopen bounce. The frame, the slide and the ghost are
//! [`luma_ui::sheet`]'s; this module owns what goes in it.
//!
//! # Non-modal, and that is load-bearing
//!
//! The timeline keeps every click, key and wheel it had while the sheet is
//! open — the sheet occludes its own box and nothing else. That is what lets a
//! person nudge a clip and then correct its intensity without dismissing
//! anything, and it is why this is a sheet rather than a dialog.
//!
//! # What populates it
//!
//! The clip selection. One clip shows its pattern's name; several sharing a
//! pattern show the name and a count; a mixed selection names only the count
//! and offers no args, because there is no one schema to edit against. The
//! blend select follows the *primary* (first selected) clip; blend applies to
//! every selected clip, args batch-apply whenever the whole selection shares
//! the pattern.
//!
//! A clip's span is not here. Bounds are edited by dragging the clip on the
//! timeline, which is the one place they can be read against the waveform they
//! mean something relative to; the sheet is for args.
//!
//! # Two write paths, deliberately
//!
//! A blend pick is an ordinary committed write: it rides
//! [`Luma::track_command`], so the gesture is one [`History`] checkpoint and
//! one compare-and-swap publish, exactly like a drag on the canvas.
//!
//! Arg edits ride the web's fast path instead. A change lands in the working
//! copy at once and re-renders the touched clips' heatmap previews (through
//! [`Luma::refresh_clip_preview`]'s per-clip coalescing), so the picture
//! answers the pointer live; the *write* trails on a ~250 ms debounce
//! ([`ARG_FLUSH`]) and goes through the same serialized [`Luma::commit_clips`]
//! as everything else, so a burst of picker motion is one checkpoint and one
//! write, and two consecutive tweaks cannot race their own compare-and-swap.
//!
//! # The rendered state is [`Built`], never the selection
//!
//! Every reading the sheet draws — the pattern line, the blend mode, each
//! cell's value — is taken from [`Built`], which [`resync`] refreshes against
//! the working copy once per frame. Nothing in the render path reaches for the
//! primary clip. That is what makes a *leaving* sheet legible: the selection is
//! already empty while the exit plays, so a render that read through it would
//! spend the slide showing a pattern's defaults instead of what the user just
//! deselected.
//!
//! # Where the schema comes from
//!
//! The arg definitions are the pattern's, read venue-resolved through
//! [`Library::pattern_args`] and cached per pattern for the life of the
//! editor. The blend list is [`BlendMode::ALL`] — the seam's one canonical
//! list — matched exhaustively nowhere and copied nowhere. The autocomplete
//! vocabulary for selection expressions is the venue's group names, read once
//! through [`Library::venue_groups`].

use luma_lib::models::node_graph::{PatternArgDef, PatternArgType};
use luma_lib::models::selection::{Selection, Subset};
use luma_ui::arg::arg_row;
use luma_ui::arg::color::{luma_hsv_picker, ColorArg, ColorArgEditor, ColorArgEvent, Hsv};
use luma_ui::arg::expression::{ExpressionEvent, GroupExpressionEditor};
use luma_ui::arg::gradient::{luma_gradient_bar, Gradient, GradientEvent, GradientStop};
use luma_ui::arg::number::{DraftedNumber, NumberEvent};
use luma_ui::arg::palette::{luma_palette_row, PaletteEvent};
use luma_ui::arg::select::luma_arg_select;
use luma_ui::pane::PaneWidth;
use luma_ui::CONTROL_HEIGHT;

use super::*;

/// Air between one arg row and the next, and between the sheet's bands.
const ROW_GAP: f32 = 14.;

/// A full-bleed control's width inside the sheet.
const FIELD_W: f32 = luma_ui::sheet::CONTENT_WIDTH;

/// The expression field, which shares its row with the fixture-picker chip.
const EXPR_W: f32 = FIELD_W - 62.;

/// The trailing edge a burst of live arg edits is committed on — the web
/// panel's 250 ms.
const ARG_FLUSH: Duration = Duration::from_millis(250);

/// The subset select's rows: how much of the expression's match to light.
///
/// A closed ladder, not a number field: the shares a lighting desk actually
/// asks for are halves and thirds. A value
/// authored elsewhere (Python, an agent) that is not on the ladder still shows,
/// via [`subset_label`]; picking then snaps to a rung.
pub(crate) const SUBSETS: [(&str, Subset); 7] = [
    ("All", Subset::All),
    ("1/2", Subset::Fraction(0.5)),
    ("1/3", Subset::Fraction(1. / 3.)),
    ("1/4", Subset::Fraction(0.25)),
    ("1", Subset::Count(1)),
    ("2", Subset::Count(2)),
    ("3", Subset::Count(3)),
];

/// What the subset cell shows. Off-ladder values keep their own reading —
/// a percentage for a share, a bare number for a count — so an agent's
/// `subset=0.7` is legible rather than silently displayed as "All".
pub(crate) fn subset_label(subset: Subset) -> SharedString {
    if let Some((label, _)) = SUBSETS.iter().find(|(_, rung)| *rung == subset) {
        return (*label).into();
    }
    match subset {
        Subset::All => "All".into(),
        Subset::Fraction(f) => format!("{}%", (f * 100.).round()).into(),
        Subset::Count(c) => c.to_string().into(),
    }
}

// -- state --------------------------------------------------------------------

/// The sheet's own state, owned by the [`Editor`].
pub(crate) struct State {
    /// How much of the sheet is on screen — see [`luma_ui::sheet`]. The one
    /// place "is the sheet up" is stored: [`PaneWidth::target`] is the
    /// destination and [`PaneWidth::current`] is the picture, so the two
    /// cannot disagree mid-slide the way a separate flag would.
    slide: PaneWidth,
    /// The venue's group names, for the expression editor's autocomplete.
    groups: Groups,
    /// Arg definitions per pattern id, venue-resolved, cached for the life of
    /// the editor — the web store's `patternArgs`.
    defs: HashMap<String, Rc<[PatternArgDef]>>,
    defs_inflight: HashSet<String>,
    /// Everything the sheet draws, for as long as it is on screen — including
    /// the slide out, after the selection it was built from is already gone.
    built: Option<Built>,
    /// Which sheet-owned menu is open. One at a time — opening one closes the
    /// rest, which is what a single field states for free.
    open: Option<Menu>,
    /// A live arg burst is running: its checkpoint is recorded and a trailing
    /// commit is owed.
    burst: bool,
    /// Debounce generation for the trailing commit; each live edit retires
    /// the timer before it.
    flush_gen: u64,
}

impl Default for State {
    fn default() -> Self {
        Self {
            slide: PaneWidth::new(0.),
            groups: Groups::NotAsked,
            defs: HashMap::new(),
            defs_inflight: HashSet::new(),
            built: None,
            open: None,
            burst: false,
            flush_gen: 0,
        }
    }
}

impl State {
    /// Whether the sheet is up — heading open, not merely still painted. What
    /// `Escape` asks before it decides the key meant "clear the selection".
    pub(crate) fn is_open(&self) -> bool {
        self.slide.target() > 0.
    }

    /// Close whichever menu the sheet has up, reporting whether there was one.
    pub(crate) fn dismiss_menu(&mut self) -> bool {
        self.open.take().is_some()
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
    /// The subset select of the selection cell at this index.
    Subset(usize),
    /// The HSV plate for the selected swatch/stop of the cell at this index.
    Swatch(usize),
}

/// What the entities were built for, the entities themselves, and every
/// reading the sheet draws — see the module docs on why the render path may
/// not go back to the selection for them.
struct Built {
    /// The primary (first selected) clip the cells read their values from.
    primary: SharedString,
    /// The selection's shared pattern; `None` for a mixed selection, which
    /// has no args to offer.
    pattern: Option<SharedString>,
    /// The header line: a pattern name, a name and a count, or a bare count.
    reading: SharedString,
    /// The primary clip's blend mode, which the select reads.
    blend: BlendMode,
    cells: Vec<Cell>,
    _subs: Vec<Subscription>,
}

/// One arg's slot: its definition, its widget, and the wire value the widget
/// was last pointed at — pushed again only when the stored value moves, so an
/// in-progress draft or drag is never stomped by its own echo.
struct Cell {
    def: PatternArgDef,
    synced: serde_json::Value,
    widget: Widget,
}

enum Widget {
    Color(Entity<ColorArgEditor>),
    Scalar(Entity<DraftedNumber>),
    Selection(Entity<GroupExpressionEditor>),
    /// Stateless kit rows keep their selection (and the picker's working HSV)
    /// here, on the host — the kit's contract.
    Palette {
        selected: Option<usize>,
        hsv: Hsv,
    },
    Gradient {
        selected: Option<usize>,
        hsv: Hsv,
    },
}

// -- wire codecs --------------------------------------------------------------
//
// The sheet's serialization edge: everything below speaks the widget kit's
// typed values, everything above speaks the args JSON the score stores. The
// shapes are the web panel's exactly — colors as 0–255 rgb with the tri-mode
// alpha, palettes as hex lists, gradients as (color, t) stops.

fn color_from_wire(value: &serde_json::Value, fallback: &serde_json::Value) -> ColorArg {
    let read = |value: &serde_json::Value, key: &str| value.get(key).and_then(|v| v.as_f64());
    let channel = |key: &str, or: f64| {
        read(value, key)
            .or_else(|| read(fallback, key))
            .unwrap_or(or)
    };
    let rgb = [
        (channel("r", 255.) / 255.) as f32,
        (channel("g", 0.) / 255.) as f32,
        (channel("b", 0.) / 255.) as f32,
    ];
    ColorArg::decode(rgb, channel("a", 1.) as f32)
}

fn color_to_wire(arg: ColorArg) -> serde_json::Value {
    let (rgb, alpha) = arg.encode();
    serde_json::json!({
        "r": f64::from((rgb[0] * 255.).round()),
        "g": f64::from((rgb[1] * 255.).round()),
        "b": f64::from((rgb[2] * 255.).round()),
        "a": f64::from(alpha),
    })
}

fn scalar_from_wire(value: &serde_json::Value, fallback: &serde_json::Value) -> f64 {
    value
        .as_f64()
        .or_else(|| fallback.as_f64())
        .filter(|v| v.is_finite())
        .unwrap_or(1.)
}

/// A stored arg value as a selection, falling back to the whole venue when the
/// value is missing or malformed — an arg row always has something to show.
fn selection_from_wire(value: &serde_json::Value) -> Selection {
    Selection::from_value(value).unwrap_or_else(Selection::all)
}

fn hex_to_rgba(hex: &str) -> Option<Rgba> {
    let hex = hex.strip_prefix('#')?;
    if hex.len() < 6 {
        return None;
    }
    let byte = |at: usize| u8::from_str_radix(&hex[at..at + 2], 16).ok();
    Some(Rgba {
        r: f32::from(byte(0)?) / 255.,
        g: f32::from(byte(2)?) / 255.,
        b: f32::from(byte(4)?) / 255.,
        a: 1.,
    })
}

fn rgba_to_hex(color: Rgba) -> String {
    format!(
        "#{:02x}{:02x}{:02x}",
        (color.r * 255.).round() as u8,
        (color.g * 255.).round() as u8,
        (color.b * 255.).round() as u8
    )
}

/// The web panel's palette fallback, for an arg with no value and no default.
const PALETTE_FALLBACK: [&str; 3] = ["#ff0080", "#00ffc8", "#ffbe28"];

fn palette_from_wire(value: &serde_json::Value, fallback: &serde_json::Value) -> Vec<Rgba> {
    let colors = |value: &serde_json::Value| -> Option<Vec<Rgba>> {
        let list = value.get("colors")?.as_array()?;
        let parsed: Vec<Rgba> = list
            .iter()
            .filter_map(|c| hex_to_rgba(c.as_str()?))
            .collect();
        (!parsed.is_empty()).then_some(parsed)
    };
    colors(value)
        .or_else(|| colors(fallback))
        .unwrap_or_else(|| {
            PALETTE_FALLBACK
                .iter()
                .filter_map(|c| hex_to_rgba(c))
                .collect()
        })
}

fn palette_to_wire(colors: &[Rgba]) -> serde_json::Value {
    serde_json::json!({
        "colors": colors.iter().map(|c| rgba_to_hex(*c)).collect::<Vec<_>>(),
    })
}

fn gradient_from_wire(value: &serde_json::Value, fallback: &serde_json::Value) -> Gradient {
    let stops = |value: &serde_json::Value| -> Option<Vec<GradientStop>> {
        let list = value.get("stops")?.as_array()?;
        let parsed: Vec<GradientStop> = list
            .iter()
            .filter_map(|stop| {
                Some(GradientStop {
                    t: stop.get("t")?.as_f64()? as f32,
                    color: hex_to_rgba(stop.get("color")?.as_str()?)?,
                })
            })
            .collect();
        (!parsed.is_empty()).then_some(parsed)
    };
    Gradient::new(stops(value).or_else(|| stops(fallback)).unwrap_or_default())
}

fn gradient_to_wire(gradient: &Gradient) -> serde_json::Value {
    serde_json::json!({
        "stops": gradient
            .stops()
            .iter()
            .map(|stop| serde_json::json!({ "color": rgba_to_hex(stop.color), "t": f64::from(stop.t) }))
            .collect::<Vec<_>>(),
    })
}

// -- subject ------------------------------------------------------------------

/// The selection's shared pattern id, or `None` when the selection is empty
/// or mixed. `(pattern, mixed)` because those two `None`s render differently.
fn shared_pattern(editor: &Editor) -> Option<SharedString> {
    let mut selected = editor
        .clips
        .iter()
        .filter(|clip| editor.selected.contains(&clip.id));
    let first = selected.next()?.pattern.clone();
    selected.all(|clip| clip.pattern == first).then_some(first)
}

fn primary_clip(editor: &Editor) -> Option<&Clip> {
    let id = editor.selected.first()?;
    editor.clips.iter().find(|clip| &clip.id == id)
}

/// A cell's stored wire value: the primary clip's, falling back to the def's
/// default — the same read the web panel makes.
fn stored_arg(editor: &Editor, def: &PatternArgDef) -> serde_json::Value {
    primary_clip(editor)
        .and_then(|clip| clip.args.get(&def.id).cloned())
        .unwrap_or_else(|| def.default_value.clone())
}

// -- sync ---------------------------------------------------------------------

/// Reconcile the sheet against the editor, on every frame the editor draws,
/// and report how much of it is on screen this frame.
///
/// Four jobs, all idempotent: aim the slide at whatever the selection says;
/// ask for the vocabulary and the schema the sheet is missing (venue groups,
/// the selected pattern's arg defs — each asked for once); rebuild the widget
/// entities when the *subject* changed (another primary clip, another pattern,
/// the defs arriving); and refresh [`Built`]'s readings from the working copy,
/// guarded per cell so a draft in progress is never stomped by its own echo.
///
/// Runs in the render pass because that is the one place every path that can
/// change the subject already converges — and the one place a [`Window`] is
/// in hand to build entities with and to ask for the slide's next frame.
pub(super) fn sync(editor: &mut Editor, window: &mut Window, cx: &mut Context<Luma>) -> Pixels {
    ensure_groups(editor, cx);
    ensure_defs(editor, cx);

    let primary = primary_clip(editor).map(|clip| clip.id.clone());
    editor.sheet.slide.retarget(
        if primary.is_some() {
            luma_ui::sheet::WIDTH
        } else {
            0.
        },
        cx,
    );
    let revealed = editor.sheet.slide.eval(window);

    let Some(primary) = primary else {
        // Nothing selected: the sheet is leaving, and what it draws on the way
        // out is what it was already drawing. Only once it is wholly gone is
        // the state it was drawing from let go.
        if revealed <= px(0.) {
            editor.sheet.built = None;
            editor.sheet.open = None;
        }
        return revealed;
    };
    let pattern = shared_pattern(editor);
    let defs = pattern
        .as_ref()
        .and_then(|id| editor.sheet.defs.get(id.as_ref()).cloned())
        .unwrap_or_else(|| Rc::from(Vec::new()));

    let stale = match &editor.sheet.built {
        Some(built) => {
            built.primary != primary || built.pattern != pattern || built.cells.len() != defs.len()
        }
        None => true,
    };
    if stale {
        editor.sheet.open = None;
        let built = build(editor, primary, pattern, &defs, window, cx);
        editor.sheet.built = Some(built);
    }
    resync(editor, cx);
    revealed
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

/// Ask for the selected pattern's arg defs, once per pattern.
fn ensure_defs(editor: &mut Editor, cx: &mut Context<Luma>) {
    let Some(pattern) = shared_pattern(editor) else {
        return;
    };
    let key = pattern.to_string();
    if editor.sheet.defs.contains_key(&key) || editor.sheet.defs_inflight.contains(&key) {
        return;
    }
    editor.sheet.defs_inflight.insert(key.clone());
    let venue = editor.venue_id.clone();
    cx.spawn(async move |this, cx| {
        let Ok(pending) = this.update(cx, |this, _| this.library.pattern_args(&key, &venue)) else {
            return;
        };
        let rows = pending.await;
        this.update(cx, |this, cx| {
            this.with_track_editor(cx, |editor| {
                editor.sheet.defs_inflight.remove(&key);
                if editor.venue_id != venue {
                    return;
                }
                // A pattern with no graph behind it answers with an error;
                // that is a pattern with no args, and asking again would only
                // fail again.
                editor
                    .sheet
                    .defs
                    .insert(key.clone(), rows.unwrap_or_default().into());
            });
        })
        .ok();
    })
    .detach();
}

/// Build the widget entities for one subject and wire their events into the
/// two write paths.
fn build(
    editor: &Editor,
    primary: SharedString,
    pattern: Option<SharedString>,
    defs: &Rc<[PatternArgDef]>,
    window: &mut Window,
    cx: &mut Context<Luma>,
) -> Built {
    let mut subs = Vec::new();

    let groups: Vec<SharedString> = match &editor.sheet.groups {
        Groups::Ready(names) => names.as_ref().clone(),
        _ => Vec::new(),
    };

    let cells = defs
        .iter()
        .map(|def| {
            let stored = stored_arg(editor, def);
            let widget = match def.arg_type {
                PatternArgType::Color => {
                    let value = color_from_wire(&stored, &def.default_value);
                    let entity = cx.new(|cx| ColorArgEditor::new(def.name.clone(), value, cx));
                    let arg_id = def.id.clone();
                    subs.push(cx.subscribe(
                        &entity,
                        move |this: &mut Luma, _, event: &ColorArgEvent, cx| {
                            let ColorArgEvent::Changed(value) = *event;
                            this.arg_live(&arg_id, color_to_wire(value), cx);
                        },
                    ));
                    Widget::Color(entity)
                }
                PatternArgType::Scalar => {
                    let value = scalar_from_wire(&stored, &def.default_value);
                    let entity = cx.new(|cx| {
                        DraftedNumber::new(def.name.clone(), value, -1e9, 1e9, FIELD_W, window, cx)
                    });
                    let arg_id = def.id.clone();
                    subs.push(cx.subscribe(
                        &entity,
                        move |this: &mut Luma, _, event: &NumberEvent, cx| {
                            let NumberEvent::Committed(value) = *event;
                            this.arg_live(&arg_id, serde_json::json!(value), cx);
                        },
                    ));
                    Widget::Scalar(entity)
                }
                PatternArgType::Selection => {
                    let entity = cx.new(|cx| {
                        GroupExpressionEditor::new(
                            groups.iter().cloned(),
                            selection_from_wire(&stored).expression,
                            EXPR_W,
                            window,
                            cx,
                        )
                    });
                    let arg_id = def.id.clone();
                    let def_for_event = def.clone();
                    subs.push(cx.subscribe(
                        &entity,
                        move |this: &mut Luma, _, event: &ExpressionEvent, cx| {
                            let ExpressionEvent::Committed(expression) = event.clone();
                            this.arg_selection(&arg_id, &def_for_event, cx, |selection| {
                                selection.expression = expression;
                            });
                        },
                    ));
                    Widget::Selection(entity)
                }
                PatternArgType::Palette => Widget::Palette {
                    selected: None,
                    hsv: Hsv {
                        h: 0.,
                        s: 0.,
                        v: 1.,
                    },
                },
                PatternArgType::Gradient => Widget::Gradient {
                    selected: None,
                    hsv: Hsv {
                        h: 0.,
                        s: 0.,
                        v: 1.,
                    },
                },
            };
            Cell {
                def: def.clone(),
                synced: stored,
                widget,
            }
        })
        .collect();

    Built {
        primary,
        pattern,
        reading: reading(editor),
        blend: primary_clip(editor).map_or(BlendMode::Replace, |clip| clip.blend),
        cells,
        _subs: subs,
    }
}

/// The header line for the current selection.
fn reading(editor: &Editor) -> SharedString {
    let count = editor.selected.len();
    let Some(clip) = primary_clip(editor) else {
        return SharedString::default();
    };
    match (shared_pattern(editor), count) {
        (Some(_), 1) => clip.label.clone(),
        (Some(_), n) => format!("{} ({n})", clip.label).into(),
        (None, n) => format!("{n} patterns").into(),
    }
}

/// Refresh every reading the sheet draws, and push externally moved values
/// into the widgets that show them.
///
/// "Externally" is anything that rewrote the working copy — an undo, a lost
/// write reloading, another gesture — including this sheet's own edits, whose
/// echo the per-cell `synced` guard filters out: a value the sheet itself
/// wrote round-trips byte-identical, so the guard sees no movement and the
/// widget's in-progress state survives.
fn resync(editor: &mut Editor, cx: &mut Context<Luma>) {
    let reading = reading(editor);
    let blend = primary_clip(editor).map_or(BlendMode::Replace, |clip| clip.blend);
    let stored: Vec<serde_json::Value> = editor
        .sheet
        .built
        .as_ref()
        .map(|built| {
            built
                .cells
                .iter()
                .map(|cell| stored_arg(editor, &cell.def))
                .collect()
        })
        .unwrap_or_default();
    let Some(built) = editor.sheet.built.as_mut() else {
        return;
    };
    built.reading = reading;
    built.blend = blend;

    for (cell, stored) in built.cells.iter_mut().zip(stored) {
        if cell.synced == stored {
            continue;
        }
        match &mut cell.widget {
            Widget::Color(entity) => {
                let value = color_from_wire(&stored, &cell.def.default_value);
                entity.update(cx, |editor, cx| editor.set_value(value, cx));
            }
            Widget::Scalar(entity) => {
                let value = scalar_from_wire(&stored, &cell.def.default_value);
                entity.update(cx, |field, cx| field.set_value(value, cx));
            }
            Widget::Selection(entity) => {
                let expression = selection_from_wire(&stored).expression;
                entity.update(cx, |editor, cx| editor.set_text(expression, cx));
            }
            // Stateless rows read `Cell::synced` at render; only the
            // selection index needs a bound check.
            Widget::Palette { selected, .. } => {
                let count = palette_from_wire(&stored, &cell.def.default_value).len();
                if selected.is_some_and(|index| index >= count) {
                    *selected = None;
                }
            }
            Widget::Gradient { selected, .. } => {
                let count = gradient_from_wire(&stored, &cell.def.default_value)
                    .stops()
                    .len();
                if selected.is_some_and(|index| index >= count) {
                    *selected = None;
                }
            }
        }
        cell.synced = stored;
    }
}

// -- the write paths ----------------------------------------------------------

impl Luma {
    /// A blend pick from the sheet: every selected clip takes the mode, in
    /// one committed write — the web's `updateAnnotationsBatch`.
    pub(crate) fn sheet_blend(&mut self, mode: BlendMode, cx: &mut Context<Self>) {
        self.track_command(
            move |editor| {
                if editor.selected.is_empty() {
                    return;
                }
                let mut clips: Vec<Clip> = editor.clips.iter().cloned().collect();
                for clip in &mut clips {
                    if editor.selected.contains(&clip.id) {
                        clip.blend = mode;
                    }
                }
                editor.replace_clips(clips);
            },
            cx,
        );
    }

    /// The fast path: land `value` on every selected clip's `arg_id` now —
    /// working copy and heatmap previews — and owe the seam one write on the
    /// trailing edge.
    ///
    /// The first edit of a burst records the [`History`] checkpoint; the
    /// flush closes the burst, so a whole picker drag is one undo step and
    /// one compare-and-swap.
    pub(crate) fn arg_live(
        &mut self,
        arg_id: &str,
        value: serde_json::Value,
        cx: &mut Context<Self>,
    ) {
        let mut touched: Vec<SharedString> = Vec::new();
        self.with_track_editor(cx, |editor| {
            if !editor.writable() || editor.selected.is_empty() {
                return;
            }
            if !editor.sheet.burst {
                editor.checkpoint();
                editor.sheet.burst = true;
            }
            let selected = editor.selected.clone();
            let mut clips: Vec<Clip> = editor.clips.iter().cloned().collect();
            for clip in &mut clips {
                if !selected.contains(&clip.id) {
                    continue;
                }
                match &mut clip.args {
                    serde_json::Value::Object(map) => {
                        map.insert(arg_id.to_string(), value.clone());
                    }
                    other => {
                        *other = serde_json::json!({ arg_id: value.clone() });
                    }
                }
                touched.push(clip.id.clone());
            }
            if touched.is_empty() {
                return;
            }
            editor.replace_clips(clips);
        });
        for id in touched {
            self.refresh_clip_preview(id, cx);
        }
        self.schedule_arg_flush(cx);
    }

    /// Open the fixture picker on one selection arg.
    ///
    /// The sheet reads out what the dialog cannot see for itself: which arg,
    /// what it currently says, and the venue's group vocabulary the expression
    /// field already loaded for its autocomplete — asking for it twice would
    /// be a second load of the same list.
    fn pick_fixtures(&mut self, def: &PatternArgDef, cx: &mut Context<Self>) {
        let mut opened = None;
        self.with_track_editor(cx, |editor| {
            let groups = match &editor.sheet.groups {
                Groups::Ready(names) => names.as_ref().clone(),
                Groups::NotAsked | Groups::Loading => Vec::new(),
            };
            opened = Some((
                editor.venue_id.clone(),
                groups,
                selection_from_wire(&stored_arg(editor, def)),
            ));
        });
        if let Some((venue, groups, selection)) = opened {
            self.open_fixture_picker(def.clone(), venue, groups, &selection, cx);
        }
    }

    /// A selection arg edit: apply `edit` to the whole stored selection, then
    /// ride the fast path. The cell's controls — expression, space, subset —
    /// commit independently, and each writes the whole value back, so editing
    /// one can never drop what the others hold.
    pub(crate) fn arg_selection(
        &mut self,
        arg_id: &str,
        def: &PatternArgDef,
        cx: &mut Context<Self>,
        edit: impl FnOnce(&mut Selection),
    ) {
        let mut wire = None;
        self.with_track_editor(cx, |editor| {
            let mut selection = selection_from_wire(&stored_arg(editor, def));
            edit(&mut selection);
            wire = Some(selection.to_value());
        });
        if let Some(wire) = wire {
            self.arg_live(arg_id, wire, cx);
        }
    }

    /// Arm (or re-arm) the trailing commit. Serialized under
    /// [`Luma::commit_clips`]'s own in-flight discipline, so a burst that
    /// outruns a slow write queues exactly one follow-up.
    fn schedule_arg_flush(&mut self, cx: &mut Context<Self>) {
        let mut gen = None;
        self.with_track_editor(cx, |editor| {
            editor.sheet.flush_gen += 1;
            gen = Some(editor.sheet.flush_gen);
        });
        let Some(gen) = gen else { return };
        let pending = self.library.debounce(ARG_FLUSH);
        cx.spawn(async move |this, cx| {
            pending.await;
            this.update(cx, |this, cx| {
                let mut flush = false;
                this.with_track_editor(cx, |editor| {
                    if editor.sheet.flush_gen == gen {
                        editor.sheet.burst = false;
                        flush = true;
                    }
                });
                if flush {
                    this.commit_clips(cx);
                }
            })
            .ok();
        })
        .detach();
    }
}

// -- rendering ----------------------------------------------------------------

/// The sheet, when there is any of it on screen.
///
/// `revealed` is [`sync`]'s return — zero means gone. Being *painted* and
/// being *open* are different questions and both are asked here: a sheet on
/// its way out is still painted, so it is still built, but it is no longer
/// open, which is what makes it a ghost.
pub(super) fn sheet(
    state: &Editor,
    revealed: Pixels,
    app: &Entity<Luma>,
) -> Option<impl IntoElement> {
    if revealed <= px(0.) {
        return None;
    }
    let built = state.sheet.built.as_ref()?;
    Some(
        luma_ui::sheet::Sheet {
            label: "Args sheet".into(),
            width: luma_ui::sheet::WIDTH,
            revealed,
            interactive: state.sheet.is_open(),
        }
        .render(body(state, built, app)),
    )
}

/// The sheet's content: what is selected, then the controls for it.
fn body(state: &Editor, built: &Built, app: &Entity<Luma>) -> AnyElement {
    let pad = px(luma_ui::sheet::PAD);
    div()
        .size_full()
        .min_h_0()
        .flex()
        .flex_col()
        .child(
            div()
                .flex_none()
                .flex()
                .flex_col()
                .gap(px(4.))
                .px(pad)
                .pt(pad)
                .pb(px(12.))
                .child(luma_ui::silkscreen("PATTERN".to_string()))
                .child(
                    div()
                        .w_full()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_size(px(13.))
                        .text_color(ladder::foreground())
                        .child(built.reading.clone())
                        .agent_node(Role::Text, built.reading.clone()),
                ),
        )
        .child(luma_ui::float::divider())
        // The gutters live on a wrapper outside the scroller — `float::viewport`'s
        // contract — so a schema long enough to scroll still has air at both ends.
        .child(
            luma_ui::float::viewport().child(
                div()
                    .id("args-sheet-fields")
                    .size_full()
                    .overflow_y_scroll()
                    .px(pad)
                    .flex()
                    .flex_col()
                    .gap(px(ROW_GAP))
                    .child(named("blend", blend_select(state, built, app)))
                    .children(args(state, built, app)),
            ),
        )
        .into_any_element()
}

/// The pattern's own schema, or the one line that says why there is none.
fn args(state: &Editor, built: &Built, app: &Entity<Luma>) -> Vec<AnyElement> {
    let note = |message: &str| {
        vec![div()
            .child(luma_ui::silkscreen(message.to_string()))
            .opacity(ladder::DISABLED_OPACITY)
            .agent_node(Role::Text, message.to_string())
            .into_any_element()]
    };
    match &built.pattern {
        None => note("Mixed patterns"),
        Some(_) if built.cells.is_empty() => note("No args"),
        Some(_) => built
            .cells
            .iter()
            .enumerate()
            .flat_map(|(index, cell)| arg_rows(state, app, index, cell))
            .collect(),
    }
}

/// The blend row: the canonical nine, from [`BlendMode::ALL`] and nowhere
/// else, applied to the whole selection on pick.
fn blend_select(state: &Editor, built: &Built, app: &Entity<Luma>) -> Div {
    let names: Vec<&str> = BlendMode::ALL.iter().map(|mode| mode.name()).collect();
    let open = state.sheet.open == Some(Menu::Blend);
    let toggle = app.clone();
    let pick = app.clone();
    luma_arg_select(
        "blend",
        built.blend.name(),
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
                this.sheet_blend(BlendMode::ALL[index], cx);
            });
        },
    )
}

/// One arg's row (or two — a selection arg carries its subset select).
fn arg_rows(state: &Editor, app: &Entity<Luma>, index: usize, cell: &Cell) -> Vec<AnyElement> {
    let name = cell.def.name.as_str();
    let one = |control: Div| vec![named(name, control)];
    match &cell.widget {
        Widget::Color(entity) => one(div().child(entity.clone())),
        Widget::Scalar(entity) => one(div().child(entity.clone())),
        Widget::Selection(entity) => {
            let selection = selection_from_wire(&cell.synced);

            let subset_labels: Vec<&str> = SUBSETS.iter().map(|(label, _)| *label).collect();
            let toggle = app.clone();
            let pick = app.clone();
            let def = cell.def.clone();
            let amount = luma_arg_select(
                format!("{name}:subset"),
                &subset_label(selection.subset),
                &subset_labels,
                state.sheet.open == Some(Menu::Subset(index)),
                move |_, cx| {
                    toggle.update(cx, |this, cx| {
                        this.with_track_editor(cx, |editor| {
                            editor.sheet.open = match editor.sheet.open {
                                Some(Menu::Subset(at)) if at == index => None,
                                _ => Some(Menu::Subset(index)),
                            };
                        });
                    });
                },
                move |picked, _, cx| {
                    let def = def.clone();
                    pick.update(cx, |this, cx| {
                        this.with_track_editor(cx, |editor| editor.sheet.open = None);
                        this.arg_selection(&def.id, &def, cx, |selection| {
                            selection.subset = SUBSETS[picked].1;
                        });
                    });
                },
            );

            // The field stays the power user's spelling; the chip beside it
            // opens the picture. Both write the same value through
            // `arg_selection`, so neither is a second way to say it.
            let opened = app.clone();
            let picked_def = cell.def.clone();
            let pick_chip = luma_ui::float::chip()
                .id(SharedString::from(format!("{name}:pick")))
                .child("Pick")
                .on_click(move |_, _, cx| {
                    let def = picked_def.clone();
                    opened.update(cx, |this, cx| this.pick_fixtures(&def, cx));
                })
                .agent_node(Role::Button, "Pick fixtures");

            vec![
                named(
                    name,
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(6.))
                        .child(entity.clone())
                        .child(pick_chip),
                ),
                named("how many", amount),
            ]
        }
        Widget::Palette { selected, hsv } => {
            let colors = palette_from_wire(&cell.synced, &cell.def.default_value);
            one(palette_widget(
                app,
                index,
                cell,
                colors,
                *selected,
                *hsv,
                plate_open(state, index),
            ))
        }
        Widget::Gradient { selected, hsv } => {
            let gradient = gradient_from_wire(&cell.synced, &cell.def.default_value);
            one(gradient_widget(
                app,
                index,
                cell,
                gradient,
                *selected,
                *hsv,
                plate_open(state, index),
            ))
        }
    }
}

/// One labelled row, named for the agent tree.
fn named(label: &str, control: Div) -> AnyElement {
    arg_row(label, control)
        .agent_node(Role::Row, label.to_string())
        .into_any_element()
}

/// Whether the HSV plate for the cell at `index` is up.
fn plate_open(state: &Editor, index: usize) -> bool {
    state.sheet.open == Some(Menu::Swatch(index))
}

/// Run `edit` against one palette/gradient cell's widget state, from inside
/// a `Luma` update.
fn edit_widget(
    this: &mut Luma,
    index: usize,
    cx: &mut Context<Luma>,
    edit: impl FnOnce(&mut Widget),
) {
    this.with_track_editor(cx, |editor| {
        if let Some(built) = editor.sheet.built.as_mut() {
            if let Some(cell) = built.cells.get_mut(index) {
                edit(&mut cell.widget);
            }
        }
    });
}

/// The host-side HSV plate for a palette swatch or gradient stop — a float
/// (the float tier's card) anchored off the row; the window snap decides
/// which way it opens.
fn swatch_plate(
    id: String,
    hsv: Hsv,
    dismiss: impl Fn(&mut Window, &mut App) + 'static,
    on_change: impl Fn(Hsv, &mut Window, &mut App) + Clone + 'static,
) -> impl IntoElement {
    luma_ui::float::anchored_below(
        SharedString::from(format!("{id}:plate")),
        CONTROL_HEIGHT,
        luma_ui::float::Dismiss::on_press_out(dismiss),
        luma_ui::float::popover_card()
            .p(px(8.))
            .child(luma_hsv_picker(id, hsv, on_change))
            .into_any_element(),
    )
}

fn palette_widget(
    app: &Entity<Luma>,
    index: usize,
    cell: &Cell,
    colors: Vec<Rgba>,
    selected: Option<usize>,
    hsv: Hsv,
    plate_open: bool,
) -> Div {
    let def = cell.def.clone();
    let events = app.clone();
    let colors_for_events = colors.clone();
    let row = luma_palette_row(def.name.clone(), &colors, selected, move |event, _, cx| {
        let def = def.clone();
        let mut colors = colors_for_events.clone();
        events.update(cx, |this, cx| {
            let write = match event {
                PaletteEvent::Select(at) => {
                    let color = colors.get(at).copied();
                    this.with_track_editor(cx, |editor| {
                        if let Some(color) = color {
                            if let Some(built) = editor.sheet.built.as_mut() {
                                if let Some(cell) = built.cells.get_mut(index) {
                                    if let Widget::Palette { selected, hsv } = &mut cell.widget {
                                        *selected = Some(at);
                                        *hsv = Hsv::from_rgb([color.r, color.g, color.b]);
                                    }
                                }
                            }
                            editor.sheet.open = Some(Menu::Swatch(index));
                        }
                    });
                    None
                }
                PaletteEvent::Add => {
                    let last = colors.last().copied().unwrap_or(gpui::white().into());
                    colors.push(last);
                    Some(colors)
                }
                PaletteEvent::Remove(at) => {
                    if colors.len() > 1 && at < colors.len() {
                        colors.remove(at);
                        this.with_track_editor(cx, |editor| {
                            if let Some(built) = editor.sheet.built.as_mut() {
                                if let Some(cell) = built.cells.get_mut(index) {
                                    if let Widget::Palette { selected, .. } = &mut cell.widget {
                                        *selected = None;
                                    }
                                }
                            }
                            editor.sheet.open = None;
                        });
                        Some(colors)
                    } else {
                        None
                    }
                }
                PaletteEvent::Move { from, to } => {
                    if from < colors.len() && to < colors.len() {
                        let color = colors.remove(from);
                        colors.insert(to, color);
                        Some(colors)
                    } else {
                        None
                    }
                }
            };
            if let Some(colors) = write {
                this.arg_live(&def.id, palette_to_wire(&colors), cx);
            }
        });
    });
    let plate = plate_open.then_some(selected).flatten().map(|at| {
        let def = cell.def.clone();
        let picker_app = app.clone();
        let colors = colors.clone();
        let dismiss = app.clone();
        swatch_plate(
            format!("{}:swatch-picker", cell.def.name),
            hsv,
            move |_, cx| {
                dismiss.update(cx, |this, cx| {
                    if this.dismiss_sheet_menu() {
                        cx.notify();
                    }
                });
            },
            move |hsv, _, cx| {
                let def = def.clone();
                let mut colors = colors.clone();
                picker_app.update(cx, |this, cx| {
                    edit_widget(this, index, cx, |widget| {
                        if let Widget::Palette { hsv: held, .. } = widget {
                            *held = hsv;
                        }
                    });
                    if let Some(slot) = colors.get_mut(at) {
                        let [r, g, b] = hsv.to_rgb();
                        *slot = Rgba { r, g, b, a: 1. };
                        this.arg_live(&def.id, palette_to_wire(&colors), cx);
                    }
                });
            },
        )
    });
    div().relative().child(row).children(plate)
}

fn gradient_widget(
    app: &Entity<Luma>,
    index: usize,
    cell: &Cell,
    gradient: Gradient,
    selected: Option<usize>,
    hsv: Hsv,
    plate_open: bool,
) -> Div {
    let def = cell.def.clone();
    let events = app.clone();
    let gradient_for_events = gradient.clone();
    let bar = luma_gradient_bar(
        def.name.clone(),
        &gradient,
        selected,
        FIELD_W,
        move |event, _, cx| {
            let def = def.clone();
            let mut gradient = gradient_for_events.clone();
            events.update(cx, |this, cx| {
                let write = match event {
                    GradientEvent::Select(at) => {
                        let color = gradient.stops().get(at).map(|stop| stop.color);
                        this.with_track_editor(cx, |editor| {
                            if let Some(color) = color {
                                if let Some(built) = editor.sheet.built.as_mut() {
                                    if let Some(cell) = built.cells.get_mut(index) {
                                        if let Widget::Gradient { selected, hsv } = &mut cell.widget
                                        {
                                            *selected = Some(at);
                                            *hsv = Hsv::from_rgb([color.r, color.g, color.b]);
                                        }
                                    }
                                }
                                editor.sheet.open = Some(Menu::Swatch(index));
                            }
                        });
                        None
                    }
                    GradientEvent::Move { index: stop, t } => {
                        gradient.move_stop(stop, t);
                        Some(gradient)
                    }
                    GradientEvent::Add { t } => {
                        let at = gradient.insert(t);
                        let color = gradient.stops()[at].color;
                        this.with_track_editor(cx, |editor| {
                            if let Some(built) = editor.sheet.built.as_mut() {
                                if let Some(cell) = built.cells.get_mut(index) {
                                    if let Widget::Gradient { selected, hsv } = &mut cell.widget {
                                        *selected = Some(at);
                                        *hsv = Hsv::from_rgb([color.r, color.g, color.b]);
                                    }
                                }
                            }
                        });
                        Some(gradient)
                    }
                };
                if let Some(gradient) = write {
                    this.arg_live(&def.id, gradient_to_wire(&gradient), cx);
                }
            });
        },
    );
    let plate = plate_open.then_some(selected).flatten().map(|at| {
        let def = cell.def.clone();
        let picker_app = app.clone();
        let gradient = gradient.clone();
        let dismiss = app.clone();
        swatch_plate(
            format!("{}:stop-picker", cell.def.name),
            hsv,
            move |_, cx| {
                dismiss.update(cx, |this, cx| {
                    if this.dismiss_sheet_menu() {
                        cx.notify();
                    }
                });
            },
            move |hsv, _, cx| {
                let def = def.clone();
                let mut gradient = gradient.clone();
                picker_app.update(cx, |this, cx| {
                    edit_widget(this, index, cx, |widget| {
                        if let Widget::Gradient { hsv: held, .. } = widget {
                            *held = hsv;
                        }
                    });
                    if at < gradient.stops().len() {
                        let [r, g, b] = hsv.to_rgb();
                        gradient.set_color(at, Rgba { r, g, b, a: 1. });
                        this.arg_live(&def.id, gradient_to_wire(&gradient), cx);
                    }
                });
            },
        )
    });
    div().relative().child(bar).children(plate)
}
