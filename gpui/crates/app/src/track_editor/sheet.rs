//! Clip controls in the dedicated editing area, beside the visualizer.
//! The inspector is always open and has its own scrolling viewport,
//! independent of timeline height. With a clip selected it shows the clip's
//! controls; with none it shows the preset browser. Selection retargets the
//! existing controls; argument writes retain their debounced history and
//! persistence path.

use luma_lib::models::node_graph::{PatternArgDef, PatternArgType};
use luma_lib::models::selection::Selection;
use luma_ui::arg::arg_row;
use luma_ui::arg::color::{luma_hsv_picker, ColorArg, ColorArgEditor, ColorArgEvent, Hsv};
use luma_ui::arg::expression::{ExpressionEvent, GroupExpressionEditor};
use luma_ui::arg::gradient::{Gradient, GradientStop};
use luma_ui::arg::gradient_editor::{GradientChanged, GradientEditor};
use luma_ui::arg::number::{DraftedNumber, NumberEvent};
use luma_ui::arg::palette::{luma_palette_row, PaletteEvent};
use luma_ui::arg::select::{luma_arg_select, MenuVisibility};
use luma_ui::arg::signal::{SignalChanged, SignalEditor};
use luma_ui::CONTROL_HEIGHT;

use super::*;

mod browser;
mod form;

pub(crate) use browser::Audition;
pub(super) use browser::{DropGhost, PresetDrag};

/// Air between one arg row and the next, and between the sheet's bands.
const ROW_GAP: f32 = 14.;

/// A full-bleed control's width inside the sheet.
const FIELD_W: f32 = luma_ui::sheet::CONTENT_WIDTH;

/// The expression field, which shares its row with the fixture-picker chip.
const EXPR_W: f32 = FIELD_W - 62.;

/// The trailing edge a burst of live arg edits is committed on.
const ARG_FLUSH: Duration = Duration::from_millis(250);

// -- state --------------------------------------------------------------------

/// The sheet's own state, owned by the [`Editor`].
pub(crate) struct State {
    /// The venue's group names, for the expression editor's autocomplete.
    groups: Groups,
    /// Arg definitions per pattern id, read off the score's library, cached
    /// for the life of the editor.
    defs: HashMap<String, Rc<[PatternArgDef]>>,
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
            groups: Groups::NotAsked,
            defs: HashMap::new(),
            built: None,
            browser: browser::State::default(),
            open: None,
            was_open: None,
            closing: None,
            burst: false,
            flush_gen: 0,
        }
    }
}

impl State {
    pub(super) fn invalidate_defs(&mut self) {
        self.defs.clear();
        self.built = None;
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
    Choice(usize),
    Blend,
    /// The HSV plate for the selected swatch/stop of the cell at this index.
    Swatch(usize),
    /// The plain-or-source menu of the form input at this index.
    Source(usize),
    /// The spans menu of the axis at this index.
    Span(usize),
    /// The plane menu of the axis at this index.
    Plane(usize),
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
    /// Set for an input of a form clip.
    form: Option<form::Slot>,
}

enum Widget {
    Seed(Entity<DraftedNumber<u64>>),
    Invalid(String),
    Envelope(Entity<luma_ui::arg::envelope::EnvelopeEditor>),
    Choice(Vec<luma_lib::models::node_graph::ParamOption>),
    Color(Entity<ColorArgEditor>),
    Scalar(Entity<DraftedNumber>),
    Signal(Entity<SignalEditor>),
    Selection(Entity<GroupExpressionEditor>),
    /// Stateless kit rows keep their selection (and the picker's working HSV)
    /// here, on the host — the kit's contract.
    Palette {
        selected: Option<usize>,
        hsv: Hsv,
    },
    Gradient(Entity<GradientEditor>),
    /// A form input's named choices. The row reads the stored value. A
    /// choice of curves also holds the editor for a custom curve.
    Preset(
        &'static [luma_patterns::Preset],
        Option<Entity<luma_ui::arg::envelope::EnvelopeEditor>>,
    ),
    /// Sparkle's grain: a head, a fixture, or a clump; holds the clump size.
    Grain(Entity<DraftedNumber>),
    /// `every` of a color over time: once, or a period in beats.
    Every(Entity<DraftedNumber>),
    /// A noise source: speed, then the low and high of its range.
    Noise([Entity<DraftedNumber>; 3]),
    /// An audio source: from and to in Hz, then the floor in percent.
    Audio([Entity<DraftedNumber>; 4]),
    /// A form's axis: its presets, spans and plane; holds the custom plane
    /// axis U, V, Z.
    Axis([Entity<DraftedNumber>; 3]),
}

// -- wire codecs --------------------------------------------------------------

//
// The sheet's serialization edge: everything below speaks the widget kit's
// typed values, everything above speaks the args JSON the score stores:
// colors as 0–255 rgb with the tri-mode alpha, palettes as hex lists,
// gradients as (color, t) stops.

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

/// What a scalar field multiplies the stored value by to show it: a
/// proportion shows as a percent.
fn shown_scale(arg_type: &PatternArgType) -> f64 {
    if *arg_type == PatternArgType::Proportion {
        100.
    } else {
        1.
    }
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

/// The palette fallback, for an arg with no value and no default.
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
        let parsed: Option<Vec<GradientStop>> = list
            .iter()
            .map(|stop| {
                Some(GradientStop {
                    t: stop.get("t")?.as_f64()? as f32,
                    color: {
                        let color = stop.get("color")?;
                        let mut color = if let Some(hex) = color.as_str() {
                            hex_to_rgba(hex)?
                        } else {
                            let c = color.as_array()?;
                            Rgba {
                                r: c.first()?.as_f64()? as f32,
                                g: c.get(1)?.as_f64()? as f32,
                                b: c.get(2)?.as_f64()? as f32,
                                a: 1.0,
                            }
                        };
                        if let Some(alpha) = stop.get("alpha").and_then(serde_json::Value::as_f64) {
                            color.a = alpha as f32;
                        }
                        color
                    },
                })
            })
            .collect();
        parsed
    };
    Gradient::new(stops(value).or_else(|| stops(fallback)).unwrap_or_default())
}

