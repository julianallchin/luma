//! The clip graph editor: the clip's graph as a node-and-wire canvas (see
//! [`canvas`]). Each node is a card with its settings and one row per input;
//! a value input shows its control, a wired input a port with the wire from
//! the node that feeds it.
//!
//! Every edit goes through [`Luma::graph_live`], so a drag is one undo step
//! and one write. The widgets are built once per graph shape (see
//! [`edit::layout`]); a value that moves under them is pushed in, and a shape
//! change builds them again.

use std::collections::{BTreeMap, BTreeSet};

use luma_patterns as p;
use luma_ui::arg::noise;
use luma_ui::arg::strip::{self, CurveStrip, StripChanged, StripValue};
use luma_ui::icons::IconName;
use luma_ui::{icon_button, rpx, Enabled};
use p::clip_graph::{definition, ClipGraph, Input, Kind, Node};

use super::*;

pub(in crate::track_editor) mod canvas;
pub(in crate::track_editor) mod edit;

/// The shipped curve shapes, as the strips and the promotion defaults read
/// them.
pub(in crate::track_editor) mod presets {
    use luma_patterns as p;
    use luma_ui::arg::strip::StripValue;

    pub(crate) fn curve(name: &str) -> Option<p::Envelope> {
        p::presets().curve(name).cloned()
    }

    /// Every shipped shape, in menu order, for a strip's preset chip.
    pub(crate) fn curves() -> Vec<(gpui::SharedString, StripValue)> {
        p::presets()
            .curves
            .iter()
            .map(|preset| {
                (
                    preset.name.clone().into(),
                    StripValue::Number(preset.curve.clone()),
                )
            })
            .collect()
    }

    /// The name of the shipped shape `points` is, if it is one.
    pub(crate) fn curve_name(points: &p::Envelope) -> Option<String> {
        p::presets()
            .curves
            .iter()
            .find(|preset| preset.curve == *points)
            .map(|preset| preset.name.clone())
    }
}

use edit::Ty;

/// A vector's three fields share a row: U, V and Z.
const VECTOR_GAP: f32 = 8.;
/// A vector component shows at most this many decimals, so it fits its
/// third of the row.
const VECTOR_DECIMALS: usize = 3;
const VECTOR_FIELD_W: f32 = (canvas::NODE_FIELD_W - 2. * VECTOR_GAP) / 3.;
/// A closed curve card's picture of its shape.
const PLOT_H: f32 = 56.;

/// The widgets for one graph shape.
pub(super) struct Controls {
    /// The graph the widgets were last pointed at.
    synced: ClipGraph,
    /// By node and [`item_key`]: an input, or one number of a math node's
    /// values.
    fields: BTreeMap<(String, String), Field>,
    /// A preview per noise node.
    noise: BTreeMap<String, Entity<noise::NoisePreview>>,
    _subs: Vec<Subscription>,
}

enum Field {
    Number(Entity<DraftedNumber>),
    /// U, V and Z.
    Vector([Entity<DraftedNumber>; 3]),
    Color(Entity<ColorArgEditor>),
    Strip(Entity<CurveStrip>),
}

// -- values -------------------------------------------------------------------

fn ui_gradient(gradient: &p::Gradient) -> Gradient {
    Gradient::new(gradient.stops.iter().map(|stop| GradientStop {
        t: stop.t as f32,
        color: Light {
            a: stop.alpha as f32,
            ..Light::opaque(stop.color)
        },
    }))
}

fn pattern_gradient(gradient: &Gradient) -> p::Gradient {
    let mut stops: Vec<_> = gradient
        .stops()
        .iter()
        .map(|stop| p::ColorStop {
            t: f64::from(stop.t).clamp(0., 1.),
            color: stop.color.channels().map(|v| v.clamp(0., 1.)),
            alpha: f64::from(stop.color.a).clamp(0., 1.),
        })
        .collect();
    stops.sort_by(|a, b| a.t.total_cmp(&b.t));
    p::Gradient { stops }
}

fn color_arg(rgb: [f64; 3]) -> ColorArg {
    ColorArg::decode(rgb.map(|c| c as f32), 1.)
}

/// A curve's low and high as the strip's scale, with their unit, when both
/// are plain numbers.
fn strip_scale(graph: &ClipGraph, curve: &str) -> ([f64; 2], Option<&'static str>) {
    let bound = |name: &str| match edit::shown(graph, curve, name) {
        Some(Input::Number(v)) => Some(v),
        _ => None,
    };
    let unit = edit::spec(graph, curve, "low").and_then(|spec| spec.unit);
    match (bound("low"), bound("high")) {
        (Some(low), Some(high)) => {
            let scale = edit::scale(unit);
            ([low * scale, high * scale], edit::suffix(unit))
        }
        _ => ([0., 1.], None),
    }
}

/// The node a curve's `x` is wired to, and its kind.
fn coordinate(graph: &ClipGraph, curve: &str) -> Option<(String, Kind)> {
    let x = graph.nodes.get(curve)?.inputs.get("x")?.source()?;
    Some((x.to_owned(), graph.nodes.get(x)?.kind))
}

/// How a link menu names a node: "Curve 1 · Ramp up", "Math 1 · cut × fade".
fn describe(graph: &ClipGraph, id: &str) -> String {
    let label = edit::label(id);
    let Some(node) = graph.nodes.get(id) else {
        return label;
    };
    if node.kind == Kind::Math {
        return format!("{label} · {}", math_summary(graph, id));
    }
    if node.kind != Kind::Curve {
        return label;
    }
    let shape = match node.inputs.get("shape") {
        Some(Input::Points(points)) => presets::curve_name(points),
        None => Some("Ramp up".to_owned()),
        _ => None,
    };
    match shape.or_else(|| coordinate(graph, id).map(|(_, kind)| edit::source_label(kind).into())) {
        Some(detail) => format!("{label} · {detail}"),
        None => label,
    }
}

// -- summaries ------------------------------------------------------------------

/// A math op's sign in a summary and on its button.
fn op_symbol(op: &str) -> &str {
    match op {
        "*" => "×",
        "-" => "−",
        other => other,
    }
}

