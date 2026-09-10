//! Clip controls in the dedicated editing area, beside the visualizer.
//! The inspector has its own scrolling viewport, independent of timeline height.
//! Selection retargets the existing controls; argument writes retain their
//! debounced history and persistence path.

use luma_lib::models::node_graph::{PatternArgDef, PatternArgType};
use luma_lib::models::selection::{Selection, Subset};
use luma_ui::arg::arg_row;
use luma_ui::arg::color::{luma_hsv_picker, ColorArg, ColorArgEditor, ColorArgEvent, Hsv};
use luma_ui::arg::expression::{ExpressionEvent, GroupExpressionEditor};
use luma_ui::arg::gradient::{Gradient, GradientStop};
use luma_ui::arg::gradient_editor::{GradientChanged, GradientEditor};
use luma_ui::arg::mapping::{MappingChanged, MappingEditor};
use luma_ui::arg::number::{DraftedNumber, NumberEvent};
use luma_ui::arg::palette::{luma_palette_row, PaletteEvent};
use luma_ui::arg::select::luma_arg_select;
use luma_ui::arg::signal::{SignalChanged, SignalEditor};
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
    /// The venue's group names, for the expression editor's autocomplete.
    groups: Groups,
    /// Arg definitions per pattern id, venue-resolved, cached for the life of
    /// the editor — the web store's `patternArgs`.
    defs: HashMap<String, Rc<[PatternArgDef]>>,
    defs_inflight: HashSet<String>,
    /// Controls and readings for the current selection.
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
    pub(super) fn invalidate_defs(&mut self) {
        self.defs.clear();
        self.built = None;
    }
    /// Whether the sheet is up — heading open, not merely still painted. What
    /// `Escape` asks before it decides the key meant "clear the selection".
    pub(crate) fn is_open(&self) -> bool {
        self.built.is_some()
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
    Seed(Entity<DraftedNumber<u64>>),
    Mapping(Entity<MappingEditor>),
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
}

// -- wire codecs --------------------------------------------------------------

fn mapping_value(value: &serde_json::Value) -> Result<luma_patterns::MappingSpec, String> {
    match luma_lib::node_graph::lighting::decode(luma_patterns::ValueType::Mapping, value)? {
        luma_patterns::Value::Mapping(mapping) => Ok(mapping),
        _ => Err("Expected a mapping".into()),
    }
}