fn gradient_to_wire(gradient: &Gradient) -> serde_json::Value {
    serde_json::json!({
        "stops": gradient
            .stops()
            .iter()
            .map(|stop| serde_json::json!({ "color": rgba_to_hex(stop.color), "t": f64::from(stop.t), "alpha": f64::from(stop.color.a) }))
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
/// default.
fn stored_arg(editor: &Editor, def: &PatternArgDef) -> serde_json::Value {
    primary_clip(editor)
        .and_then(|clip| {
            if def.id == document::SELECTION_INPUT {
                clip.core.as_ref().map(|clip| clip.selection.to_value())
            } else {
                clip.args.get(&def.id).cloned()
            }
        })
        .unwrap_or_else(|| def.default_value.clone())
}

// -- sync ---------------------------------------------------------------------

/// Refresh controls from the selected clip without overwriting active drafts.
pub(super) fn sync(editor: &mut Editor, window: &mut Window, cx: &mut Context<Luma>) {
    tick_menus(&mut editor.sheet, window, cx);
    ensure_groups(editor, cx);
    ensure_defs(editor);
    let Some(primary) = primary_clip(editor).map(|clip| clip.id.clone()) else {
        editor.sheet.open = None;
        editor.sheet.built = None;
        browser::sync(editor, cx);
        return;
    };
    browser::leave(editor);
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

/// Ask for the selected pattern's arg defs, once per pattern.
fn ensure_defs(editor: &mut Editor) {
    let Some(pattern) = shared_pattern(editor) else {
        return;
    };
    let key = pattern.to_string();
    if editor.sheet.defs.contains_key(&key) {
        return;
    }
    if let Some(defs) = editor.graph_input_defs(&key) {
        // A form clip's alpha is edited on the timeline, as its alpha line.
        let form = luma_patterns::is_form(&key);
        let defs: Rc<[PatternArgDef]> = defs
            .into_iter()
            .filter(|def| !form || def.id != super::fades::ALPHA)
            .collect();
        editor.sheet.defs.insert(key, defs);
    }
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

    let rgb_only = editor.graph_score.is_some()
        || defs.iter().any(|input| {
            matches!(
                input.arg_type,
                PatternArgType::Beats | PatternArgType::Proportion | PatternArgType::Mapping
            )
        });
    let form = pattern.as_deref().filter(|id| luma_patterns::is_form(id));
    let cells = defs
        .iter()
        .map(|def| {
            let stored = stored_arg(editor, def);
            let slot = form.and_then(|form| form::Slot::new(form, def, &stored));
            let widget = match &slot {
                Some(slot) => form::widget(slot, def, &stored, window, cx, &mut subs),
                None => plain_widget(def, &stored, rgb_only, &groups, window, cx, &mut subs),
            };
            Cell {
                def: def.clone(),
                synced: stored,
                widget,
                form: slot,
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

/// The control for one arg's plain value.
fn plain_widget(
    def: &PatternArgDef,
    stored: &serde_json::Value,
    rgb_only: bool,
    groups: &[SharedString],
    window: &mut Window,
    cx: &mut Context<Luma>,
    subs: &mut Vec<Subscription>,
) -> Widget {
    let stored = stored.clone();
    if let Ok(signal) = serde_json::from_value::<luma_patterns::Signal>(stored.clone()) {
        let field = cx.new(|cx| SignalEditor::new(def.name.clone(), signal, FIELD_W, window, cx));
        let arg_id = def.id.clone();
        subs.push(cx.subscribe(
            &field,
            move |this: &mut Luma, _, event: &SignalChanged, cx| {
                this.arg_live(
                    &arg_id,
                    serde_json::to_value(&event.0).expect("serializable signal"),
                    cx,
                );
            },
        ));
        Widget::Signal(field)
    } else {
        match def.arg_type {
            PatternArgType::Seed => {
                match luma_lib::node_graph::lighting::decode(
                    luma_patterns::ValueType::Seed,
                    &stored,
                ) {
                    Ok(luma_patterns::Value::Seed(seed)) => {
                        let field = cx.new(|cx| {
                            DraftedNumber::new(
                                def.name.clone(),
                                seed,
                                0,
                                u64::MAX,
                                FIELD_W,
                                window,
                                cx,
                            )
                        });
                        let arg_id = def.id.clone();
                        subs.push(cx.subscribe(
                            &field,
                            move |this: &mut Luma, _, event: &NumberEvent<u64>, cx| {
                                let NumberEvent::Committed(value) = *event;
                                this.arg_live(&arg_id, serde_json::json!(value.to_string()), cx);
                            },
                        ));
                        Widget::Seed(field)
                    }
                    Err(error) => Widget::Invalid(error),
                    _ => unreachable!("seed decoder"),
                }
            }
            PatternArgType::Envelope => {
                let points = envelope_value(&stored, &def.default_value);
                let entity = cx.new(|_| luma_ui::arg::envelope::EnvelopeEditor::new(points));
                let arg_id = def.id.clone();
                subs.push(cx.subscribe(
                    &entity,
                    move |this: &mut Luma,
                          _,
                          event: &luma_ui::arg::envelope::EnvelopeChanged,
                          cx| {
                        this.arg_live(
                            &arg_id,
                            serde_json::to_value(&event.0).expect("validated envelope"),
                            cx,
                        );
                    },
                ));
                Widget::Envelope(entity)
            }
            PatternArgType::Color => {
                let value = color_from_wire(&stored, &def.default_value);
                let entity = cx.new(|cx| {
                    let control = ColorArgEditor::new(def.name.clone(), value, cx);
                    if rgb_only {
                        control.rgb_only()
                    } else {
                        control
                    }
                });
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
            // A form's axis is its own widget; a mapping here did not decode.
            PatternArgType::Mapping => Widget::Invalid("Unreadable axis".into()),
            PatternArgType::Boundary
            | PatternArgType::Boolean
            | PatternArgType::AudioSource
            | PatternArgType::Drum => {
                Widget::Choice(luma_lib::node_graph::lighting::arg_choices(&def.arg_type))
            }
            PatternArgType::Scalar
            | PatternArgType::Beats
            | PatternArgType::Proportion
            | PatternArgType::Position => {
                // A proportion reads as a percent; beats say so.
                let scale = shown_scale(&def.arg_type);
                let value = scalar_from_wire(&stored, &def.default_value) * scale;
                let entity = cx.new(|cx| {
                    let field = DraftedNumber::new(
                        def.name.clone(),
                        value,
                        if matches!(
                            def.arg_type,
                            PatternArgType::Proportion | PatternArgType::Beats
                        ) {
                            0.
                        } else {
                            -1e9
                        },
                        if def.arg_type == PatternArgType::Proportion {
                            100.
                        } else {
                            1e9
                        },
                        FIELD_W,
                        window,
                        cx,
                    );
                    match def.arg_type {
                        PatternArgType::Proportion => field.with_unit("%"),
                        PatternArgType::Beats => field.with_unit("beats"),
                        _ => field,
                    }
                });
                let arg_id = def.id.clone();
                subs.push(cx.subscribe(
                    &entity,
                    move |this: &mut Luma, _, event: &NumberEvent, cx| {
                        let NumberEvent::Committed(value) = *event;
                        this.arg_live(&arg_id, serde_json::json!(value / scale), cx);
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
            PatternArgType::Gradient => {
                let value = gradient_from_wire(&stored, &def.default_value);
                let entity = cx.new(|cx| GradientEditor::new(value, window, cx));
                let arg_id = def.id.clone();
                subs.push(cx.subscribe(
                    &entity,
                    move |this: &mut Luma, _, event: &GradientChanged, cx| {
                        this.arg_live(&arg_id, gradient_to_wire(&event.0), cx);
                    },
                ));
                Widget::Gradient(entity)
            }
        }
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
fn resync(editor: &mut Editor, window: &mut Window, cx: &mut Context<Luma>) {
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
        if let Some(slot) = cell.form.as_mut() {
            if form::resync(
                slot,
                &cell.def,
                &mut cell.widget,
                &stored,
                window,
                cx,
                &mut built._subs,
            ) {
                cell.synced = stored;
                continue;
            }
        }
        match &mut cell.widget {
            Widget::Invalid(_) => {}
            Widget::Envelope(entity) => {
                let points = envelope_value(&stored, &cell.def.default_value);
                entity.update(cx, |editor, cx| editor.set_value(points, cx));
            }
            Widget::Choice(_)
            | Widget::Preset(..)
            | Widget::Grain(_)
            | Widget::Every(_)
            | Widget::Noise(_)
            | Widget::Audio(_)
            | Widget::Axis(_) => {}
            Widget::Color(entity) => {
                let value = color_from_wire(&stored, &cell.def.default_value);
                entity.update(cx, |editor, cx| editor.set_value(value, cx));
            }
            Widget::Scalar(entity) => {
                let value = scalar_from_wire(&stored, &cell.def.default_value)
                    * shown_scale(&cell.def.arg_type);
                entity.update(cx, |field, cx| field.set_value(value, cx));
            }
            Widget::Seed(entity) => {
                if let Ok(luma_patterns::Value::Seed(seed)) =
                    luma_lib::node_graph::lighting::decode(luma_patterns::ValueType::Seed, &stored)
                {
                    entity.update(cx, |field, cx| field.set_value(seed, cx));
                }
            }
            Widget::Signal(entity) => {
                if let Ok(value) = serde_json::from_value::<luma_patterns::Signal>(stored.clone()) {
                    entity.update(cx, |field, cx| field.set_value(value, window, cx));
                }
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
            Widget::Gradient(entity) => {
                let value = gradient_from_wire(&stored, &cell.def.default_value);
                entity.update(cx, |editor, cx| editor.set_value(value, cx));
            }
        }
        cell.synced = stored;
    }
}

// -- the write paths ----------------------------------------------------------

impl Luma {
    /// A blend pick from the sheet: every selected clip takes the mode, in
    /// one committed write.
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
    /// one write.
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
                if arg_id == document::SELECTION_INPUT {
                    let Some(core) = clip.core.as_mut() else {
                        continue;
                    };
                    let selection = selection_from_wire(&value);
                    if let Err(error) = selection.validate() {
                        editor.error = Some(error.to_string());
                        continue;
                    }
                    core.selection = selection;
                    touched.push(clip.id.clone());
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
    /// ride the fast path. The cell's controls — the expression field and
    /// the fixture picker — commit independently, and each writes the whole
    /// value back.
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

// -- rendering ----------------------------------------------------------------

/// The inspector: the selected clip's controls, or the preset browser. It
/// is always open at its full width.
pub(super) fn panel(state: &Editor, app: &Entity<Luma>) -> AnyElement {
    let (label, body) = match state.sheet.built.as_ref() {
        Some(built) => ("Clip inputs", body(state, built, app)),
        None => ("Presets", browser::body(state, app)),
    };
    let width = px(luma_ui::sheet::WIDTH);
    let content = div()
        .id("clip-inspector")
        .size_full()
        .overflow_hidden()
        .bg(ladder::background())
        .border_r_1()
        .border_color(ladder::trim())
        .child(body)
        .into_any_element();
    luma_ui::pane::pane(width, width, content)
        .agent_node(Role::Card, label)
        .into_any_element()
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
                .child(luma_ui::caption("Form".to_string()))
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
                    .child(named("Blend", blend_select(state, built, app)))
                    .children(args(state, built, app)),
            ),
        )
        .into_any_element()
}

/// The pattern's own schema, or the one line that says why there is none.
fn args(state: &Editor, built: &Built, app: &Entity<Luma>) -> Vec<AnyElement> {
    let note = |message: &str| {
        vec![div()
            .child(luma_ui::caption(message.to_string()))
            .opacity(ladder::DISABLED_OPACITY)
            .agent_node(Role::Text, message.to_string())
            .into_any_element()]
    };
    match &built.pattern {
        None => note("Mixed patterns"),
        Some(_) if built.cells.is_empty() => note("No exposed inputs"),
        Some(_) => form::rows(state, built, app),
    }
}

/// The blend row: the canonical nine, from [`BlendMode::ALL`] and nowhere
/// else, applied to the whole selection on pick.
fn blend_select(state: &Editor, built: &Built, app: &Entity<Luma>) -> Div {
    let names: Vec<&str> = BlendMode::ALL.iter().map(|mode| mode.name()).collect();
    let open = menu_visibility(state, Menu::Blend);
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

/// One arg's row.
fn arg_rows(state: &Editor, app: &Entity<Luma>, index: usize, cell: &Cell) -> Vec<AnyElement> {
    let name = cell.def.name.as_str();
    let one = |control: Div| vec![named(name, control)];
    match &cell.widget {
        Widget::Invalid(error) => one(div().text_color(ladder::danger()).child(error.clone())),
        Widget::Choice(options) => {
            let labels: Vec<&str> = options.iter().map(|option| option.label.as_str()).collect();
            let stored = cell
                .synced
                .pointer("/source/kind")
                .unwrap_or(&cell.synced)
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| cell.synced.to_string());
            let selected = options
                .iter()
                .find(|option| option.id == stored)
                .map(|option| option.label.as_str())
                .unwrap_or("Choose…");
            let toggle = app.clone();
            let pick = app.clone();
            let options = options.clone();
            let id = cell.def.id.clone();
            one(luma_arg_select(
                name,
                selected,
                &labels,
                menu_visibility(state, Menu::Choice(index)),
                move |_, cx| {
                    toggle.update(cx, |this, cx| {
                        this.with_track_editor(cx, |editor| {
                            editor.sheet.open = if editor.sheet.open == Some(Menu::Choice(index)) {
                                None
                            } else {
                                Some(Menu::Choice(index))
                            };
                        });
                    });
                },
                move |selected, _, cx| {
                    pick.update(cx, |this, cx| {
                        this.with_track_editor(cx, |editor| editor.sheet.open = None);
                        let value = serde_json::json!(options[selected].id);
                        this.arg_live(&id, value, cx);
                    });
                },
            ))
        }
        Widget::Envelope(entity) => one(div().child(entity.clone())),
        Widget::Color(entity) => one(div().child(entity.clone())),
        Widget::Scalar(entity) => one(div().child(entity.clone())),
        Widget::Seed(entity) => one(div().child(entity.clone())),
        Widget::Signal(entity) => one(div().child(entity.clone())),
        Widget::Selection(entity) => {
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

            one(div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.))
                .child(entity.clone())
                .child(pick_chip))
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
        Widget::Gradient(entity) => one(div().child(entity.clone())),
        // Form rows draw these themselves.
        Widget::Preset(..)
        | Widget::Grain(_)
        | Widget::Every(_)
        | Widget::Noise(_)
        | Widget::Audio(_)
        | Widget::Axis(_) => Vec::new(),
    }
}

/// The width of a row's mode menu ("Fixed", "↗ Over time"…), the same on
/// every row.
const MODE_W: f32 = 140.;
/// Air between a row's header line and its control.
const LABEL_GAP: f32 = 6.;

/// One labelled row, named for the agent tree.
fn named(label: &str, control: Div) -> AnyElement {
    sheet_row(label, Vec::new(), control)
}

/// Every row of the sheet has one shape. A header line one control tall
/// carries the label, sentence case, and on its right anything that changes
/// how the value is read (a mode menu, a toggle). Under it, the value control
/// spans the column, whatever kind it is.
fn sheet_row(label: &str, accessories: Vec<AnyElement>, control: Div) -> AnyElement {
    let label = sentence_case(label);
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(px(LABEL_GAP))
        .child(
            div()
                .w_full()
                .h(px(CONTROL_HEIGHT))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.))
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

fn envelope_value(
    value: &serde_json::Value,
    default: &serde_json::Value,
) -> luma_patterns::Envelope {
    [value, default]
        .into_iter()
        .find_map(|value| {
            serde_json::from_value::<luma_patterns::Envelope>(value.clone())
                .ok()
                .filter(|e| e.validate().is_ok())
        })
        .unwrap_or_else(|| luma_patterns::Envelope::soft_edges(0.))
}