/// A number as a summary writes it: at most two decimals.
fn short(value: f64) -> String {
    let text = format!("{value:.2}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text == "-0" { "0" } else { text }.to_owned()
}

/// A value or a wire, as a summary names it.
fn item_text(item: &Input) -> String {
    match item {
        Input::Number(v) => short(*v),
        Input::Wire(to) => edit::label(to),
        _ => "…".into(),
    }
}

/// One line on what a math node does: "cut × bloom × fade", "max(a, b)".
fn math_summary(graph: &ClipGraph, id: &str) -> String {
    let Some(node) = graph.nodes.get(id) else {
        return String::new();
    };
    let items: Vec<String> = node
        .inputs
        .get("values")
        .map(|values| match values {
            Input::List(items) => items.iter().map(item_text).collect(),
            one => vec![item_text(one)],
        })
        .unwrap_or_default();
    match node.setting("op").unwrap_or("*") {
        op @ ("*" | "+" | "-") => items.join(&format!(" {} ", op_symbol(op))),
        op => format!("{op}({})", items.join(", ")),
    }
}

/// One line on what a curve does, under its picture: the node its x reads
/// and its bounds, "t · 0→1"; a color curve names its gradient.
fn curve_summary(graph: &ClipGraph, id: &str) -> String {
    let Some(node) = graph.nodes.get(id) else {
        return String::new();
    };
    let mut parts = Vec::new();
    if let Some((x, _)) = coordinate(graph, id) {
        parts.push(edit::label(&x));
    }
    if node.setting("kind") == Some("color") {
        let name = match node.inputs.get("gradient") {
            Some(Input::Gradient(gradient)) => {
                let value = StripValue::Gradient(ui_gradient(gradient));
                strip::gradient_presets()
                    .into_iter()
                    .find(|(_, preset)| *preset == value)
                    .map_or("gradient".to_owned(), |(name, _)| name.to_string())
            }
            _ => "gradient".to_owned(),
        };
        parts.push(name);
        return parts.join(" · ");
    }
    let bound = |name: &str| match node.inputs.get(name) {
        Some(item @ (Input::Number(_) | Input::Wire(_))) => Some(item_text(item)),
        Some(_) => None,
        None => edit::empty(graph, id, name).as_ref().map(item_text),
    };
    if let (Some(low), Some(high)) = (bound("low"), bound("high")) {
        parts.push(format!("{low}→{high}"));
    }
    parts.join(" · ")
}

// -- build and sync ------------------------------------------------------------

/// The field key of item `index` of a math node's values.
fn item_key(input: &str, index: usize) -> String {
    format!("{input}#{index}")
}

/// What a field shows, by its key: the input's value, or a list item's
/// number.
fn field_value(graph: &ClipGraph, id: &str, key: &str) -> Option<Input> {
    let Some((input, index)) = key.split_once('#') else {
        return edit::shown(graph, id, key);
    };
    let Input::List(items) = graph.nodes.get(id)?.inputs.get(input)? else {
        return None;
    };
    items
        .get(index.parse::<usize>().ok()?)
        .filter(|item| matches!(item, Input::Number(_)))
        .cloned()
}

/// The field of number item `index` of a math node's values, which leaves
/// room for the item's remove button.
fn item_field(
    graph: &ClipGraph,
    id: &str,
    input: &str,
    index: usize,
    window: &mut Window,
    cx: &mut Context<Luma>,
    subs: &mut Vec<Subscription>,
) -> Option<Field> {
    let spec = edit::spec(graph, id, input)?;
    let Some(Input::Number(value)) = field_value(graph, id, &item_key(input, index)) else {
        return None;
    };
    let entity = number_field(
        format!("{}: item {}", field_name(id, input), index + 1),
        value,
        spec,
        canvas::NODE_FIELD_W - CONTROL_HEIGHT - VECTOR_GAP,
        luma_ui::arg::number::DECIMALS,
        window,
        cx,
    );
    let scale = edit::scale(spec.unit);
    let (at, name) = (id.to_owned(), input.to_owned());
    subs.push(cx.subscribe(
        &entity,
        move |this: &mut Luma, _, event: &NumberEvent, cx| {
            let NumberEvent::Committed(value) = *event;
            let (at, name) = (at.clone(), name.clone());
            this.graph_live(cx, move |graph| {
                edit::set_item(graph, &at, &name, index, Input::Number(value / scale))
            });
        },
    ));
    Some(Field::Number(entity))
}

fn number_field(
    name: String,
    value: f64,
    spec: edit::Spec,
    width: f32,
    decimals: usize,
    window: &mut Window,
    cx: &mut Context<Luma>,
) -> Entity<DraftedNumber> {
    let scale = edit::scale(spec.unit);
    let [low, high] = spec.range.map(|v| v * scale);
    cx.new(|cx| {
        let field = DraftedNumber::new(
            name,
            value * scale,
            low.min(value * scale),
            high.max(value * scale),
            width,
            window,
            cx,
        )
        .with_decimals(decimals, cx);
        match edit::suffix(spec.unit) {
            Some(unit) => field.with_unit(unit),
            None => field,
        }
    })
}

fn field_name(id: &str, input: &str) -> String {
    format!("{} {}", edit::label(id), input_label(input).to_lowercase())
}

/// Build the widget for one value input.
fn field(
    graph: &ClipGraph,
    id: &str,
    input: &str,
    value: &Input,
    window: &mut Window,
    cx: &mut Context<Luma>,
    subs: &mut Vec<Subscription>,
) -> Option<Field> {
    let spec = edit::spec(graph, id, input)?;
    let (at, name) = (id.to_owned(), input.to_owned());
    let set = move |value: Input| {
        let (at, name) = (at.clone(), name.clone());
        move |graph: &mut ClipGraph| edit::set_input(graph, &at, &name, Some(value.clone()))
    };
    Some(match (spec.ty, value) {
        (Ty::Number, Input::Number(v)) => {
            let entity = number_field(
                field_name(id, input),
                *v,
                spec,
                canvas::NODE_FIELD_W,
                luma_ui::arg::number::DECIMALS,
                window,
                cx,
            );
            let scale = edit::scale(spec.unit);
            subs.push(cx.subscribe(
                &entity,
                move |this: &mut Luma, _, event: &NumberEvent, cx| {
                    let NumberEvent::Committed(value) = *event;
                    this.graph_live(cx, set(Input::Number(value / scale)));
                },
            ));
            Field::Number(entity)
        }
        (Ty::Vector, Input::Vector(v)) => {
            let spec = edit::Spec {
                ty: Ty::Number,
                ..spec
            };
            let entities = [0, 1, 2].map(|axis| {
                let name = format!("{}: {}", field_name(id, input), ["u", "v", "z"][axis]);
                let entity = number_field(
                    name,
                    v[axis],
                    spec,
                    VECTOR_FIELD_W,
                    VECTOR_DECIMALS,
                    window,
                    cx,
                );
                let (at, input) = (id.to_owned(), input.to_owned());
                subs.push(cx.subscribe(
                    &entity,
                    move |this: &mut Luma, _, event: &NumberEvent, cx| {
                        let NumberEvent::Committed(value) = *event;
                        let (at, input) = (at.clone(), input.clone());
                        this.graph_live(cx, move |graph| {
                            let Some(Input::Vector(mut v)) = edit::shown(graph, &at, &input) else {
                                return;
                            };
                            v[axis] = value;
                            edit::set_input(graph, &at, &input, Some(Input::Vector(v)));
                        });
                    },
                ));
                entity
            });
            Field::Vector(entities)
        }
        (Ty::Color, Input::Color(rgb)) => {
            let entity = cx.new(|cx| {
                ColorArgEditor::new(field_name(id, input), color_arg(*rgb), cx).rgb_only()
            });
            subs.push(cx.subscribe(
                &entity,
                move |this: &mut Luma, _, event: &ColorArgEvent, cx| {
                    let ColorArgEvent::Changed(value) = *event;
                    let rgb = value.encode().0.map(f64::from);
                    this.graph_live(cx, set(Input::Color(rgb)));
                },
            ));
            Field::Color(entity)
        }
        (Ty::Points, Input::Points(points)) => {
            let (range, unit) = strip_scale(graph, id);
            let mut strip = CurveStrip::new(edit::label(id), StripValue::Number(points.clone()))
                .with_presets(presets::curves())
                .with_scale(range, unit);
            let app = cx.entity().downgrade();
            let curve = id.to_owned();
            match coordinate(graph, id).map(|(_, kind)| kind) {
                Some(Kind::Time) => {
                    strip = strip.over_time(Rc::new(move |cx| clock_of(&app, &curve, cx)));
                }
                Some(Kind::Space) => {
                    strip = strip.across_space(Rc::new(move |cx| heads_of(&app, &curve, cx)));
                }
                _ => {}
            }
            let entity = cx.new(|_| strip);
            subs.push(cx.subscribe(
                &entity,
                move |this: &mut Luma, _, event: &StripChanged, cx| {
                    if let StripValue::Number(points) = &event.0 {
                        this.graph_live(cx, set(Input::Points(points.clone())));
                    }
                },
            ));
            Field::Strip(entity)
        }
        (Ty::Gradient, Input::Gradient(gradient)) => {
            let label = format!("{} colors", edit::label(id));
            let entity = cx.new(|_| {
                CurveStrip::new(label, StripValue::Gradient(ui_gradient(gradient)))
                    .with_presets(strip::gradient_presets())
            });
            subs.push(cx.subscribe(
                &entity,
                move |this: &mut Luma, _, event: &StripChanged, cx| {
                    if let StripValue::Gradient(gradient) = &event.0 {
                        this.graph_live(cx, set(Input::Gradient(pattern_gradient(gradient))));
                    }
                },
            ));
            Field::Strip(entity)
        }
        _ => return None,
    })
}

/// Build the widgets for `graph`: one per value input, shown or empty with a
/// value to show.
pub(super) fn build(graph: &ClipGraph, window: &mut Window, cx: &mut Context<Luma>) -> Controls {
    let mut subs = Vec::new();
    let mut fields = BTreeMap::new();
    for (id, node) in &graph.nodes {
        for (input, _) in &definition(node.kind).inputs {
            if let Some(Input::List(items)) = node.inputs.get(*input) {
                for index in 0..items.len() {
                    if let Some(field) = item_field(graph, id, input, index, window, cx, &mut subs)
                    {
                        fields.insert((id.clone(), item_key(input, index)), field);
                    }
                }
                continue;
            }
            let Some(value) = edit::shown(graph, id, input) else {
                continue;
            };
            if let Some(field) = field(graph, id, input, &value, window, cx, &mut subs) {
                fields.insert((id.clone(), (*input).to_owned()), field);
            }
        }
    }
    let noise = graph
        .nodes
        .iter()
        .filter(|(_, node)| node.kind == Kind::Noise)
        .map(|(id, node)| (id.clone(), noise_preview(id, node, cx)))
        .collect();
    Controls {
        synced: graph.clone(),
        fields,
        noise,
        _subs: subs,
    }
}

/// Push values that moved under the widgets into them. The shape is the same
/// as when they were built.
pub(super) fn sync(controls: &mut Controls, graph: &ClipGraph, window: &mut Window, cx: &mut App) {
    if controls.synced == *graph {
        return;
    }
    for ((id, key), field) in &controls.fields {
        let before = field_value(&controls.synced, id, key);
        let now = field_value(graph, id, key);
        let rescaled = matches!(field, Field::Strip(_))
            && strip_scale(&controls.synced, id) != strip_scale(graph, id);
        if before == now && !rescaled {
            continue;
        }
        let input = key.split('#').next().unwrap_or(key);
        let scale = edit::spec(graph, id, input).map_or(1., |spec| edit::scale(spec.unit));
        match (field, now) {
            (Field::Number(entity), Some(Input::Number(v))) => {
                entity.update(cx, |field, cx| field.set_value(v * scale, cx));
            }
            (Field::Vector(entities), Some(Input::Vector(v))) => {
                for (entity, value) in entities.iter().zip(v) {
                    entity.update(cx, |field, cx| field.set_value(value * scale, cx));
                }
            }
            (Field::Color(entity), Some(Input::Color(rgb))) => {
                entity.update(cx, |field, cx| field.set_value(color_arg(rgb), cx));
            }
            (Field::Strip(entity), Some(Input::Points(points))) => {
                let (range, unit) = strip_scale(graph, id);
                entity.update(cx, |strip, cx| {
                    strip.set_scale(range, unit, cx);
                    strip.set_value(StripValue::Number(points), cx);
                });
            }
            (Field::Strip(entity), Some(Input::Gradient(gradient))) => {
                entity.update(cx, |strip, cx| {
                    strip.set_value(StripValue::Gradient(ui_gradient(&gradient)), cx)
                });
            }
            _ => {}
        }
    }
    for (id, preview) in &controls.noise {
        if let Some(node) = graph.nodes.get(id) {
            let settings = noise_settings(node);
            preview.update(cx, |preview, cx| preview.set_value(settings, cx));
        }
    }
    let _ = window;
    controls.synced = graph.clone();
}

/// The clock of a curve over time, each frame its strip draws: the beats
/// one event lasts and where the playhead is in it, after the time's delay
/// and phase. Before the delay the playhead holds at 0 and after the event
/// at 1, as the curve holds its ends there. A time with a wired `every`,
/// `duration`, delay or phase has no one clock to draw.
fn clock_of(app: &WeakEntity<Luma>, curve: &str, cx: &App) -> Option<strip::Clock> {
    let app = app.upgrade()?;
    let Some(Body::TrackEditor(editor)) = app.read(cx).workspace.active_body() else {
        return None;
    };
    let clip = primary_clip(editor)?;
    let graph = &clip.core.as_ref()?.graph;
    let (_, length, elapsed) = clip_beats(editor, clip)?;
    let (time, _) = coordinate(graph, curve)?;
    let time = graph.nodes.get(&time)?;
    let number = |name: &str| match time.inputs.get(name) {
        Some(Input::Number(v)) => Some(Some(*v)),
        None => Some(None),
        _ => None,
    };
    let every = number("every")?.filter(|every| *every > 0.);
    let duration = number("duration")?.filter(|duration| *duration > 0.);
    let (delay, phase) = (number("delay")?.unwrap_or(0.), number("phase")?);
    // Empty duration: as long as every, or the clip with no events.
    let life = duration.or(every).unwrap_or(length);
    if life <= 0. {
        return None;
    }
    let age = every.map_or(elapsed, |every| elapsed.rem_euclid(every));
    let inside = (0. ..=length).contains(&elapsed) && age <= life;
    let x = (age / life).clamp(0., 1.) - delay / life;
    let x = match phase {
        Some(phase) if phase != 0. => (x + phase).rem_euclid(1.),
        _ => x.clamp(0., 1.),
    };
    Some(strip::Clock {
        beats: life,
        phase: inside.then_some(x),
        playing: editor.transport.playing,
    })
}

/// The clip's beats: where the playhead is since the clip start, and how
/// long the clip lasts.
fn clip_beats(editor: &Editor, clip: &Clip) -> Option<(f64, f64, f64)> {
    let timeline = editor.beats.as_deref()?.timeline().ok()?;
    let start = timeline.beat_at(clip.start).ok()?;
    let length = timeline.beat_at(clip.end).ok()? - start;
    let now = timeline
        .beat_at(f64::from(editor.transport.position))
        .ok()?;
    Some((start, length, now - start))
}

/// Where the clip's heads fall along a curve's space axis at the playhead,
/// as playback computes it, after the space's shift and scale: one place
/// per head, outside 0–1 where a head is past an end of the curve.
fn heads_of(app: &WeakEntity<Luma>, curve: &str, cx: &App) -> Option<Rc<[f64]>> {
    let app = app.upgrade()?;
    let Some(Body::TrackEditor(editor)) = app.read(cx).workspace.active_body() else {
        return None;
    };
    let clip = primary_clip(editor)?;
    let core = clip.core.as_ref()?;
    let (space, _) = coordinate(&core.graph, curve)?;
    let cells = editor.sheet.heads.cells.clone()?;
    let (start, length, elapsed) = clip_beats(editor, clip)?;
    let places = core
        .graph
        .coordinate_at_heads(
            &space,
            p::Frame {
                cells: &cells,
                features: None,
                beat: start + elapsed.clamp(0., length.next_down().max(0.)),
                clip_start: start,
                clip_duration: length,
                seed: core.seed,
            },
        )
        .ok()?;
    Some(places.into_iter().flatten().collect())
}

/// A noise node's settings as the preview draws them: a wired one is held
/// at its empty value, and the preview says so.
fn noise_settings(node: &Node) -> noise::Settings {
    let mut held = Vec::new();
    let mut number = |input: &str, empty: f64, note: &str| match node.inputs.get(input) {
        Some(Input::Number(v)) => Some(*v),
        Some(_) => {
            held.push(note.to_owned());
            Some(empty)
        }
        None => None,
    };
    let speed = number("speed", 4., "speed at 4 beats").unwrap_or(4.);
    let scale = number("scale", 0.25, "scale at 25 %");
    let contrast = number("contrast", 0., "contrast at 0").unwrap_or(0.);
    noise::Settings {
        speed,
        scale,
        contrast,
        held,
    }
}

/// The noise preview of node `id`, sampled as playback samples it.
fn noise_preview(id: &str, node: &Node, cx: &mut Context<Luma>) -> Entity<noise::NoisePreview> {
    let at = id.to_owned();
    let sampler: noise::Sampler = Rc::new(move |settings, seed, place, beats| {
        p::clip_graph::sample_noise(
            &at,
            seed,
            place,
            beats,
            settings.speed,
            settings.scale,
            settings.contrast,
        )
        .ok()
    });
    let app = cx.entity().downgrade();
    let transport: noise::TransportSource = Rc::new(move |cx| {
        let transport = |cx: &App| {
            let app = app.upgrade()?;
            let Some(Body::TrackEditor(editor)) = app.read(cx).workspace.active_body() else {
                return None;
            };
            let clip = primary_clip(editor)?;
            let (_, length, elapsed) = clip_beats(editor, clip)?;
            Some(noise::Transport {
                seed: clip.core.as_ref()?.seed,
                beat: if (0. ..=length).contains(&elapsed) {
                    elapsed
                } else {
                    0.
                },
                playing: editor.transport.playing,
            })
        };
        transport(cx).unwrap_or_default()
    });
    let settings = noise_settings(node);
    cx.new(|cx| {
        let mut preview = noise::NoisePreview::new(edit::label(id), sampler, transport);
        preview.set_value(settings, cx);
        preview
    })
}

// -- menus --------------------------------------------------------------------

/// The key of an input's menus.
fn menu_key(id: &str, input: &str) -> usize {
    format!("{id}.{input}")
        .bytes()
        .fold(0xcbf29ce484222325usize, |h, b| {
            (h ^ usize::from(b)).wrapping_mul(0x100000001b3)
        })
}

/// The picks a source chip offers, and what its trigger reads.
struct Chip {
    reading: String,
    options: Vec<(String, Pick)>,
}

#[derive(Clone)]
enum Pick {
    /// Back to a value: the last one, or a first one.
    Value,
    /// Empty: best fit, uniform, all heads, once over the clip.
    Empty,
    Promote(Kind),
    /// A new node of this kind (a shaper, a clock) into the input.
    Insert(Kind),
    /// A new coordinate node for a curve's `x`.
    Coordinate(Kind),
    /// Multiply what the input holds by 1, through a math node.
    Multiply,
    /// Open the list of nodes to share.
    Links,
}

fn chip(graph: &ClipGraph, id: &str, input: &str) -> Option<Chip> {
    let spec = edit::spec(graph, id, input)?;
    let held = graph.nodes.get(id)?.inputs.get(input);
    let wired = held.and_then(Input::source).and_then(|to| {
        let node = graph.nodes.get(to)?;
        Some((to.to_owned(), node.kind))
    });
    let links = !edit::link_candidates(graph, id, input).is_empty();
    let mut options: Vec<(String, Pick)> = Vec::new();
    let reading = match spec.ty {
        Ty::Number | Ty::Vector | Ty::Color => {
            let showable = edit::empty(graph, id, input).is_some();
            options.push(("Value".into(), Pick::Value));
            if !showable
                && edit::first_value(graph, id, input).is_some()
                && !is_bound(graph, id, input)
            {
                options.push((
                    edit::empty_note(&graph.nodes[id], input).into(),
                    Pick::Empty,
                ));
            }
            options.extend(
                edit::SOURCES
                    .iter()
                    .map(|kind| (edit::source_label(*kind).into(), Pick::Promote(*kind))),
            );
            if edit::can_multiply(graph, id, input) {
                options.push((Kind::Math.label().into(), Pick::Multiply));
            }
            match (&wired, held) {
                (Some((to, _)), _) => from_name(to),
                (None, None) if !showable => edit::empty_note(&graph.nodes[id], input).into(),
                _ => "Value".into(),
            }
        }
        Ty::Coordinate => {
            options.extend(
                edit::SOURCES
                    .iter()
                    .map(|kind| (edit::source_label(*kind).into(), Pick::Coordinate(*kind))),
            );
            wired.map_or("None".into(), |(to, _)| from_name(&to))
        }
        Ty::Heads => {
            options.push(("All".into(), Pick::Empty));
            options.extend(
                edit::SHAPERS
                    .iter()
                    .map(|kind| (kind.label().into(), Pick::Insert(*kind))),
            );
            wired.map_or("All".into(), |(to, _)| from_name(&to))
        }
        Ty::Time => {
            options.push(("Once".into(), Pick::Empty));
            options.push((Kind::Time.label().into(), Pick::Insert(Kind::Time)));
            wired.map_or("Once".into(), |(to, _)| from_name(&to))
        }
        Ty::Points | Ty::Gradient | Ty::Values => return None,
    };
    if links {
        options.push(("Link…".into(), Pick::Links));
    }
    Some(Chip { reading, options })
}

/// What a wired input's source chip reads: the name of the node that feeds
/// it, "← mix".
fn from_name(to: &str) -> String {
    format!("← {}", edit::label(to))
}

/// A curve's low or high: it has no empty to go back to when the curve is a
/// vector.
fn is_bound(graph: &ClipGraph, id: &str, input: &str) -> bool {
    graph
        .nodes
        .get(id)
        .is_some_and(|node| node.kind == Kind::Curve)
        && matches!(input, "low" | "high")
}

/// What an input goes back to when its wire is taken out: the value it held
/// before, or a first value where empty has none to show.
fn restore(graph: &ClipGraph, id: &str, input: &str, last: Option<&Input>) -> Option<Input> {
    last.cloned().or_else(|| {
        edit::empty(graph, id, input)
            .is_none()
            .then(|| edit::first_value(graph, id, input))
            .flatten()
    })
}

impl Luma {
    fn graph_pick(&mut self, id: &str, input: &str, pick: Pick, cx: &mut Context<Self>) {
        if matches!(pick, Pick::Links) {
            let key = menu_key(id, input);
            self.with_track_editor(cx, |editor| editor.sheet.open = Some(Menu::Link(key)));
            return;
        }
        let key = (id.to_owned(), input.to_owned());
        let last = self
            .track_editor_ref()
            .and_then(|editor| editor.sheet.last.get(&key).cloned());
        let (at, name) = key.clone();
        let mut held = None;
        let held_out = &mut held;
        self.graph_live(cx, |graph| match &pick {
            Pick::Value => {
                let back = restore(graph, &at, &name, last.as_ref());
                let wired = graph.nodes[&at]
                    .inputs
                    .get(&name)
                    .is_some_and(|v| matches!(v, Input::Wire(_)));
                if wired || graph.nodes[&at].inputs.get(&name).is_none() {
                    edit::set_input(graph, &at, &name, back);
                }
            }
            Pick::Empty => edit::set_input(graph, &at, &name, None),
            Pick::Promote(kind) => {
                if let Some(value) = edit::promote(graph, &at, &name, *kind) {
                    *held_out = Some(value);
                }
            }
            Pick::Insert(kind) => edit::insert(graph, &at, &name, *kind),
            Pick::Coordinate(kind) => edit::recoordinate(graph, &at, *kind),
            Pick::Multiply => {
                *held_out = graph.nodes[&at]
                    .inputs
                    .get(&name)
                    .filter(|held| !matches!(held, Input::Wire(_)))
                    .cloned();
                edit::multiply(graph, &at, &name);
            }
            Pick::Links => {}
        });
        if let Some(value) = held {
            self.with_track_editor(cx, |editor| {
                editor.sheet.last.insert(key, value);
            });
        }
    }

    /// Share `target` into an input from the Link… menu, keeping the value
    /// it held for an unwire to give back.
    fn graph_link(&mut self, id: &str, input: &str, target: &str, cx: &mut Context<Self>) {
        let mut held = None;
        let held_out = &mut held;
        self.graph_live(cx, |graph| {
            *held_out = graph
                .nodes
                .get(id)
                .and_then(|node| node.inputs.get(input))
                .filter(|value| !matches!(value, Input::Wire(_) | Input::List(_)))
                .cloned();
            edit::link(graph, id, input, target);
        });
        if let Some(value) = held {
            let key = (id.to_owned(), input.to_owned());
            self.with_track_editor(cx, |editor| {
                editor.sheet.last.insert(key, value);
            });
        }
    }
}

// -- rendering ----------------------------------------------------------------

pub(super) struct Ctx<'a> {
    state: &'a Editor,
    controls: &'a Controls,
    graph: &'a ClipGraph,
    app: &'a Entity<Luma>,
}