fn mapping_widget(
    definition: &PatternArgDef,
    stored: &serde_json::Value,
    window: &mut Window,
    cx: &mut Context<Luma>,
    subscriptions: &mut Vec<Subscription>,
) -> Widget {
    let value = match mapping_value(stored) {
        Ok(value) => value,
        Err(error) => return Widget::Invalid(error),
    };
    let field =
        cx.new(|cx| MappingEditor::new(definition.name.clone(), value, FIELD_W, window, cx));
    let input = definition.id.clone();
    subscriptions.push(
        cx.subscribe(&field, move |this, _, event: &MappingChanged, cx| {
            this.arg_live(
                &input,
                luma_lib::node_graph::lighting::wire_value(&luma_patterns::Value::Mapping(
                    event.0.clone(),
                )),
                cx,
            );
        }),
    );
    Widget::Mapping(field)
}

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
/// default — the same read the web panel makes.
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
    ensure_groups(editor, cx);
    ensure_defs(editor, cx);
    let Some(primary) = primary_clip(editor).map(|clip| clip.id.clone()) else {
        editor.sheet.built = None;
        editor.sheet.open = None;
        return;
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
    resync(editor, window, cx);
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
    if editor.graph_score.is_some() {
        if let Some(defs) = editor.graph_input_defs(&key) {
            editor.sheet.defs.insert(key, defs.into());
        }
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
            let widget = if let Ok(signal) =
                serde_json::from_value::<luma_patterns::Signal>(stored.clone())
            {
                let field =
                    cx.new(|cx| SignalEditor::new(def.name.clone(), signal, FIELD_W, window, cx));
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
                                        this.arg_live(
                                            &arg_id,
                                            serde_json::json!(value.to_string()),
                                            cx,
                                        );
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
                        let entity =
                            cx.new(|_| luma_ui::arg::envelope::EnvelopeEditor::new(points));
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
                            if editor.graph_score.is_some()
                                || defs.iter().any(|input| {
                                    matches!(
                                        input.arg_type,
                                        PatternArgType::Beats
                                            | PatternArgType::Proportion
                                            | PatternArgType::Mapping
                                    )
                                })
                            {
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
                    PatternArgType::Mapping => mapping_widget(def, &stored, window, cx, &mut subs),
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
                        let value = scalar_from_wire(&stored, &def.default_value);
                        let entity = cx.new(|cx| {
                            DraftedNumber::new(
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
                                    1.
                                } else {
                                    1e9
                                },
                                FIELD_W,
                                window,
                                cx,
                            )
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
        if cell.def.arg_type == PatternArgType::Mapping {
            match (&cell.widget, mapping_value(&stored)) {
                (Widget::Mapping(entity), Ok(value)) => {
                    entity.update(cx, |field, cx| field.set_value(value, cx));
                }
                _ => cell.widget = mapping_widget(&cell.def, &stored, window, cx, &mut built._subs),
            }
            cell.synced = stored;
            continue;
        }
        match &mut cell.widget {
            Widget::Mapping(_) | Widget::Invalid(_) => {}
            Widget::Envelope(entity) => {
                let points = envelope_value(&stored, &cell.def.default_value);
                entity.update(cx, |editor, cx| editor.set_value(points, cx));
            }
            Widget::Choice(_) => {}
            Widget::Color(entity) => {
                let value = color_from_wire(&stored, &cell.def.default_value);
                entity.update(cx, |editor, cx| editor.set_value(value, cx));
            }
            Widget::Scalar(entity) => {
                let value = scalar_from_wire(&stored, &cell.def.default_value);
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

pub(super) fn panel(state: &Editor, app: &Entity<Luma>) -> AnyElement {
    let content = match state.sheet.built.as_ref() {
        Some(built) => body(state, built, app),
        None => div()
            .size_full()
            .p(px(16.))
            .flex()
            .flex_col()
            .gap(px(12.))
            .child("Pattern")
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(ladder::muted_foreground())
                    .child("Select a clip to edit its pattern and fixtures."),
            )
            .into_any_element(),
    };
    div()
        .id("clip-inspector")
        .h_full()
        .w(px(luma_ui::sheet::WIDTH))
        .flex_none()
        .overflow_hidden()
        .bg(ladder::background())
        .border_r_1()
        .border_color(ladder::trim())
        .child(content)
        .agent_node(
            Role::Card,
            if state.sheet.built.is_some() {
                "Clip inputs"
            } else {
                "Pattern inspector"
            },
        )
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
                .child(luma_ui::silkscreen(
                    if state.graph_score.is_some()
                        || built
                            .pattern
                            .as_ref()
                            .and_then(|id| state.patterns.iter().find(|p| p.id == id.as_ref()))
                            .is_some_and(|p| p.score_id.is_some())
                    {
                        "PATTERN · THIS SCORE"
                    } else {
                        "PATTERN · LIBRARY"
                    }
                    .to_string(),
                ))
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
                    .children(state.graph_score.as_ref().map(|_| {
                        let app = app.clone();
                        luma_ui::button("Make independent", Enabled::Yes)
                            .id("make-clip-independent")
                            .on_click(move |_, _, cx| {
                                app.update(cx, |this, cx| this.make_clips_independent(cx));
                            })
                            .agent_node(Role::Button, "Make independent")
                            .into_any_element()
                    }))
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
        Some(_) if built.cells.is_empty() => note("No exposed inputs"),
        Some(_) => {
            let typed = built
                .cells
                .iter()
                .any(|cell| cell.def.arg_type == PatternArgType::Mapping);
            if !typed {
                return built
                    .cells
                    .iter()
                    .enumerate()
                    .flat_map(|(index, cell)| arg_rows(state, app, index, cell))
                    .collect();
            }
            let mut rows = Vec::new();
            for (title, ids) in [
                ("Shape", &["width", "shape", "softness"][..]),
                (
                    "Space",
                    &["selection", "mapping", "boundary", "start", "end"][..],
                ),
                (
                    "Timing",
                    &["travel", "repeat", "grid_aligned", "reseed"][..],
                ),
                ("Appearance", &["color", "brightness"][..]),
            ] {
                let cells: Vec<_> = ids
                    .iter()
                    .filter_map(|id| {
                        built
                            .cells
                            .iter()
                            .enumerate()
                            .find(|(_, cell)| cell.def.id == *id)
                    })
                    .collect();
                if cells.is_empty() {
                    continue;
                }
                rows.push(
                    div()
                        .pt(px(8.))
                        .child(luma_ui::silkscreen(title.to_string()))
                        .agent_node(Role::Text, title.to_string())
                        .into_any_element(),
                );
                for (index, cell) in cells {
                    rows.extend(arg_rows(state, app, index, cell));
                }
            }
            for (index, cell) in built.cells.iter().enumerate().filter(|(_, cell)| {
                ![
                    "width",
                    "shape",
                    "softness",
                    "selection",
                    "mapping",
                    "boundary",
                    "start",
                    "end",
                    "travel",
                    "repeat",
                    "grid_aligned",
                    "reseed",
                    "color",
                    "brightness",
                ]
                .contains(&cell.def.id.as_str())
            }) {
                rows.extend(arg_rows(state, app, index, cell));
            }
            rows
        }
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
        Widget::Mapping(entity) => one(div().child(entity.clone())),
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
                state.sheet.open == Some(Menu::Choice(index)),
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
        Widget::Gradient(entity) => one(div().child(entity.clone())),
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