/// An input's row label: "Low hz" reads "Low".
fn input_label(input: &str) -> String {
    let words = match input {
        "low_hz" => "low",
        "high_hz" => "high",
        other => other,
    };
    sentence_case(&words.replace('_', " "))
}

/// How a port names its input in words, lower case: "brightness", and a
/// math node's item by its place, "values 2".
fn port_name(input: &str) -> String {
    match input.split_once('#') {
        Some((list, index)) => {
            let place = index.parse::<usize>().map_or(0, |index| index + 1);
            format!("{} {place}", input_label(list).to_lowercase())
        }
        None => input_label(input).to_lowercase(),
    }
}

/// Every wire in the graph: the node it comes from and the port it lands
/// on. A math node's items each land on their own row's port
/// ([`item_key`]).
pub(super) fn links(graph: &ClipGraph) -> Vec<(String, (String, String))> {
    let mut links = Vec::new();
    for (id, node) in &graph.nodes {
        for (input, held) in &node.inputs {
            match held {
                Input::List(items) => {
                    for (index, item) in items.iter().enumerate() {
                        if let Some(from) = item.source() {
                            links.push((from.to_owned(), (id.clone(), item_key(input, index))));
                        }
                    }
                }
                one => {
                    if let Some(from) = one.source() {
                        links.push((from.to_owned(), (id.clone(), input.clone())));
                    }
                }
            }
        }
    }
    links
}

/// Whether a row shows for this node: an aim shows the vector its base uses,
/// a space in order has no direction, only radial and angle have a centre,
/// a curve shows its bounds or its gradient by its kind.
fn visible(node: &Node, input: &str) -> bool {
    match (node.kind, input) {
        (Kind::Aim, "direction") => node.setting("base") == Some("direction"),
        (Kind::Aim, "point") => node.setting("base") != Some("direction"),
        (Kind::Space, "direction") => node.setting("kind") != Some("order"),
        (Kind::Space, "centre") => {
            matches!(node.setting("kind"), Some("radial" | "angle"))
        }
        (Kind::Curve, "low" | "high") => node.setting("kind") != Some("color"),
        (Kind::Curve, "gradient") => node.setting("kind") == Some("color"),
        _ => true,
    }
}

/// The graph editor under the blend row: the clip's graph as a canvas.
/// `wide` is whether the panel is widened, or `None` where it already fills
/// its box.
pub(super) fn editor(
    state: &Editor,
    controls: &Controls,
    graph: &ClipGraph,
    app: &Entity<Luma>,
    wide: Option<bool>,
) -> AnyElement {
    let cx = Ctx {
        state,
        controls,
        graph,
        app,
    };
    canvas::view(&cx, wide)
}

/// A node's settings as segmented controls, and a space's wrap as a
/// switch. A curve's kind follows what it feeds, and a math node's op is
/// its title's button ([`title_buttons`]).
fn settings(cx: &Ctx, id: &str, node: &Node) -> Option<AnyElement> {
    let defs = &definition(node.kind).settings;
    let shown: Vec<_> = defs
        .iter()
        .filter(|(name, _)| !matches!(node.kind, Kind::Curve | Kind::Math) && *name != "wrap")
        .collect();
    if shown.is_empty() {
        return None;
    }
    let mut column = div().w_full().flex().flex_col().gap(rpx(8.));
    for (name, setting) in shown {
        let current = node.setting(name).unwrap_or(setting.default);
        let mut track = luma_ui::float::segmented().w_full();
        for option in setting.options {
            let app = cx.app.clone();
            let (at, name, value) = (id.to_owned(), (*name).to_owned(), (*option).to_owned());
            let key = format!("{id}-{name}-{option}");
            track = track.child(
                luma_ui::float::segment((*option).to_owned(), *option == current, key.clone())
                    .id(SharedString::from(key))
                    .on_click(move |_, _, cx| {
                        let (at, name, value) = (at.clone(), name.clone(), value.clone());
                        app.update(cx, |this, cx| {
                            this.graph_live(cx, move |graph| {
                                edit::set_setting(graph, &at, &name, &value)
                            })
                        });
                    })
                    .agent_node(Role::Button, sentence_case(option)),
            );
        }
        column = column.child(track);
    }
    if node.kind == Kind::Space {
        let on = node.setting("wrap") == Some("yes");
        let app = cx.app.clone();
        let at = id.to_owned();
        column = column.child(
            div()
                .id(SharedString::from(format!("{id}-wrap")))
                .w_full()
                .h(rpx(CONTROL_HEIGHT))
                .flex()
                .flex_row()
                .items_center()
                .cursor_pointer()
                .child(luma_ui::caption("Wrap".to_string()))
                .child(div().flex_1())
                .child(luma_ui::float::switch(if on { 1. } else { 0. }))
                .on_click(move |_, _, cx| {
                    let at = at.clone();
                    app.update(cx, |this, cx| {
                        this.graph_live(cx, move |graph| {
                            let value = if on { "no" } else { "yes" };
                            edit::set_setting(graph, &at, "wrap", value)
                        })
                    });
                })
                .agent_node(Role::Button, if on { "Wrap on" } else { "Wrap off" }),
        );
    }
    Some(column.into_any_element())
}

/// A card's body under its title: its settings, then one row per input; a
/// curve's and a math node's are their own ([`curve_rows`], [`math_rows`]).
pub(super) fn body(cx: &Ctx, id: &str) -> Vec<AnyElement> {
    let node = &cx.graph.nodes[id];
    match node.kind {
        Kind::Curve => curve_rows(cx, id),
        Kind::Math => math_rows(cx, id),
        _ => settings(cx, id, node)
            .into_iter()
            .chain(
                definition(node.kind)
                    .inputs
                    .iter()
                    .filter(|(input, _)| visible(node, input))
                    .map(|(input, _)| row(cx, id, input)),
            )
            .collect(),
    }
}

/// The buttons on a card's title before its delete: a math node's op, and
/// on an open curve its widen and close.
pub(super) fn title_buttons(cx: &Ctx, id: &str) -> Vec<AnyElement> {
    let node = &cx.graph.nodes[id];
    let label = edit::label(id);
    match node.kind {
        Kind::Math => vec![op_button(cx, id, node)],
        Kind::Curve if curve_open(cx, id) => {
            let wide = cx.state.sheet.canvas.wide.contains(id);
            let app = cx.app.clone();
            let at = id.to_owned();
            let widen = icon_button(
                if wide {
                    IconName::Minimize
                } else {
                    IconName::Expand
                },
                Enabled::Yes,
            )
            .id(SharedString::from(format!("widen-{id}")))
            .on_click(move |_, _, cx| {
                let at = at.clone();
                app.update(cx, |this, cx| {
                    this.with_track_editor(cx, |editor| {
                        let wide = &mut editor.sheet.canvas.wide;
                        if !wide.remove(&at) {
                            wide.insert(at);
                        }
                    })
                });
            })
            .agent_node(
                Role::Button,
                format!("{} {label}", if wide { "Narrow" } else { "Widen" }),
            )
            .into_any_element();
            let close = icon_button(IconName::ChevronUp, Enabled::Yes)
                .id(SharedString::from(format!("collapse-{id}")))
                .on_click(toggle_curve(cx.app, id, true))
                .agent_node(Role::Button, format!("Collapse {label}"))
                .into_any_element();
            vec![widen, close]
        }
        _ => Vec::new(),
    }
}

/// Whether curve `id`'s card is open to its full editor: as it was last
/// left, else closed once anything past its x is set: its shape, gradient
/// or a bound.
pub(super) fn curve_open(cx: &Ctx, id: &str) -> bool {
    let node = &cx.graph.nodes[id];
    cx.state
        .sheet
        .canvas
        .expanded
        .get(id)
        .copied()
        .unwrap_or_else(|| node.inputs.keys().all(|input| input == "x"))
}

/// A press that opens curve `id`'s card when `open` is false, and closes
/// it when true.
fn toggle_curve(
    app: &Entity<Luma>,
    id: &str,
    open: bool,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    let app = app.clone();
    let at = id.to_owned();
    move |_, _, cx| {
        let at = at.clone();
        app.update(cx, |this, cx| {
            this.with_track_editor(cx, |editor| {
                editor.sheet.canvas.expanded.insert(at, !open);
            })
        });
    }
}

/// A curve's shape or gradient as its strip edits it.
fn curve_value(graph: &ClipGraph, id: &str) -> StripValue {
    let color = graph.nodes[id].setting("kind") == Some("color");
    match (
        edit::shown(graph, id, "gradient"),
        edit::shown(graph, id, "shape"),
    ) {
        (Some(Input::Gradient(gradient)), _) if color => {
            StripValue::Gradient(ui_gradient(&gradient))
        }
        (_, Some(Input::Points(points))) => StripValue::Number(points),
        _ => StripValue::Number(edit::preset_curve("Ramp up")),
    }
}

/// A curve card's rows. Closed: a picture of its shape, which opens it, a
/// line with what its x reads and its bounds, and a row for each other
/// wired input, so every wire into it lands. Open: every row, the strip
/// among them; a bound left empty waits behind a "+".
fn curve_rows(cx: &Ctx, id: &str) -> Vec<AnyElement> {
    let node = &cx.graph.nodes[id];
    let label = edit::label(id);
    let inputs: Vec<&str> = definition(node.kind)
        .inputs
        .iter()
        .map(|(input, _)| *input)
        .filter(|input| visible(node, input))
        .collect();
    if curve_open(cx, id) {
        let all = cx.state.sheet.canvas.defaults.contains(id);
        let unset =
            |input: &str| matches!(input, "low" | "high") && !node.inputs.contains_key(input);
        let mut rows: Vec<AnyElement> = inputs
            .iter()
            .filter(|input| all || !unset(input))
            .map(|input| row(cx, id, input))
            .collect();
        if !all && inputs.iter().any(|input| unset(input)) {
            let app = cx.app.clone();
            let at = id.to_owned();
            rows.push(
                div()
                    .w_full()
                    .flex()
                    .flex_row()
                    .child(
                        icon_button(IconName::Plus, Enabled::Yes)
                            .id(SharedString::from(format!("defaults-{id}")))
                            .on_click(move |_, _, cx| {
                                let at = at.clone();
                                app.update(cx, |this, cx| {
                                    this.with_track_editor(cx, |editor| {
                                        editor.sheet.canvas.defaults.insert(at);
                                    })
                                });
                            })
                            .agent_node(Role::Button, format!("More {label} inputs")),
                    )
                    .into_any_element(),
            );
        }
        return rows;
    }
    let picture = div()
        .id(SharedString::from(format!("plot-{id}")))
        .w_full()
        .cursor_pointer()
        .child(strip::plot(&curve_value(cx.graph, id), PLOT_H))
        .on_click(toggle_curve(cx.app, id, false))
        .agent_node(Role::Button, format!("Expand {label}"))
        .into_any_element();
    let summary = curve_summary(cx.graph, id);
    let line = div()
        .relative()
        .w_full()
        .h(rpx(CONTROL_HEIGHT))
        .flex()
        .flex_row()
        .items_center()
        .children(canvas::input_port(cx, id, "x").map(canvas::port_slot))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(ladder::muted_foreground())
                .child(summary.clone())
                .agent_node(Role::Text, summary),
        )
        .into_any_element();
    let wired = inputs
        .into_iter()
        .filter(|input| *input != "x")
        .filter(|input| node.inputs.get(*input).and_then(Input::source).is_some())
        .map(|input| row(cx, id, input));
    [picture, line].into_iter().chain(wired).collect()
}

/// A math node's op as one button with its sign, which picks another. A
/// subtraction takes exactly two items, so it is offered only then.
fn op_button(cx: &Ctx, id: &str, node: &Node) -> AnyElement {
    let count = match node.inputs.get("values") {
        Some(Input::List(items)) => items.len(),
        _ => 0,
    };
    let current = node.setting("op").unwrap_or("*");
    let ops: Vec<&'static str> = definition(Kind::Math)
        .setting("op")
        .map_or(&[][..], |setting| setting.options)
        .iter()
        .copied()
        .filter(|op| *op != "-" || count == 2 || current == "-")
        .collect();
    let names: Vec<&str> = ops.iter().map(|op| op_symbol(op)).collect();
    let key = menu_key(id, "op");
    let (toggle, choose) = (cx.app.clone(), cx.app.clone());
    let at = id.to_owned();
    luma_arg_select(
        format!("{id}.op"),
        op_symbol(current),
        &names,
        menu_visibility(cx.state, Menu::Source(key)),
        move |_, cx| {
            toggle.update(cx, |this, cx| {
                this.with_track_editor(cx, |editor| {
                    editor.sheet.open = match editor.sheet.open {
                        Some(Menu::Source(k)) if k == key => None,
                        _ => Some(Menu::Source(key)),
                    };
                });
            });
        },
        move |index, _, cx| {
            let Some(op) = ops.get(index).copied() else {
                return;
            };
            let at = at.clone();
            choose.update(cx, |this, cx| {
                this.with_track_editor(cx, |editor| editor.sheet.open = None);
                this.graph_live(cx, move |graph| edit::set_setting(graph, &at, "op", op));
            });
        },
    )
    .into_any_element()
}

/// A math node's rows: one per item, the name of the node wired there or a
/// number, each with its own port and a button that takes it out; then a
/// "+" that adds one, whose port takes a new wire as one more item. A
/// subtraction has exactly two items and no "+".
fn math_rows(cx: &Ctx, id: &str) -> Vec<AnyElement> {
    let node = &cx.graph.nodes[id];
    let input = "values";
    let items: &[Input] = match node.inputs.get(input) {
        Some(Input::List(items)) => items,
        _ => &[],
    };
    let mut rows: Vec<AnyElement> = items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let key = item_key(input, index);
            let control = match item.source() {
                Some(from) => {
                    let name = from_name(from);
                    div()
                        .h(rpx(CONTROL_HEIGHT))
                        .flex()
                        .items_center()
                        .truncate()
                        .text_color(ladder::foreground())
                        .child(name.clone())
                        .agent_node(Role::Text, name)
                        .into_any_element()
                }
                None => cx
                    .controls
                    .fields
                    .get(&(id.to_owned(), key.clone()))
                    .map_or_else(div, field_element)
                    .into_any_element(),
            };
            let app = cx.app.clone();
            let at = id.to_owned();
            let remove = icon_button(IconName::Minus, Enabled::Yes)
                .id(SharedString::from(format!(
                    "remove-item-{id}-{input}-{index}"
                )))
                .on_click(move |_, _, cx| {
                    let at = at.clone();
                    app.update(cx, |this, cx| {
                        this.graph_live(cx, move |graph| {
                            edit::remove_item(graph, &at, input, index)
                        })
                    });
                })
                .agent_node(
                    Role::Button,
                    format!("Remove {} item {}", field_name(id, input), index + 1),
                );
            div()
                .relative()
                .w_full()
                .min_h(rpx(CONTROL_HEIGHT))
                .flex()
                .flex_row()
                .items_start()
                .gap(rpx(VECTOR_GAP))
                .children(canvas::input_port(cx, id, &key).map(canvas::port_slot))
                .child(div().flex_1().min_w_0().child(control))
                .child(remove)
                .agent_node(Role::Row, format!("Item {}", index + 1))
                .into_any_element()
        })
        .collect();
    if node.setting("op") != Some("-") {
        rows.push(
            div()
                .relative()
                .w_full()
                .h(rpx(CONTROL_HEIGHT))
                .flex()
                .flex_row()
                .items_center()
                .children(canvas::input_port(cx, id, input).map(canvas::port_slot))
                .child(add_item_button(id, input, cx))
                .into_any_element(),
        );
    }
    rows
}

/// One input's row: its port on the card's edge, its label and source chip,
/// then its control while it holds a value. A wired input shows no control:
/// its chip names the node that feeds it, and its wire comes from there.
fn row(cx: &Ctx, id: &str, input: &str) -> AnyElement {
    let node = &cx.graph.nodes[id];
    let label = input_label(input);
    let spec = edit::spec(cx.graph, id, input);
    let mut accessories: Vec<AnyElement> = Vec::new();
    if let Some(port) = canvas::input_port(cx, id, input) {
        accessories.push(canvas::port_slot(port).into_any_element());
    }
    accessories.extend(chip(cx.graph, id, input).map(|chip| source_chip(cx, id, input, chip)));
    let control = match node.inputs.get(input) {
        Some(Input::Wire(_) | Input::List(_)) => None,
        _ => Some(
            match cx.controls.fields.get(&(id.to_owned(), input.to_owned())) {
                Some(field) => field_element(field),
                None => match spec.map(|spec| spec.ty) {
                    Some(Ty::Heads | Ty::Time | Ty::Coordinate | Ty::Values) => {
                        return header_row(&label, accessories)
                    }
                    _ => {
                        let note = edit::empty_note(node, input);
                        div()
                            .child(luma_ui::caption(note.to_string()))
                            .opacity(ladder::DISABLED_OPACITY)
                    }
                },
            },
        ),
    };
    match control {
        Some(control) => sheet_row(&label, accessories, control),
        None => header_row(&label, accessories),
    }
}

/// The button that adds an item, a 1, to a math node's values.
fn add_item_button(id: &str, input: &str, cx: &Ctx) -> AnyElement {
    let app = cx.app.clone();
    let (at, name) = (id.to_owned(), input.to_owned());
    icon_button(IconName::Plus, Enabled::Yes)
        .id(SharedString::from(format!("add-item-{id}-{input}")))
        .on_click(move |_, _, cx| {
            let (at, name) = (at.clone(), name.clone());
            app.update(cx, |this, cx| {
                this.graph_live(cx, move |graph| edit::add_item(graph, &at, &name))
            });
        })
        .agent_node(Role::Button, format!("Add to {}", field_name(id, input)))
        .into_any_element()
}

/// A row that is only its header line: a wired input, or one whose empty
/// needs no words.
fn header_row(label: &str, accessories: Vec<AnyElement>) -> AnyElement {
    let label = sentence_case(label);
    div()
        .relative()
        .w_full()
        .h(rpx(CONTROL_HEIGHT))
        .flex()
        .flex_row()
        .items_center()
        .gap(rpx(6.))
        .child(luma_ui::caption(label.clone()))
        .child(div().flex_1())
        .children(accessories)
        .agent_node(Role::Row, label)
        .into_any_element()
}

fn field_element(field: &Field) -> Div {
    match field {
        Field::Number(entity) => div().child(entity.clone()),
        Field::Vector(parts) => parts.iter().zip(["U", "V", "Z"]).fold(
            div().flex().flex_row().gap(rpx(VECTOR_GAP)),
            |el, (entity, name)| el.child(arg_row(name, entity.clone()).w(rpx(VECTOR_FIELD_W))),
        ),
        Field::Color(entity) => div().child(entity.clone()),
        Field::Strip(entity) => div().child(entity.clone()),
    }
}

fn source_chip(cx: &Ctx, id: &str, input: &str, chip: Chip) -> AnyElement {
    let key = menu_key(id, input);
    let linking = cx.state.sheet.open == Some(Menu::Link(key))
        || matches!(cx.state.sheet.closing, Some((Menu::Link(k), _)) if k == key);
    let (menu, labels, picks): (Menu, Vec<String>, Vec<Option<Pick>>) = if linking {
        let targets = edit::link_candidates(cx.graph, id, input);
        (
            Menu::Link(key),
            targets.iter().map(|to| describe(cx.graph, to)).collect(),
            targets.iter().map(|_| None).collect(),
        )
    } else {
        (
            Menu::Source(key),
            chip.options
                .iter()
                .map(|(label, _)| label.clone())
                .collect(),
            chip.options
                .iter()
                .map(|(_, pick)| Some(pick.clone()))
                .collect(),
        )
    };
    let targets = edit::link_candidates(cx.graph, id, input);
    let names: Vec<&str> = labels.iter().map(String::as_str).collect();
    let toggle = cx.app.clone();
    let choose = cx.app.clone();
    let (at, name) = (id.to_owned(), input.to_owned());
    luma_arg_select(
        format!("{id}.{input} source"),
        &chip.reading,
        &names,
        menu_visibility(cx.state, menu),
        move |_, cx| {
            toggle.update(cx, |this, cx| {
                this.with_track_editor(cx, |editor| {
                    editor.sheet.open = match editor.sheet.open {
                        Some(Menu::Source(k) | Menu::Link(k)) if k == key => None,
                        _ => Some(Menu::Source(key)),
                    };
                });
            });
        },
        move |index, _, cx| {
            let (at, name) = (at.clone(), name.clone());
            let pick = picks.get(index).cloned().flatten();
            let target = targets.get(index).cloned();
            choose.update(cx, |this, cx| {
                this.with_track_editor(cx, |editor| editor.sheet.open = None);
                match pick {
                    Some(pick) => this.graph_pick(&at, &name, pick, cx),
                    None => {
                        if let Some(target) = target {
                            this.graph_link(&at, &name, &target, cx);
                        }
                    }
                }
            });
        },
    )
    .into_any_element()
}
