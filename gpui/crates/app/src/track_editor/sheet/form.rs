//! Form clip inputs. A form clip names a shipped form and holds a value for
//! each of its inputs. The sheet shows them in the form's order, under the
//! engine's names. Where the form allows it, an input can hold a source in
//! place of a plain value: a curve over the clip or over each hit, noise, the
//! level of a frequency range of the mix, or values along an axis of the
//! heads, still or moving.

use super::*;
use luma_lib::node_graph::lighting::decode;
use luma_patterns as p;
use luma_ui::arg::number::format_value;
use luma_ui::arg::preset_picker::{luma_preset_picker, Thumb};
use luma_ui::arg::strip::{self, Clock, ClockSource, CurveStrip, HeadSource, StripValue};

/// Inputs in beats that must stay above zero.
const SPEEDS: [&str; 4] = ["every", "travel", "duration", "speed"];
/// The least beats a speed input takes.
const MIN_BEATS: f64 = 1. / 32.;
/// The top of a speed curve's value axis, in beats.
const CURVE_BEATS: f64 = 8.;
/// Beats a new noise source takes to wander once.
const NOISE_SPEED: f64 = 4.;
/// The grain choices. A clump holds N heads; N is its own field.
const GRAINS: [&str; 3] = ["Head", "Fixture", "Clump"];
/// The two readings of a period that can be 0: once over the clip, or beats.
const EVERY: [&str; 2] = ["Once", "Beats"];
/// The period a switch from "Once" to "Beats" starts at.
const EVERY_BEATS: f64 = 4.;
/// How far from the venue's origin a point field reaches, in metres.
const POINT_REACH: f64 = 100.;

/// A form input's row: what the form says about the input, and which source
/// the stored value holds.
#[derive(Clone, Copy)]
pub(super) struct Slot {
    form: &'static str,
    key: &'static str,
    spec: &'static p::Input,
    speed: bool,
    mode: Option<p::SourceKind>,
    /// Custom was chosen in the row's preset picker: its editor shows even
    /// while the value matches a preset.
    editing: bool,
}

impl Slot {
    /// The slot for `def` when `form` is a shipped form that has it.
    pub(super) fn new(form: &str, def: &PatternArgDef, stored: &serde_json::Value) -> Option<Self> {
        let form = *p::FORMS.iter().find(|id| **id == form)?;
        let (key, spec) = document::form_definition(form)?
            .inputs
            .get_key_value(&def.id)?;
        Some(Self {
            form,
            key,
            spec,
            speed: SPEEDS.contains(&def.id.as_str()),
            mode: decode(spec.value_type, stored)
                .ok()
                .and_then(|value| value.source_kind()),
            editing: false,
        })
    }

    /// A period where the sheet offers 0 beats, once over the clip: the
    /// hits of a color.
    fn once(&self) -> bool {
        self.form == "color@1" && self.key == "every"
    }

    /// The bounds the form gives a number input, such as a chase width.
    fn bounds(&self) -> Option<[f64; 2]> {
        match self.spec.author {
            Some(p::Author::Number {
                min: Some(min),
                max: Some(max),
            }) => Some([min, max]),
            _ => None,
        }
    }

    /// The span a number curve's value axis covers, in the input's unit. A
    /// vector's noise wanders on each of U, V and Z within −1 to 1.
    fn range(&self) -> [f64; 2] {
        if self.speed {
            [0., CURVE_BEATS]
        } else if self.vector() {
            [-1., 1.]
        } else {
            self.bounds().unwrap_or([0., 1.])
        }
    }

    /// A U, V, Z input: an aim direction or a point.
    fn vector(&self) -> bool {
        matches!(self.spec.default, Some(p::Value::Vector(_)))
    }

    /// The unit a number field of this input shows: beats for a speed,
    /// degrees for an aim's fan, size and spread.
    fn unit(&self) -> Option<&'static str> {
        if self.speed {
            Some("beats")
        } else if self.form == "aim@1" && matches!(self.key, "fan" | "size" | "spread") {
            Some("°")
        } else {
            None
        }
    }

    /// `low–high` in the input's unit, for a caption.
    fn span_text(&self, [low, high]: [f64; 2]) -> String {
        let unit = match self.unit() {
            Some("°") => "°".to_string(),
            Some(unit) => format!(" {unit}"),
            None => String::new(),
        };
        format!("{}–{}{unit}", format_value(low), format_value(high))
    }

    /// The shipped curve presets this input offers.
    fn curves(&self) -> impl Iterator<Item = &'static p::CurvePreset> {
        p::presets().curves_for(self.key)
    }

    /// A number the input accepts: speeds stay above zero, the rest within
    /// the input's bounds, 0–1 by default.
    fn fit(&self, value: f64) -> f64 {
        if self.speed {
            value.max(MIN_BEATS)
        } else {
            let [low, high] = self.range();
            value.clamp(low, high)
        }
    }

    /// A plain number in the input's own unit.
    fn plain(&self, value: f64) -> p::Value {
        match self.spec.default {
            Some(p::Value::Beats(_)) => p::Value::Beats(value),
            Some(p::Value::Proportion(_)) => p::Value::Proportion(value),
            _ => p::Value::Number(value),
        }
    }
}

/// What the promote menu calls each source.
fn source_label(kind: p::SourceKind) -> &'static str {
    match kind {
        p::SourceKind::Time => "↗ Over time",
        p::SourceKind::Hit => "↗ Per hit",
        p::SourceKind::Noise => "↗ Noise",
        p::SourceKind::Audio => "↗ Audio",
        p::SourceKind::Space => "↗ Across space",
    }
}

/// What the promote menu calls a value that is not a source.
const FIXED: &str = "Fixed";

fn level(value: &p::Value) -> Option<f64> {
    match value {
        p::Value::Beats(v) | p::Value::Proportion(v) | p::Value::Number(v) => Some(*v),
        _ => None,
    }
}

/// The color a color source starts at.
fn start_color(curve: &p::SourceCurve) -> [f64; 3] {
    match curve {
        p::SourceCurve::Keys(keys) => keys.sample(0.),
        p::SourceCurve::Gradient(read) => read.gradient.sample(read.curve.sample(0.)),
    }
}

/// The number a number value starts at: a plain number, or the start of a
/// curve.
fn start_number(value: &p::Value) -> Option<f64> {
    match value {
        p::Value::Time(p::SourceCurve::Keys(curve))
        | p::Value::Hit(p::SourceCurve::Keys(curve)) => Some(curve.sample(0.)[0]),
        other => level(other),
    }
}

/// An axis along stage X over the whole selection: where a new space source
/// starts.
fn axis_x() -> p::MappingSpec {
    match p::axis_presets()
        .into_iter()
        .find(|(name, _)| *name == "X")
        .map(|(_, value)| value)
    {
        Some(p::Value::Mapping(mapping)) => mapping,
        _ => unreachable!("an X axis preset"),
    }
}

/// `value` converted to `to`. A plain value becomes a source that starts at
/// it; a source becomes the plain value it starts at. Across space, a color
/// fades from itself to black along the axis, and a number starts flat.
fn promote(slot: &Slot, value: &p::Value, to: Option<p::SourceKind>) -> p::Value {
    // A color or a vector: three channels.
    let triple = match value {
        p::Value::Color(rgb) | p::Value::Vector(rgb) => Some(*rgb),
        p::Value::Time(curve) | p::Value::Hit(curve) if curve.is_color() => {
            Some(start_color(curve))
        }
        p::Value::Space(space) => space.gradient.as_ref().map(|gradient| gradient.sample(0.)),
        _ => None,
    }
    .or_else(|| match (slot.vector(), &slot.spec.default) {
        (true, Some(p::Value::Vector(v))) => Some(*v),
        _ => None,
    });
    let number = match value {
        p::Value::Noise(p::NoiseSource { range, .. }) => Some(range[1]),
        // Audio at its loudest gives the top of the input: 1, or the most
        // degrees of a fan or a size.
        p::Value::Audio(_) => Some(slot.range()[1]),
        p::Value::Space(space) => space.curve.as_ref().map(|curve| curve.sample(0.)),
        other => start_number(other),
    }
    .or_else(|| slot.spec.default.as_ref().and_then(level))
    .unwrap_or(1.);
    let number = slot.fit(number);
    let key = triple.map_or(p::Key::Number(number), p::Key::Color);
    let flat = || p::SourceCurve::from(p::Keyframes::with_eases([(0., key), (1., key)], &[]));
    match to {
        None => match triple {
            Some(v) if slot.vector() => p::Value::Vector(v),
            Some(rgb) => p::Value::Color(rgb),
            None => slot.plain(number),
        },
        Some(p::SourceKind::Time) => p::Value::Time(flat()),
        Some(p::SourceKind::Hit) => p::Value::Hit(flat()),
        Some(p::SourceKind::Noise) => p::Value::Noise(p::NoiseSource {
            speed: NOISE_SPEED,
            range: if slot.vector() {
                slot.range()
            } else {
                [number.min(0.), number.max(0.)]
            },
        }),
        Some(p::SourceKind::Audio) => {
            let first = &p::presets().frequencies[0];
            p::Value::Audio(p::AudioLevel {
                from_hz: first.from_hz,
                to_hz: first.to_hz,
                floor: 0.,
                threshold: 0.,
            })
        }
        Some(p::SourceKind::Space) => {
            let stop = |t: f64, color: [f64; 3]| p::ColorStop {
                t,
                color,
                alpha: 1.,
            };
            let level = number.clamp(0., 1.);
            p::Value::Space(match triple {
                Some(rgb) => p::SpaceSource {
                    axis: axis_x(),
                    gradient: Some(p::Gradient {
                        stops: vec![stop(0., rgb), stop(1., [0.; 3])],
                    }),
                    curve: None,
                    movement: None,
                },
                None => p::SpaceSource {
                    axis: axis_x(),
                    gradient: None,
                    curve: Some(p::Envelope::linear(vec![[0., level], [1., level]])),
                    movement: None,
                },
            })
        }
    }
}

/// A stroke that starts moving: forward along the axis, with the chase's
/// travel and width.
fn new_movement() -> p::Movement {
    let default = |key: &str| {
        p::movement_inputs()
            .into_iter()
            .find(|(name, _)| *name == key)
            .and_then(|(_, input)| input.default)
            .expect("a movement default")
    };
    let path = match p::path_presets().remove(0).1 {
        p::Value::Envelope(path) => path,
        _ => unreachable!("paths are curves"),
    };
    p::Movement {
        path,
        travel: default("travel"),
        width: default("width"),
        width_relative: true,
        boundary: p::Boundary::Clip,
    }
}

/// `value` with its space source passed through `change`.
fn edit_space(value: &p::Value, change: impl FnOnce(&mut p::SpaceSource)) -> Option<p::Value> {
    match value {
        p::Value::Space(space) => {
            let mut space = space.clone();
            change(&mut space);
            Some(p::Value::Space(space))
        }
        _ => None,
    }
}

/// `value` with its moving stroke passed through `change`.
fn edit_movement(value: &p::Value, change: impl FnOnce(&mut p::Movement)) -> Option<p::Value> {
    edit_space(value, |space| {
        if let Some(movement) = space.movement.as_mut() {
            change(movement);
        }
    })
}

/// `value` with the gradient and curve of a gradient read over time or per
/// hit passed through `change`.
fn edit_gradient_curve(
    value: &p::Value,
    change: impl FnOnce(&mut p::GradientCurve),
) -> Option<p::Value> {
    match value {
        p::Value::Time(p::SourceCurve::Gradient(read)) => {
            let mut read = read.clone();
            change(&mut read);
            Some(p::Value::Time(p::SourceCurve::Gradient(read)))
        }
        p::Value::Hit(p::SourceCurve::Gradient(read)) => {
            let mut read = read.clone();
            change(&mut read);
            Some(p::Value::Hit(p::SourceCurve::Gradient(read)))
        }
        _ => None,
    }
}

/// The axis of an axis input or of a space source.
fn mapping_mut(value: &mut p::Value) -> Option<&mut p::MappingSpec> {
    match value {
        p::Value::Mapping(mapping) => Some(mapping),
        p::Value::Space(space) => Some(&mut space.axis),
        _ => None,
    }
}

/// A pattern gradient in the strip's terms.
fn ui_gradient(gradient: &p::Gradient) -> Gradient {
    Gradient::new(gradient.stops.iter().map(|stop| GradientStop {
        t: stop.t as f32,
        color: Rgba {
            r: stop.color[0] as f32,
            g: stop.color[1] as f32,
            b: stop.color[2] as f32,
            a: stop.alpha as f32,
        },
    }))
}

/// The strip's stops as a pattern gradient, in order and within
/// 0–1.
fn pattern_gradient(gradient: &Gradient) -> p::Gradient {
    let mut stops: Vec<p::ColorStop> = gradient
        .stops()
        .iter()
        .map(|stop| p::ColorStop {
            t: f64::from(stop.t).clamp(0., 1.),
            color: [stop.color.r, stop.color.g, stop.color.b].map(|v| f64::from(v).clamp(0., 1.)),
            alpha: f64::from(stop.color.a).clamp(0., 1.),
        })
        .collect();
    stops.sort_by(|a, b| a.t.total_cmp(&b.t));
    p::Gradient { stops }
}

/// Named curves in the strip's 0–1 box.
type Curves = Vec<(String, p::Envelope)>;

fn curves_of(options: Vec<(&'static str, p::Value)>) -> Curves {
    options
        .into_iter()
        .filter_map(|(name, value)| match value {
            p::Value::Envelope(envelope) => Some((name.to_string(), envelope)),
            _ => None,
        })
        .collect()
}

/// The curves a still number across space starts from: the shipped general
/// curves, 0–1 along the axis.
fn along_curves() -> Curves {
    p::presets()
        .curves
        .iter()
        .filter(|preset| preset.input.is_none())
        .map(|preset| {
            let envelope = preset.curve.map(|key| match key {
                p::Key::Number(v) => v.clamp(0., 1.),
                p::Key::Color(_) => 0.,
            });
            (preset.name.clone(), envelope)
        })
        .collect()
}

fn thumbs(curves: &Curves) -> Vec<(SharedString, Thumb)> {
    curves
        .iter()
        .map(|(name, envelope)| (name.clone().into(), Thumb::Curve(envelope.clone())))
        .collect()
}

fn curve_at(curves: &Curves, envelope: &p::Envelope) -> Option<usize> {
    curves.iter().position(|(_, curve)| curve == envelope)
}

/// A named curve preset (values 0–1) scaled to the input's range. Eases are
/// local to their segments, so they keep their shape.
fn scaled(slot: &Slot, curve: &p::Keyframes) -> p::Keyframes {
    let [low, high] = slot.range();
    curve.map(|key| match key {
        p::Key::Number(v) => p::Key::Number(slot.fit(low + v * (high - low))),
        color => *color,
    })
}

fn same_curve(a: &p::Keyframes, b: &p::Keyframes) -> bool {
    let close = |a: f64, b: f64| (a - b).abs() <= 1e-9;
    let same_ease = |a: p::Ease, b: p::Ease| match (a, b) {
        (p::Ease::Bezier(a), p::Ease::Bezier(b)) => a.iter().zip(&b).all(|(a, b)| close(*a, *b)),
        (a, b) => a == b,
    };
    a.points.len() == b.points.len()
        && a.points.iter().zip(&b.points).all(|(a, b)| {
            close(a.x, b.x)
                && same_ease(a.ease, b.ease)
                && match (a.value, b.value) {
                    (p::Key::Number(a), p::Key::Number(b)) => close(a, b),
                    (p::Key::Color(a), p::Key::Color(b)) => {
                        a.iter().zip(&b).all(|(a, b)| close(*a, *b))
                    }
                    _ => false,
                }
        })
}

/// A number curve in the strip's 0–1 box.
fn envelope_of(slot: &Slot, curve: &p::Keyframes) -> p::Envelope {
    let [low, high] = slot.range();
    let envelope = curve.map(|key| {
        let value = match key {
            p::Key::Number(v) => *v,
            p::Key::Color(_) => low,
        };
        ((value - low) / (high - low)).clamp(0., 1.)
    });
    if envelope.validate().is_ok() {
        envelope
    } else {
        p::Envelope::linear(vec![[0., 0.], [1., 0.]])
    }
}

/// The strip's box back as a curve in the input's unit. The eases
/// are the same, so what is drawn is what plays.
fn keyframes_of(slot: &Slot, envelope: &p::Envelope) -> p::Keyframes {
    let [low, high] = slot.range();
    envelope.map(|y| p::Key::Number(slot.fit(low + y * (high - low))))
}

/// A direction as turn and tilt, in degrees. Turn 0 is downstage and grows
/// toward stage right; tilt 0 is level and −90 is straight down. A direction
/// straight up or down has no turn: `None`.
fn turn_tilt([u, v, z]: [f64; 3]) -> (Option<f64>, f64) {
    let flat = u.hypot(v);
    let tilt = z.atan2(flat).to_degrees();
    let turn = (flat > 1e-6).then(|| u.atan2(v).to_degrees());
    (turn, tilt)
}

/// The unit direction at `turn` and `tilt` degrees, to four places.
fn direction_at(turn: f64, tilt: f64) -> [f64; 3] {
    let (turn, tilt) = (turn.to_radians(), tilt.to_radians());
    [tilt.cos() * turn.sin(), tilt.cos() * turn.cos(), tilt.sin()]
        .map(|v| (v * 1e4).round() / 1e4 + 0.)
}

/// A stored vector, spelled for a caption: "U 0.00 · V 0.77 · Z −0.64".
fn vector_text(v: [f64; 3]) -> String {
    let part = |name: &str, v: f64| {
        let text = format!("{:.2}", v.abs());
        let sign = if v < 0. && text != "0.00" { "−" } else { "" };
        format!("{name} {sign}{text}")
    };
    format!(
        "{} · {} · {}",
        part("U", v[0]),
        part("V", v[1]),
        part("Z", v[2])
    )
}

/// The vectors a vector input shows: its value when fixed, the start and
/// the end of a curve over the clip.
fn vector_ends(value: &p::Value) -> Vec<[f64; 3]> {
    let key = |key: &p::Key| match key {
        p::Key::Color(v) => *v,
        p::Key::Number(n) => [*n; 3],
    };
    match value {
        p::Value::Vector(v) => vec![*v],
        p::Value::Time(p::SourceCurve::Keys(curve)) => {
            match (curve.points.first(), curve.points.last()) {
                (Some(first), Some(last)) => vec![key(&first.value), key(&last.value)],
                _ => Vec::new(),
            }
        }
        _ => Vec::new(),
    }
}

/// `value` with end `end` (see [`vector_ends`]) passed through `edit`: the
/// vector itself, or the first or last point of a curve over the clip.
fn edit_vector_end(
    value: &p::Value,
    end: usize,
    edit: impl FnOnce([f64; 3]) -> [f64; 3],
) -> Option<p::Value> {
    match value {
        p::Value::Vector(v) => Some(p::Value::Vector(edit(*v))),
        p::Value::Time(p::SourceCurve::Keys(curve)) if curve.is_color() => {
            let mut curve = curve.clone();
            let at = if end == 0 { 0 } else { curve.points.len() - 1 };
            let p::Key::Color(v) = curve.points[at].value else {
                return None;
            };
            curve.points[at].value = p::Key::Color(edit(v));
            Some(p::Value::Time(curve.into()))
        }
        _ => None,
    }
}

// -- widgets ------------------------------------------------------------------

/// A number field, with its unit.
fn number_field(
    label: String,
    value: f64,
    [min, max]: [f64; 2],
    width: f32,
    unit: Option<&'static str>,
    window: &mut Window,
    cx: &mut Context<Luma>,
) -> Entity<DraftedNumber> {
    cx.new(|cx| {
        let field = DraftedNumber::new(label, value, min, max, width, window, cx);
        match unit {
            Some("beats") => field.with_unit("beats").with_per_unit("per beat", cx),
            Some(unit) => field.with_unit(unit),
            None => field,
        }
    })
}

/// A number field that edits one part of the stored value.
fn number_edit(
    def: &PatternArgDef,
    spec: &'static p::Input,
    field: &Entity<DraftedNumber>,
    cx: &mut Context<Luma>,
    edit: fn(&mut p::Value, f64),
) -> Subscription {
    let def = def.clone();
    cx.subscribe(field, move |this: &mut Luma, _, event: &NumberEvent, cx| {
        let NumberEvent::Committed(number) = *event;
        this.form_edit(&def, spec, cx, |value| {
            let mut value = value.clone();
            edit(&mut value, number);
            Some(value)
        });
    })
}

/// A curve strip for a part of the stored value; `edit` puts the edited
/// part back.
fn strip_editor(
    strip: CurveStrip,
    def: &PatternArgDef,
    spec: &'static p::Input,
    cx: &mut Context<Luma>,
    subs: &mut Vec<Subscription>,
    edit: fn(&p::Value, StripValue) -> Option<p::Value>,
) -> Entity<CurveStrip> {
    let entity = cx.new(|_| strip);
    let def = def.clone();
    subs.push(cx.subscribe(
        &entity,
        move |this: &mut Luma, _, event: &StripChanged, cx| {
            let value = event.0.clone();
            this.form_edit(&def, spec, cx, |stored| edit(stored, value));
        },
    ));
    entity
}

/// A gradient's strip, with the shipped gradients as presets.
fn gradient_strip(id: String, gradient: &p::Gradient) -> CurveStrip {
    CurveStrip::new(id, StripValue::Gradient(ui_gradient(gradient)))
        .with_presets(strip::gradient_presets())
}

/// The clock of a strip over time: the clip for a `time` source, each hit
/// for a `hit` source. It reads the primary clip each frame, so it follows a
/// moved clip, a new `every` and the playhead.
fn strip_clock(app: WeakEntity<Luma>, kind: p::SourceKind) -> ClockSource {
    Rc::new(move |cx: &App| {
        let app = app.upgrade()?;
        let Some(Body::TrackEditor(editor)) = app.read(cx).workspace.active_body() else {
            return None;
        };
        clip_clock(editor, kind)
    })
}

/// Where the playhead is in the primary clip's cycle, and how many beats the
/// cycle spans. A `hit` cycle is one hit: hits come every `every` beats from
/// the clip's start (0 is one hit over the clip) and last until the next
/// one, or `duration` beats in a form that has it.
fn clip_clock(editor: &Editor, kind: p::SourceKind) -> Option<Clock> {
    let clip = primary_clip(editor)?;
    let timeline = editor.beats.as_deref()?.timeline().ok()?;
    let start = timeline.beat_at(clip.start).ok()?;
    let length = timeline.beat_at(clip.end).ok()? - start;
    let elapsed = timeline
        .beat_at(f64::from(editor.transport.position))
        .ok()?
        - start;
    if length <= 0. {
        return None;
    }
    // A plain number of beats, as the sheet writes it, or a tagged one.
    let beats = |key: &str| {
        let value = clip.args.get(key)?;
        value.as_f64().or_else(|| {
            (value["type"] == "beats")
                .then(|| value["value"].as_f64())
                .flatten()
        })
    };
    let inside = (0. ..=length).contains(&elapsed);
    let (span, phase) = match (kind, beats("every")) {
        (p::SourceKind::Hit, Some(every)) if every > 0. => {
            let life = beats("duration").unwrap_or(every);
            let into = elapsed.rem_euclid(every);
            (life, (into <= life).then(|| into / life))
        }
        // Once over the clip, as `time` is.
        (p::SourceKind::Hit, Some(_)) | (p::SourceKind::Time, _) => {
            (length, Some(elapsed / length))
        }
        _ => return None,
    };
    Some(Clock {
        beats: span,
        phase: phase.filter(|_| inside),
        playing: editor.transport.playing,
    })
}

/// The place of each head on a space source's axis, while it lies still
/// along it. Resolved again only when the axis, the heads or the seed
/// change.
fn strip_heads(app: WeakEntity<Luma>, def: &PatternArgDef, spec: &'static p::Input) -> HeadSource {
    type Resolved = (p::MappingSpec, Rc<[p::Cell]>, u64, Rc<[f64]>);
    let cache: Rc<std::cell::RefCell<Option<Resolved>>> = Rc::default();
    let key = def.id.clone();
    Rc::new(move |cx: &App| {
        let app = app.upgrade()?;
        let Some(Body::TrackEditor(editor)) = app.read(cx).workspace.active_body() else {
            return None;
        };
        let clip = primary_clip(editor)?;
        let Ok(p::Value::Space(space)) = decode(spec.value_type, clip.args.get(&key)?) else {
            return None;
        };
        if space.movement.is_some() {
            return None;
        }
        let cells = editor.sheet.heads.cells.clone()?;
        let seed = clip.core.as_ref().map_or(0, |clip| clip.seed);
        let mut cache = cache.borrow_mut();
        if let Some((axis, held, at, positions)) = cache.as_ref() {
            if *axis == space.axis && Rc::ptr_eq(held, &cells) && *at == seed {
                return Some(positions.clone());
            }
        }
        let positions: Rc<[f64]> = space
            .axis
            .resolve(&cells, seed)
            .ok()?
            .coordinates
            .iter()
            .map(|coordinate| coordinate.position)
            .collect();
        *cache = Some((space.axis, cells, seed, positions.clone()));
        Some(positions)
    })
}

/// What a strip over time spans, for its caption.
fn over(kind: Option<p::SourceKind>) -> &'static str {
    match kind {
        Some(p::SourceKind::Hit) => "Over each hit",
        _ => "Over the clip",
    }
}

/// The number fields of an axis: a custom plane's axis, and a mirror's
/// normal and offset.
fn axis_fields(
    name: &str,
    mapping: &p::MappingSpec,
    def: &PatternArgDef,
    spec: &'static p::Input,
    window: &mut Window,
    cx: &mut Context<Luma>,
    subs: &mut Vec<Subscription>,
) -> AxisFields {
    let normal = plane_normal(mapping);
    let third = (FIELD_W - 16.) / 3.;
    let plane: [_; 3] = std::array::from_fn(|axis| {
        number_field(
            format!("{name}: Plane {}", ["U", "V", "Z"][axis]),
            normal[axis],
            [-1e9, 1e9],
            third,
            None,
            window,
            cx,
        )
    });
    subs.push(number_edit(def, spec, &plane[0], cx, |value, n| {
        set_normal(value, 0, n)
    }));
    subs.push(number_edit(def, spec, &plane[1], cx, |value, n| {
        set_normal(value, 1, n)
    }));
    subs.push(number_edit(def, spec, &plane[2], cx, |value, n| {
        set_normal(value, 2, n)
    }));
    let mirror = mirror_plane(mapping);
    let normal: [_; 3] = std::array::from_fn(|axis| {
        number_field(
            format!("{name}: Mirror {}", ["U", "V", "Z"][axis]),
            mirror.normal[axis],
            [-1e9, 1e9],
            third,
            None,
            window,
            cx,
        )
    });
    subs.push(number_edit(def, spec, &normal[0], cx, |value, n| {
        set_mirror(value, |plane| plane.normal[0] = n)
    }));
    subs.push(number_edit(def, spec, &normal[1], cx, |value, n| {
        set_mirror(value, |plane| plane.normal[1] = n)
    }));
    subs.push(number_edit(def, spec, &normal[2], cx, |value, n| {
        set_mirror(value, |plane| plane.normal[2] = n)
    }));
    let offset = number_field(
        format!("{name}: Mirror offset"),
        mirror.offset,
        [-1e9, 1e9],
        FIELD_W,
        Some("m"),
        window,
        cx,
    );
    subs.push(number_edit(def, spec, &offset, cx, |value, n| {
        set_mirror(value, |plane| plane.offset = n)
    }));
    AxisFields {
        plane,
        normal,
        offset,
        custom_mirror: Rc::new(std::cell::Cell::new(
            mirror_index(mapping.mirror.as_ref()) == CUSTOM_MIRROR,
        )),
    }
}

/// The control for a form input's current value.
pub(super) fn widget(
    slot: &Slot,
    def: &PatternArgDef,
    stored: &serde_json::Value,
    window: &mut Window,
    cx: &mut Context<Luma>,
    subs: &mut Vec<Subscription>,
) -> Widget {
    let value = decode(slot.spec.value_type, stored).ok();
    let spec = slot.spec;
    let name = spec.name.clone();
    let on_number =
        |field: &Entity<DraftedNumber>, cx: &mut Context<Luma>, edit: fn(&mut p::Value, f64)| {
            number_edit(def, spec, field, cx, edit)
        };
    match value {
        // A direction is edited as turn and tilt, which the row draws; a
        // point as U, V and Z in metres. A curve edits its two ends.
        Some(value) if slot.vector() && !vector_ends(&value).is_empty() => {
            let ends = vector_ends(&value);
            if slot.key == "direction" {
                return Widget::Direction(
                    ends.iter().map(|v| turn_tilt(*v).0.unwrap_or(0.)).collect(),
                );
            }
            let names = if ends.len() == 1 {
                vec![""]
            } else {
                vec!["Start ", "End "]
            };
            let third = (FIELD_W - 16.) / 3.;
            Widget::Point(
                ends.iter()
                    .zip(names)
                    .enumerate()
                    .map(|(end, (v, prefix))| {
                        std::array::from_fn(|axis| {
                            let field = number_field(
                                format!("{name}: {prefix}{}", ["U", "V", "Z"][axis]),
                                v[axis],
                                [-POINT_REACH, POINT_REACH],
                                third,
                                Some("m"),
                                window,
                                cx,
                            );
                            let def = def.clone();
                            subs.push(cx.subscribe(
                                &field,
                                move |this: &mut Luma, _, event: &NumberEvent, cx| {
                                    let NumberEvent::Committed(n) = *event;
                                    this.form_edit(&def, spec, cx, |value| {
                                        edit_vector_end(value, end, |mut v| {
                                            v[axis] = n;
                                            v
                                        })
                                    });
                                },
                            ));
                            field
                        })
                    })
                    .collect(),
            )
        }
        Some(
            p::Value::Time(p::SourceCurve::Gradient(read))
            | p::Value::Hit(p::SourceCurve::Gradient(read)),
        ) => {
            let gradient = strip_editor(
                gradient_strip(name.clone(), &read.gradient),
                def,
                spec,
                cx,
                subs,
                |value, edited| match edited {
                    StripValue::Gradient(g) => {
                        edit_gradient_curve(value, |read| read.gradient = pattern_gradient(&g))
                    }
                    _ => None,
                },
            );
            let mode = slot.mode.unwrap_or(p::SourceKind::Time);
            let curve = strip_editor(
                CurveStrip::new(format!("{name} curve"), StripValue::Number(read.curve))
                    .over_time(strip_clock(cx.entity().downgrade(), mode)),
                def,
                spec,
                cx,
                subs,
                |value, edited| match edited {
                    StripValue::Number(e) => edit_gradient_curve(value, |read| read.curve = e),
                    _ => None,
                },
            );
            Widget::GradientCurve(gradient, curve)
        }
        Some(p::Value::Space(space)) => {
            let axis = axis_fields(&name, &space.axis, def, spec, window, cx, subs);
            let heads = strip_heads(cx.entity().downgrade(), def, spec);
            let along = match (&space.gradient, space.curve.clone()) {
                (Some(gradient), _) => gradient_strip(name.clone(), gradient),
                (None, curve) => CurveStrip::new(
                    name.clone(),
                    StripValue::Number(
                        curve.unwrap_or_else(|| p::Envelope::linear(vec![[0., 1.], [1., 1.]])),
                    ),
                ),
            };
            let along = strip_editor(
                along.across_space(heads),
                def,
                spec,
                cx,
                subs,
                |value, edited| match edited {
                    StripValue::Gradient(g) => {
                        edit_space(value, |space| space.gradient = Some(pattern_gradient(&g)))
                    }
                    StripValue::Number(e) => edit_space(value, |space| space.curve = Some(e)),
                    StripValue::Colors(_) => None,
                },
            );
            let movement = space
                .movement
                .as_deref()
                .cloned()
                .unwrap_or_else(new_movement);
            let path = strip_editor(
                CurveStrip::new(
                    format!("{name} path"),
                    StripValue::Number(movement.path.clone()),
                ),
                def,
                spec,
                cx,
                subs,
                |value, edited| match edited {
                    StripValue::Number(e) => edit_movement(value, |movement| movement.path = e),
                    _ => None,
                },
            );
            let travel = number_field(
                format!("{name}: Travel"),
                start_number(&movement.travel).unwrap_or(0.),
                [0., 1e9],
                FIELD_W,
                Some("beats"),
                window,
                cx,
            );
            subs.push(on_number(&travel, cx, |value, v| {
                if let Some(changed) = edit_movement(value, |m| m.travel = p::Value::Beats(v)) {
                    *value = changed;
                }
            }));
            let width = number_field(
                format!("{name}: Width"),
                start_number(&movement.width).unwrap_or(0.),
                [0., p::MAX_WIDTH],
                FIELD_W,
                None,
                window,
                cx,
            );
            subs.push(on_number(&width, cx, |value, v| {
                if let Some(changed) = edit_movement(value, |m| m.width = p::Value::Number(v)) {
                    *value = changed;
                }
            }));
            Widget::Space(SpaceFields {
                axis,
                along,
                path,
                travel,
                width,
            })
        }
        Some(
            p::Value::Time(p::SourceCurve::Keys(curve))
            | p::Value::Hit(p::SourceCurve::Keys(curve)),
        ) if curve.is_color() => {
            let mode = slot.mode.unwrap_or(p::SourceKind::Time);
            let strip = CurveStrip::new(name, StripValue::Colors(curve))
                .with_presets(strip::color_curve_presets())
                .over_time(strip_clock(cx.entity().downgrade(), mode));
            Widget::Strip(strip_editor(strip, def, spec, cx, subs, |value, edited| {
                let StripValue::Colors(keys) = edited else {
                    return None;
                };
                match value {
                    p::Value::Time(_) => Some(p::Value::Time(keys.into())),
                    p::Value::Hit(_) => Some(p::Value::Hit(keys.into())),
                    _ => None,
                }
            }))
        }
        Some(
            p::Value::Time(p::SourceCurve::Keys(curve))
            | p::Value::Hit(p::SourceCurve::Keys(curve)),
        ) => {
            let mode = slot.mode.unwrap_or(p::SourceKind::Time);
            let clock = strip_clock(cx.entity().downgrade(), mode);
            let entity = cx.new(|_| {
                CurveStrip::new(name, StripValue::Number(envelope_of(slot, &curve)))
                    .with_scale(slot.range(), slot.unit())
                    .over_time(clock)
            });
            let def = def.clone();
            let shape = *slot;
            subs.push(cx.subscribe(
                &entity,
                move |this: &mut Luma, _, event: &StripChanged, cx| {
                    let StripValue::Number(envelope) = &event.0 else {
                        return;
                    };
                    let curve = keyframes_of(&shape, envelope).into();
                    let value = if mode == p::SourceKind::Hit {
                        p::Value::Hit(curve)
                    } else {
                        p::Value::Time(curve)
                    };
                    this.arg_live(&def.id, document::wire_value(&value), cx);
                },
            ));
            Widget::Strip(entity)
        }
        Some(p::Value::Noise(noise)) => {
            let half = (FIELD_W - 8.) / 2.;
            let speed = number_field(
                format!("{name}: Speed"),
                noise.speed,
                [MIN_BEATS, 1e9],
                FIELD_W,
                Some("beats"),
                window,
                cx,
            );
            let low = number_field(
                format!("{name}: Low"),
                noise.range[0],
                slot.range(),
                half,
                slot.unit(),
                window,
                cx,
            );
            let high = number_field(
                format!("{name}: High"),
                noise.range[1],
                slot.range(),
                half,
                slot.unit(),
                window,
                cx,
            );
            subs.push(on_number(&speed, cx, |value, v| {
                if let p::Value::Noise(noise) = value {
                    noise.speed = v;
                }
            }));
            subs.push(on_number(&low, cx, |value, v| {
                if let p::Value::Noise(noise) = value {
                    noise.range[0] = v;
                }
            }));
            subs.push(on_number(&high, cx, |value, v| {
                if let p::Value::Noise(noise) = value {
                    noise.range[1] = v;
                }
            }));
            Widget::Noise([speed, low, high])
        }
        Some(p::Value::Audio(audio)) => {
            let half = (FIELD_W - 8.) / 2.;
            let (min, max) = (p::AudioLevel::MIN_HZ, p::AudioLevel::MAX_HZ);
            let hz = |label: String, value: f64, window: &mut Window, cx: &mut Context<Luma>| {
                cx.new(|cx| {
                    DraftedNumber::new(label, value, min, max, half, window, cx).with_unit("Hz")
                })
            };
            let from = hz(format!("{name}: From"), audio.from_hz, window, cx);
            let to = hz(format!("{name}: To"), audio.to_hz, window, cx);
            let percent = |label: &str, value: f64, window: &mut Window, cx: &mut Context<Luma>| {
                cx.new(|cx| {
                    DraftedNumber::new(
                        format!("{name}: {label}"),
                        value * 100.,
                        0.,
                        100.,
                        FIELD_W,
                        window,
                        cx,
                    )
                    .with_unit("%")
                })
            };
            let floor = percent("Floor", audio.floor, window, cx);
            let threshold = percent("Threshold", audio.threshold, window, cx);
            // A range that would cross is not stored.
            subs.push(on_number(&from, cx, |value, v| {
                if let p::Value::Audio(audio) = value {
                    audio.from_hz = v.min(audio.to_hz - 1.);
                }
            }));
            subs.push(on_number(&to, cx, |value, v| {
                if let p::Value::Audio(audio) = value {
                    audio.to_hz = v.max(audio.from_hz + 1.);
                }
            }));
            subs.push(on_number(&floor, cx, |value, v| {
                if let p::Value::Audio(audio) = value {
                    audio.floor = (v / 100.).clamp(0., 1.);
                }
            }));
            subs.push(on_number(&threshold, cx, |value, v| {
                if let p::Value::Audio(audio) = value {
                    audio.threshold = (v / 100.).clamp(0., 1.);
                }
            }));
            Widget::Audio([from, to, floor, threshold])
        }
        value => {
            let current = value.as_ref().and_then(level);
            if def.id == "grain" {
                let entity = number_field(
                    format!("{name}: Clump size"),
                    current.filter(|n| *n >= 2.).unwrap_or(2.),
                    [2., 64.],
                    FIELD_W,
                    None,
                    window,
                    cx,
                );
                subs.push(on_number(&entity, cx, |value, v| {
                    *value = p::Value::Number(v.round())
                }));
                return Widget::Grain(entity);
            }
            if slot.once() {
                let entity = number_field(
                    format!("{name}: Beats"),
                    current.unwrap_or(0.),
                    [0., 1e9],
                    FIELD_W,
                    Some("beats"),
                    window,
                    cx,
                );
                subs.push(on_number(&entity, cx, |value, v| {
                    *value = p::Value::Beats(v)
                }));
                return Widget::Every(entity);
            }
            if slot.speed {
                // A speed stays above zero.
                let entity = number_field(
                    name,
                    slot.fit(current.unwrap_or(MIN_BEATS)),
                    [MIN_BEATS, 1e9],
                    FIELD_W,
                    Some("beats"),
                    window,
                    cx,
                );
                subs.push(on_number(&entity, cx, |value, v| {
                    *value = p::Value::Beats(v)
                }));
                return Widget::Scalar(entity);
            }
            if let Some(p::Value::Mapping(mapping)) = &value {
                return Widget::Axis(axis_fields(&name, mapping, def, spec, window, cx, subs));
            }
            if let Some(p::Author::Choice { options, .. }) = &spec.author {
                // A choice of curves edits a custom curve in a strip.
                let editor = envelope_options(options).map(|curves| {
                    let envelope = match &value {
                        Some(p::Value::Envelope(envelope)) if envelope.validate().is_ok() => {
                            envelope.clone()
                        }
                        _ => match &curves[0].1 {
                            Thumb::Curve(envelope) => envelope.clone(),
                            Thumb::Gradient(_) => unreachable!("curve options"),
                        },
                    };
                    let entity =
                        cx.new(|_| CurveStrip::new(name.clone(), StripValue::Number(envelope)));
                    let def = def.clone();
                    subs.push(cx.subscribe(
                        &entity,
                        move |this: &mut Luma, _, event: &StripChanged, cx| {
                            if let StripValue::Number(envelope) = &event.0 {
                                let value = p::Value::Envelope(envelope.clone());
                                this.arg_live(&def.id, document::wire_value(&value), cx);
                            }
                        },
                    ));
                    entity
                });
                return Widget::Preset(options, editor);
            }
            if let Some([min, max]) = slot.bounds() {
                // A number with bounds of its own, such as a chase width.
                let entity = number_field(
                    name,
                    slot.fit(current.unwrap_or(min)),
                    [min, max],
                    FIELD_W,
                    slot.unit(),
                    window,
                    cx,
                );
                subs.push(on_number(&entity, cx, |value, v| {
                    *value = p::Value::Number(v)
                }));
                return Widget::Scalar(entity);
            }
            plain_widget(def, stored, true, &[], window, cx, subs)
        }
    }
}

/// Push a stored value into a form row's controls. Rebuilds the control when
/// the value changed between plain and a source, or between two sources.
/// Returns false for a plain control the sheet itself refreshes.
pub(super) fn resync(
    slot: &mut Slot,
    def: &PatternArgDef,
    widget: &mut Widget,
    stored: &serde_json::Value,
    window: &mut Window,
    cx: &mut Context<Luma>,
    subs: &mut Vec<Subscription>,
) -> bool {
    let value = decode(slot.spec.value_type, stored).ok();
    let mode = value.as_ref().and_then(p::Value::source_kind);
    // Keyframes and a gradient read along a curve are one source with two
    // editors; a space source's editors follow a gradient or a curve.
    let reshaped = match (&*widget, &value) {
        (
            Widget::GradientCurve(..),
            Some(p::Value::Time(p::SourceCurve::Keys(_)) | p::Value::Hit(p::SourceCurve::Keys(_))),
        ) => true,
        (
            Widget::Strip(_),
            Some(
                p::Value::Time(p::SourceCurve::Gradient(_))
                | p::Value::Hit(p::SourceCurve::Gradient(_)),
            ),
        ) => true,
        (Widget::Space(fields), Some(p::Value::Space(space))) => {
            fields.along.read(cx).value().is_color() != space.gradient.is_some()
        }
        _ => false,
    };
    if mode != slot.mode || reshaped {
        slot.mode = mode;
        *widget = self::widget(slot, def, stored, window, cx, subs);
        return true;
    }
    match (&mut *widget, value) {
        (Widget::Direction(turns), Some(value)) => {
            // A direction straight up or down keeps the turn it had.
            for (held, v) in turns.iter_mut().zip(vector_ends(&value)) {
                if let (Some(turn), _) = turn_tilt(v) {
                    *held = turn;
                }
            }
        }
        (Widget::Point(sets), Some(value)) => {
            for (fields, v) in sets.iter().zip(vector_ends(&value)) {
                for (field, n) in fields.iter().zip(v) {
                    field.update(cx, |field, cx| field.set_value(n, cx));
                }
            }
        }
        (
            Widget::Strip(entity),
            Some(
                p::Value::Time(p::SourceCurve::Keys(curve))
                | p::Value::Hit(p::SourceCurve::Keys(curve)),
            ),
        ) => {
            let value = if curve.is_color() {
                StripValue::Colors(curve)
            } else {
                StripValue::Number(envelope_of(slot, &curve))
            };
            entity.update(cx, |editor, cx| editor.set_value(value, cx));
        }
        (
            Widget::GradientCurve(gradient, curve),
            Some(
                p::Value::Time(p::SourceCurve::Gradient(read))
                | p::Value::Hit(p::SourceCurve::Gradient(read)),
            ),
        ) => {
            let colors = StripValue::Gradient(ui_gradient(&read.gradient));
            gradient.update(cx, |editor, cx| editor.set_value(colors, cx));
            let read = StripValue::Number(read.curve);
            curve.update(cx, |editor, cx| editor.set_value(read, cx));
        }
        (Widget::Space(fields), Some(p::Value::Space(space))) => {
            resync_axis(&fields.axis, &space.axis, cx);
            let along = match (&space.gradient, space.curve) {
                (Some(gradient), _) => Some(StripValue::Gradient(ui_gradient(gradient))),
                (None, curve) => curve.map(StripValue::Number),
            };
            if let Some(along) = along {
                fields
                    .along
                    .update(cx, |editor, cx| editor.set_value(along, cx));
            }
            if let Some(movement) = space.movement {
                let movement = *movement;
                let path = StripValue::Number(movement.path);
                fields
                    .path
                    .update(cx, |editor, cx| editor.set_value(path, cx));
                if let Some(n) = start_number(&movement.travel) {
                    fields.travel.update(cx, |field, cx| field.set_value(n, cx));
                }
                if let Some(n) = start_number(&movement.width) {
                    fields.width.update(cx, |field, cx| field.set_value(n, cx));
                }
            }
        }
        (Widget::Noise([speed, low, high]), Some(p::Value::Noise(noise))) => {
            speed.update(cx, |field, cx| field.set_value(noise.speed, cx));
            low.update(cx, |field, cx| field.set_value(noise.range[0], cx));
            high.update(cx, |field, cx| field.set_value(noise.range[1], cx));
        }
        (Widget::Audio([from, to, floor, threshold]), Some(p::Value::Audio(audio))) => {
            from.update(cx, |field, cx| field.set_value(audio.from_hz, cx));
            to.update(cx, |field, cx| field.set_value(audio.to_hz, cx));
            floor.update(cx, |field, cx| field.set_value(audio.floor * 100., cx));
            threshold.update(cx, |field, cx| field.set_value(audio.threshold * 100., cx));
        }
        (Widget::Grain(entity), Some(value)) => {
            if let Some(n) = level(&value).filter(|n| *n >= 2.) {
                entity.update(cx, |field, cx| field.set_value(n, cx));
            }
        }
        (Widget::Every(entity), Some(value)) => {
            if let Some(n) = level(&value) {
                entity.update(cx, |field, cx| field.set_value(n, cx));
            }
        }
        (Widget::Preset(_, Some(entity)), Some(p::Value::Envelope(envelope))) => {
            if envelope.validate().is_ok() {
                let value = StripValue::Number(envelope);
                entity.update(cx, |editor, cx| editor.set_value(value, cx));
            }
        }
        (Widget::Preset(..), _) => {}
        (Widget::Axis(fields), Some(p::Value::Mapping(mapping))) => {
            resync_axis(fields, &mapping, cx)
        }
        _ => return false,
    }
    true
}

fn resync_axis(fields: &AxisFields, mapping: &p::MappingSpec, cx: &mut Context<Luma>) {
    for (field, n) in fields.plane.iter().zip(plane_normal(mapping)) {
        field.update(cx, |field, cx| field.set_value(n, cx));
    }
    let mirror = mirror_plane(mapping);
    for (field, n) in fields.normal.iter().zip(mirror.normal) {
        field.update(cx, |field, cx| field.set_value(n, cx));
    }
    fields
        .offset
        .update(cx, |field, cx| field.set_value(mirror.offset, cx));
}

impl Luma {
    /// Rewrite one form input from its stored value. `edit` returns `None`
    /// to leave it as it is.
    fn form_edit(
        &mut self,
        def: &PatternArgDef,
        spec: &'static p::Input,
        cx: &mut Context<Self>,
        edit: impl FnOnce(&p::Value) -> Option<p::Value>,
    ) {
        let mut wire = None;
        self.with_track_editor(cx, |editor| {
            if let Ok(value) = decode(spec.value_type, &stored_arg(editor, def)) {
                wire = edit(&value).map(|value| document::wire_value(&value));
            }
        });
        if let Some(wire) = wire {
            self.arg_live(&def.id, wire, cx);
        }
    }
}

// -- rendering ----------------------------------------------------------------

/// The rows of a form clip: the selection, then each input in the form's
/// order. The rare ones sit under "Advanced". An aim shows its base, then
/// its motion, each without the rows that do not apply.
pub(super) fn rows(state: &Editor, built: &Built, app: &Entity<Luma>) -> Vec<AnyElement> {
    let mut rows = Vec::new();
    for (index, cell) in built.cells.iter().enumerate() {
        let Some(slot) = &cell.form else {
            rows.extend(arg_rows(state, app, index, cell));
            continue;
        };
        if slot.form == "aim@1" && aim_hides(built, slot.key) {
            continue;
        }
        // An aim's motion group.
        if slot.form == "aim@1" && cell.def.id == "motion" {
            rows.push(luma_ui::float::divider().into_any_element());
        }
        match control(state, app, index, cell, slot) {
            Some(control) => rows.push(row(state, app, index, cell, slot, control)),
            None => rows.extend(arg_rows(state, app, index, cell)),
        }
    }
    rows
}

/// Whether an `aim@1` row does not apply to the clip's blend, base and
/// motion: an Offset clip has no base, so base, direction and point go; the
/// direction or the point by the base; shape, size, spread and speed by the
/// motion. `every` paces a shape and a fan per hit.
fn aim_hides(built: &Built, key: &str) -> bool {
    let offset = built.blend == BlendMode::Offset;
    let stored = |key: &str| {
        built
            .cells
            .iter()
            .find(|cell| cell.def.id == key)
            .map(|cell| &cell.synced)
    };
    let choice = |key: &str| {
        stored(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
    };
    let (base, motion) = (choice("base"), choice("motion"));
    let fan_per_hit = stored("fan")
        .and_then(|fan| fan.get("type"))
        .and_then(serde_json::Value::as_str)
        == Some("hit");
    match key {
        "base" => offset,
        "direction" => offset || base != "direction",
        "point" => offset || base != "point",
        "shape" | "spread" => motion != "shape",
        "size" => motion == "none",
        "speed" => motion != "noise",
        "every" => motion != "shape" && !fan_per_hit,
        _ => false,
    }
}

/// A labelled form row. The header line carries the promote menu.
fn row(
    state: &Editor,
    app: &Entity<Luma>,
    index: usize,
    cell: &Cell,
    slot: &Slot,
    control: Div,
) -> AnyElement {
    let name = slot.spec.name.as_str();
    let promote = (!slot.spec.promotable.is_empty()).then(|| {
        div()
            .w(px(MODE_W))
            .flex()
            .flex_col()
            .child(promote_select(state, app, index, cell, slot))
            .into_any_element()
    });
    sheet_row(name, promote.into_iter().collect(), control)
}

/// The menu that switches an input between a fixed value and a source.
fn promote_select(
    state: &Editor,
    app: &Entity<Luma>,
    index: usize,
    cell: &Cell,
    slot: &Slot,
) -> Div {
    let kinds = slot.spec.promotable.clone();
    let labels: Vec<&str> = std::iter::once(FIXED)
        .chain(kinds.iter().map(|kind| source_label(*kind)))
        .collect();
    let current = slot.mode.map_or(FIXED, source_label);
    let toggle = app.clone();
    let pick = app.clone();
    let def = cell.def.clone();
    let spec = slot.spec;
    let mode = slot.mode;
    let shape = *slot;
    luma_arg_select(
        format!("{}:source", slot.spec.name),
        current,
        &labels,
        menu_visibility(state, Menu::Source(index)),
        move |_, cx| {
            toggle.update(cx, |this, cx| {
                this.with_track_editor(cx, |editor| {
                    editor.sheet.open = if editor.sheet.open == Some(Menu::Source(index)) {
                        None
                    } else {
                        Some(Menu::Source(index))
                    };
                });
            });
        },
        move |picked, _, cx| {
            let to = picked.checked_sub(1).map(|at| kinds[at]);
            pick.update(cx, |this, cx| {
                this.with_track_editor(cx, |editor| editor.sheet.open = None);
                if to == mode {
                    return;
                }
                this.form_edit(&def, spec, cx, |value| Some(promote(&shape, value, to)));
            });
        },
    )
}

/// A row of segments, one picked. Picking another runs `on_pick` with its
/// index.
fn segments(
    app: &Entity<Luma>,
    name: &str,
    labels: &[&'static str],
    chosen: usize,
    on_pick: impl Fn(usize, &mut Luma, &mut Context<Luma>) + 'static,
) -> Div {
    let on_pick = Rc::new(on_pick);
    luma_ui::float::segmented().children(labels.iter().enumerate().map(|(at, label)| {
        let app = app.clone();
        let on_pick = on_pick.clone();
        let key = format!("{name}-{label}");
        luma_ui::float::segment(*label, at == chosen, key.clone())
            .id(SharedString::from(key))
            .on_click(move |_, _, cx| {
                if at == chosen {
                    return;
                }
                app.update(cx, |this, cx| on_pick(at, this, cx));
            })
            .agent_node(Role::Button, format!("{name} {label}"))
    }))
}

/// Opens or closes this row's `Menu::Choice`.
fn choice_toggle(app: &Entity<Luma>, index: usize) -> impl Fn(&mut Window, &mut App) + Clone {
    menu_toggle(app, Menu::Choice(index))
}

/// Opens `menu`, or closes it when it is open.
fn menu_toggle(app: &Entity<Luma>, menu: Menu) -> impl Fn(&mut Window, &mut App) + Clone {
    let app = app.clone();
    move |_, cx| {
        app.update(cx, |this, cx| {
            this.with_track_editor(cx, |editor| {
                editor.sheet.open = if editor.sheet.open == Some(menu) {
                    None
                } else {
                    Some(menu)
                };
            });
        });
    }
}

/// A select that opens under `menu`.
fn menu_select(
    state: &Editor,
    app: &Entity<Luma>,
    menu: Menu,
    id: String,
    current: &str,
    labels: &[&str],
    on_pick: impl Fn(usize, &mut Luma, &mut Context<Luma>) + 'static,
) -> Div {
    luma_arg_select(
        id,
        current,
        labels,
        menu_visibility(state, menu),
        menu_toggle(app, menu),
        choice_pick(app, on_pick),
    )
}

/// The axis the custom plane turns around, or the default one to start from.
fn plane_normal(mapping: &p::MappingSpec) -> [f64; 3] {
    match &mapping.plane {
        Some(p::AxisPlane::Custom { normal }) => *normal,
        _ => [0., -1., 0.],
    }
}

fn set_normal(value: &mut p::Value, axis: usize, n: f64) {
    if let Some(mapping) = mapping_mut(value) {
        if let Some(p::AxisPlane::Custom { normal }) = &mut mapping.plane {
            normal[axis] = n;
        }
    }
}

/// The mirror choices: a plane through the middle of the span, left–right
/// (normal U), front–back (V) or up–down (Z), or a custom plane.
const MIRRORS: [&str; 5] = ["Off", "Left–right", "Front–back", "Up–down", "Custom plane"];
/// The index of Custom plane in [`MIRRORS`].
const CUSTOM_MIRROR: usize = 4;

/// Which of [`MIRRORS`] `mirror` is: a normal that is none of the fixed
/// planes is a custom plane.
fn mirror_index(mirror: Option<&p::MirrorPlane>) -> usize {
    match mirror.map(|plane| plane.normal) {
        None => 0,
        Some([1., 0., 0.]) => 1,
        Some([0., 1., 0.]) => 2,
        Some([0., 0., 1.]) => 3,
        Some(_) => CUSTOM_MIRROR,
    }
}

/// The mirror plane, or the left–right plane to start from.
fn mirror_plane(mapping: &p::MappingSpec) -> p::MirrorPlane {
    mapping.mirror.clone().unwrap_or(p::MirrorPlane {
        normal: [1., 0., 0.],
        offset: 0.,
    })
}

fn set_mirror(value: &mut p::Value, edit: impl FnOnce(&mut p::MirrorPlane)) {
    if let Some(mapping) = mapping_mut(value) {
        if let Some(plane) = &mut mapping.mirror {
            edit(plane);
        }
    }
}

/// The named axes: the same for an axis input and a space source.
fn axis_options() -> &'static [p::Preset] {
    static OPTIONS: std::sync::OnceLock<Vec<p::Preset>> = std::sync::OnceLock::new();
    OPTIONS.get_or_init(|| {
        p::axis_presets()
            .into_iter()
            .map(|(label, value)| p::Preset {
                label: label.into(),
                value,
            })
            .collect()
    })
}

/// The axis row: which way, what one axis spans, the mirror where the axis
/// takes one and, for radial and angle, the plane. It edits an axis input
/// or the axis of a space source.
#[allow(clippy::too_many_arguments)]
fn axis_control(
    state: &Editor,
    app: &Entity<Luma>,
    index: usize,
    name: &str,
    def: PatternArgDef,
    spec: &'static p::Input,
    mapping: p::MappingSpec,
    fields: &AxisFields,
) -> Div {
    let options = axis_options();
    let labels: Vec<&str> = options.iter().map(|option| option.label.as_str()).collect();
    let current = options.iter().position(
        |option| matches!(&option.value, p::Value::Mapping(preset) if preset.source == mapping.source),
    );
    let edit =
        move |this: &mut Luma, cx: &mut Context<Luma>, change: &dyn Fn(&mut p::MappingSpec)| {
            this.form_edit(&def, spec, cx, |value| {
                let mut value = value.clone();
                change(mapping_mut(&mut value)?);
                Some(value)
            });
        };
    let edit = Rc::new(edit);
    let pick_axis = edit.clone();
    let pick_span = edit.clone();
    let pick_mirror = edit.clone();
    let pick_plane = edit;
    let mirrors = mapping.source.takes_mirror();
    let mirror = match mapping.mirror.as_ref() {
        Some(_) if fields.custom_mirror.get() => CUSTOM_MIRROR,
        plane => mirror_index(plane),
    };
    let custom_mirror = fields.custom_mirror.clone();
    let round = mapping.plane.is_some();
    let plane = mapping.plane.as_ref().map_or(0, p::AxisPlane::index);
    let spans: Vec<&str> = p::Span::OPTIONS.iter().map(|(_, label)| *label).collect();
    let span = p::Span::OPTIONS
        .iter()
        .position(|(span, _)| *span == mapping.span)
        .unwrap_or(0);
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(menu_select(
            state,
            app,
            Menu::Choice(index),
            name.to_string(),
            current.map_or("Custom", |at| labels[at]),
            &labels,
            move |picked, this, cx| {
                let p::Value::Mapping(preset) = &options[picked].value else {
                    return;
                };
                // A new direction keeps the spans and, where it takes one,
                // the mirror; radial and angle keep their plane, Auto when
                // they had none.
                pick_axis(this, cx, &|mapping| {
                    mapping.source = preset.source.clone();
                    if !mapping.source.takes_mirror() {
                        mapping.mirror = None;
                    }
                    mapping.plane = match (&preset.plane, &mapping.plane) {
                        (None, _) => None,
                        (Some(_), Some(kept)) => Some(kept.clone()),
                        (Some(plane), None) => Some(plane.clone()),
                    };
                });
            },
        ))
        .child(arg_row(
            "Spans",
            menu_select(
                state,
                app,
                Menu::Span(index),
                format!("{name}: Spans"),
                spans[span],
                &spans,
                move |picked, this, cx| {
                    let span = p::Span::OPTIONS[picked].0;
                    pick_span(this, cx, &|mapping| mapping.span = span);
                },
            ),
        ))
        .when(mirrors, |el| {
            el.child(arg_row(
                "Mirror",
                menu_select(
                    state,
                    app,
                    Menu::Mirror(index),
                    format!("{name}: Mirror"),
                    MIRRORS[mirror],
                    &MIRRORS,
                    move |picked, this, cx| {
                        custom_mirror.set(picked == CUSTOM_MIRROR);
                        // A new plane keeps the offset; Custom plane starts
                        // from the plane there is.
                        pick_mirror(this, cx, &|mapping| {
                            let kept = mirror_plane(mapping);
                            mapping.mirror = match picked {
                                0 => None,
                                1 => Some([1., 0., 0.]),
                                2 => Some([0., 1., 0.]),
                                3 => Some([0., 0., 1.]),
                                _ => Some(kept.normal),
                            }
                            .map(|normal| p::MirrorPlane {
                                normal,
                                offset: kept.offset,
                            });
                        });
                    },
                ),
            ))
            .when(mirror == CUSTOM_MIRROR, |el| {
                el.child(arg_row(
                    "Normal · U, V, Z",
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(8.))
                        .children(fields.normal.iter().cloned()),
                ))
            })
            .when(mirror != 0, |el| {
                el.child(arg_row("Offset", fields.offset.clone()))
            })
        })
        .when(round, |el| {
            el.child(arg_row(
                "Plane",
                menu_select(
                    state,
                    app,
                    Menu::Plane(index),
                    format!("{name}: Plane"),
                    p::AxisPlane::OPTIONS[plane],
                    &p::AxisPlane::OPTIONS,
                    move |picked, this, cx| {
                        pick_plane(this, cx, &|mapping| {
                            let normal = plane_normal(mapping);
                            mapping.plane = Some(match picked {
                                1 => p::AxisPlane::UpDown,
                                2 => p::AxisPlane::FrontBack,
                                3 => p::AxisPlane::LeftRight,
                                4 => p::AxisPlane::Custom { normal },
                                _ => p::AxisPlane::Auto,
                            });
                        });
                    },
                ),
            ))
        })
        .when(plane == 4, |el| {
            el.child(arg_row(
                "Axis · U, V, Z",
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.))
                    .children(fields.plane.iter().cloned()),
            ))
        })
}

/// Closes the open menu, then runs `on_pick` with the picked index.
fn choice_pick(
    app: &Entity<Luma>,
    on_pick: impl Fn(usize, &mut Luma, &mut Context<Luma>) + 'static,
) -> impl Fn(usize, &mut Window, &mut App) + Clone {
    let app = app.clone();
    let on_pick = Rc::new(on_pick);
    move |picked, _, cx| {
        let on_pick = on_pick.clone();
        app.update(cx, |this, cx| {
            this.with_track_editor(cx, |editor| editor.sheet.open = None);
            on_pick(picked, this, cx);
        });
    }
}

/// A select that opens under this row's `Menu::Choice`.
fn choice_select(
    state: &Editor,
    app: &Entity<Luma>,
    index: usize,
    id: String,
    current: &str,
    labels: &[&str],
    on_pick: impl Fn(usize, &mut Luma, &mut Context<Luma>) + 'static,
) -> Div {
    luma_arg_select(
        id,
        current,
        labels,
        menu_visibility(state, Menu::Choice(index)),
        choice_toggle(app, index),
        choice_pick(app, on_pick),
    )
}

/// A preset picker that opens under `menu`, one of this row's menus. Its
/// Custom tile keeps the value and shows the row's editor.
#[allow(clippy::too_many_arguments)]
fn choice_presets(
    state: &Editor,
    app: &Entity<Luma>,
    index: usize,
    menu: Menu,
    id: String,
    value: &p::Envelope,
    current: Option<usize>,
    options: &[(SharedString, Thumb)],
    on_pick: impl Fn(usize, &mut Luma, &mut Context<Luma>) + 'static,
) -> Div {
    let app_pick = app.clone();
    let on_pick = Rc::new(on_pick);
    luma_preset_picker(
        id,
        &Thumb::Curve(value.clone()),
        current,
        options,
        true,
        menu_visibility(state, menu),
        menu_toggle(app, menu),
        move |picked, _, cx| {
            let on_pick = on_pick.clone();
            app_pick.update(cx, |this, cx| {
                this.with_track_editor(cx, |editor| {
                    editor.sheet.open = None;
                    let slot = editor
                        .sheet
                        .built
                        .as_mut()
                        .and_then(|built| built.cells.get_mut(index))
                        .and_then(|cell| cell.form.as_mut());
                    if let Some(slot) = slot {
                        slot.editing = picked.is_none();
                    }
                });
                if let Some(at) = picked {
                    on_pick(at, this, cx);
                }
            });
        },
    )
}

/// The shipped curve presets as the input's curves, each in the envelope
/// editor's 0–1 box.
fn curve_options(slot: &Slot) -> Vec<(SharedString, Thumb)> {
    slot.curves()
        .map(|preset| {
            let curve = scaled(slot, &preset.curve);
            (
                preset.name.clone().into(),
                Thumb::Curve(envelope_of(slot, &curve)),
            )
        })
        .collect()
}

/// The preset `curve` equals, if any.
fn curve_preset(slot: &Slot, curve: &p::Keyframes) -> Option<usize> {
    slot.curves()
        .position(|preset| same_curve(&scaled(slot, &preset.curve), curve))
}

/// A choice's options as named curves, when every option is an envelope.
fn envelope_options(options: &[p::Preset]) -> Option<Vec<(SharedString, Thumb)>> {
    options
        .iter()
        .map(|option| match &option.value {
            p::Value::Envelope(envelope) => {
                Some((option.label.clone().into(), Thumb::Curve(envelope.clone())))
            }
            _ => None,
        })
        .collect()
}

/// The control under a form row's header, or `None` for a control the plain
/// sheet draws.
fn control(
    state: &Editor,
    app: &Entity<Luma>,
    index: usize,
    cell: &Cell,
    slot: &Slot,
) -> Option<Div> {
    let name = slot.spec.name.as_str();
    let def = cell.def.clone();
    let spec = slot.spec;
    let value = decode(spec.value_type, &cell.synced).ok();
    let column = || div().w_full().flex().flex_col().gap(px(6.));
    Some(match &cell.widget {
        Widget::Direction(turns) => {
            let ends = vector_ends(value.as_ref()?);
            let timed = ends.len() > 1;
            column().children(ends.into_iter().enumerate().map(|(end, v)| {
                let (turn, tilt) = turn_tilt(v);
                let turn = turn.unwrap_or_else(|| turns.get(end).copied().unwrap_or(0.));
                let [turn_label, tilt_label] = match (timed, end) {
                    (false, _) => ["Turn", "Tilt"],
                    (true, 0) => ["Start turn", "Start tilt"],
                    (true, _) => ["End turn", "End tilt"],
                };
                let turned = app.clone();
                let turn_def = def.clone();
                let turn_scrub = direction_scrub(
                    &format!("{name}: {turn_label}"),
                    turn,
                    [-180., 180.],
                    move |turn, cx| {
                        turned.update(cx, |this, cx| {
                            edit_widget(this, index, cx, |widget| {
                                if let Widget::Direction(turns) = widget {
                                    if let Some(held) = turns.get_mut(end) {
                                        *held = turn;
                                    }
                                }
                            });
                            this.form_edit(&turn_def, spec, cx, |value| {
                                edit_vector_end(value, end, |v| direction_at(turn, turn_tilt(v).1))
                            });
                        });
                    },
                );
                let tilted = app.clone();
                let tilt_def = def.clone();
                let tilt_scrub = direction_scrub(
                    &format!("{name}: {tilt_label}"),
                    tilt,
                    [-90., 90.],
                    move |tilt, cx| {
                        tilted.update(cx, |this, cx| {
                            this.form_edit(&tilt_def, spec, cx, |value| {
                                edit_vector_end(value, end, |_| direction_at(turn, tilt))
                            });
                        });
                    },
                );
                column()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap(px(8.))
                            .child(arg_row(turn_label, turn_scrub))
                            .child(arg_row(tilt_label, tilt_scrub)),
                    )
                    .child(luma_ui::caption(vector_text(v)))
            }))
        }
        Widget::Point(sets) => {
            let timed = sets.len() > 1;
            column().children(sets.iter().enumerate().map(|(end, fields)| {
                let label = match (timed, end) {
                    (false, _) => "U, V, Z",
                    (true, 0) => "Start · U, V, Z",
                    (true, _) => "End · U, V, Z",
                };
                arg_row(
                    label,
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(8.))
                        .children(fields.iter().cloned()),
                )
            }))
        }
        Widget::Axis(fields) => {
            let Some(p::Value::Mapping(mapping)) = value.or_else(|| spec.default.clone()) else {
                return None;
            };
            axis_control(state, app, index, name, def, spec, mapping, fields)
        }
        Widget::Preset(options, editor) => {
            let options: &'static [p::Preset] = options;
            let current = options.iter().position(|option| {
                document::close(&document::wire_value(&option.value), &cell.synced)
            });
            let pick = move |picked: usize, this: &mut Luma, cx: &mut Context<Luma>| {
                this.arg_live(&def.id, document::wire_value(&options[picked].value), cx);
            };
            match (envelope_options(options), editor) {
                (Some(curves), Some(editor)) => {
                    let shown = match &value {
                        Some(p::Value::Envelope(envelope)) => envelope.clone(),
                        _ => match &curves[current.unwrap_or(0)].1 {
                            Thumb::Curve(envelope) => envelope.clone(),
                            Thumb::Gradient(_) => unreachable!("curve options"),
                        },
                    };
                    column()
                        .child(choice_presets(
                            state,
                            app,
                            index,
                            Menu::Choice(index),
                            name.to_string(),
                            &shown,
                            current,
                            &curves,
                            pick,
                        ))
                        .when(slot.editing || current.is_none(), |el| {
                            el.child(editor.clone())
                        })
                }
                _ => {
                    let labels: Vec<&str> =
                        options.iter().map(|option| option.label.as_str()).collect();
                    choice_select(
                        state,
                        app,
                        index,
                        name.to_string(),
                        current.map_or("Custom", |at| labels[at]),
                        &labels,
                        pick,
                    )
                }
            }
        }
        Widget::Grain(entity) => {
            let n = value.as_ref().and_then(level).unwrap_or(1.);
            let kind = match n {
                n if n == 1. => 0,
                n if n == 0. => 1,
                _ => 2,
            };
            let clump = entity.clone();
            column()
                .child(choice_select(
                    state,
                    app,
                    index,
                    name.to_string(),
                    GRAINS[kind],
                    &GRAINS,
                    move |picked, this, cx| {
                        let n = match picked {
                            0 => 1.,
                            1 => 0.,
                            _ => clump.read(cx).value().max(2.),
                        };
                        this.arg_live(&def.id, serde_json::json!(n), cx);
                    },
                ))
                .when(kind == 2, |el| el.child(entity.clone()))
        }
        Widget::Every(entity) => {
            let beats = value.as_ref().and_then(level).unwrap_or(0.);
            let once = beats <= 0.;
            let segments = luma_ui::float::segmented().children(EVERY.map(|label| {
                let app = app.clone();
                let id = def.id.clone();
                let chosen = (label == EVERY[0]) == once;
                let key = format!("{}-{label}", def.id);
                luma_ui::float::segment(label, chosen, key.clone())
                    .id(SharedString::from(key))
                    .on_click(move |_, _, cx| {
                        let beats = if label == EVERY[0] { 0. } else { EVERY_BEATS };
                        if chosen {
                            return;
                        }
                        app.update(cx, |this, cx| {
                            this.arg_live(&id, serde_json::json!(beats), cx)
                        });
                    })
                    .agent_node(Role::Button, format!("{name} {label}"))
            }));
            column()
                .child(segments)
                .when(!once, |el| el.child(entity.clone()))
        }
        Widget::GradientCurve(gradient, curve) => {
            let Some(
                p::Value::Time(p::SourceCurve::Gradient(read))
                | p::Value::Hit(p::SourceCurve::Gradient(read)),
            ) = value
            else {
                return None;
            };
            let curves = curves_of(p::progress_presets());
            let current = curve_at(&curves, &read.curve);
            column()
                .child(gradient.clone())
                .child(luma_ui::caption(
                    "Colors from the start of the gradient to its end".to_string(),
                ))
                .child(sub_row(
                    "Through the colors",
                    choice_presets(
                        state,
                        app,
                        index,
                        Menu::Choice(index),
                        format!("{name}: Through the colors"),
                        &read.curve,
                        current,
                        &thumbs(&curves),
                        move |picked, this, cx| {
                            let Some((_, envelope)) =
                                curves_of(p::progress_presets()).into_iter().nth(picked)
                            else {
                                return;
                            };
                            this.form_edit(&def, spec, cx, |value| {
                                edit_gradient_curve(value, |read| read.curve = envelope)
                            });
                        },
                    ),
                ))
                .when(slot.editing || current.is_none(), |el| {
                    el.child(curve.clone()).child(luma_ui::caption(format!(
                        "{} · positions in the gradient",
                        over(slot.mode)
                    )))
                })
        }
        Widget::Space(fields) => {
            let Some(p::Value::Space(space)) = value else {
                return None;
            };
            space_control(
                state,
                app,
                index,
                name,
                def,
                spec,
                &space,
                fields,
                slot.editing,
            )
        }
        Widget::Strip(entity)
            if slot.mode.is_some()
                && !matches!(
                    &value,
                    Some(p::Value::Time(curve) | p::Value::Hit(curve)) if curve.is_color()
                ) =>
        {
            let curve = match &value {
                Some(
                    p::Value::Time(p::SourceCurve::Keys(curve))
                    | p::Value::Hit(p::SourceCurve::Keys(curve)),
                ) => Some(curve),
                _ => None,
            };
            let shown = curve.map_or_else(
                || p::Envelope::linear(vec![[0., 0.], [1., 0.]]),
                |curve| envelope_of(slot, curve),
            );
            let current = curve.and_then(|curve| curve_preset(slot, curve));
            let shape = *slot;
            let span = slot.span_text(slot.range());
            let custom = slot.editing || current.is_none();
            column()
                .child(choice_presets(
                    state,
                    app,
                    index,
                    Menu::Choice(index),
                    format!("{name}:curve"),
                    &shown,
                    current,
                    &curve_options(slot),
                    move |picked, this, cx| {
                        let Some(preset) = shape.curves().nth(picked) else {
                            return;
                        };
                        let curve = scaled(&shape, &preset.curve);
                        this.form_edit(&def, spec, cx, |value| match value {
                            p::Value::Time(_) => Some(p::Value::Time(curve.into())),
                            p::Value::Hit(_) => Some(p::Value::Hit(curve.into())),
                            _ => None,
                        });
                    },
                ))
                .when(custom, |el| {
                    el.child(entity.clone()).child(luma_ui::caption(format!(
                        "{} · values {span}",
                        over(slot.mode)
                    )))
                })
        }
        Widget::Strip(entity) if slot.mode.is_some() => column()
            .child(entity.clone())
            .child(luma_ui::caption(over(slot.mode).to_string())),
        Widget::Noise([speed, low, high]) => column()
            .child(arg_row("Speed (beats)", speed.clone()))
            .child(arg_row("Range", range_row(low, high)))
            .when(slot.vector(), |el| {
                el.child(luma_ui::caption(
                    "U, V and Z each wander in this range".to_string(),
                ))
            }),
        Widget::Audio([from, to, floor, threshold]) => {
            // Named ranges only fill the two frequency fields.
            let named = &p::presets().frequencies;
            let mut labels: Vec<&str> = named.iter().map(|f| f.name.as_str()).collect();
            labels.push("Custom");
            let current = match &value {
                Some(p::Value::Audio(audio)) => named
                    .iter()
                    .find(|f| f.from_hz == audio.from_hz && f.to_hz == audio.to_hz)
                    .map_or("Custom", |f| f.name.as_str()),
                _ => "Custom",
            };
            column()
                .child(arg_row(
                    "Frequency",
                    choice_select(
                        state,
                        app,
                        index,
                        format!("{name}:frequency"),
                        current,
                        &labels,
                        move |picked, this, cx| {
                            let Some(range) = p::presets().frequencies.get(picked) else {
                                return;
                            };
                            this.form_edit(&def, spec, cx, |value| match value {
                                p::Value::Audio(audio) => Some(p::Value::Audio(p::AudioLevel {
                                    from_hz: range.from_hz,
                                    to_hz: range.to_hz,
                                    ..audio.clone()
                                })),
                                _ => None,
                            });
                        },
                    ),
                ))
                .child(arg_row("Range", range_row(from, to)))
                .child(arg_row("Floor", floor.clone()))
                // Energy below the threshold gives 0; 0% is no gate.
                .child(arg_row("Threshold", threshold.clone()))
                // The engine reads the level as a share of the input's top.
                .when(slot.unit() == Some("°"), |el| {
                    el.child(luma_ui::caption(format!(
                        "Level 0–100% gives {}",
                        slot.span_text([0., slot.range()[1]])
                    )))
                })
        }
        Widget::Color(entity) => div().child(entity.clone()),
        Widget::Scalar(entity) => div().child(entity.clone()),
        Widget::Strip(entity) => div().child(entity.clone()),
        _ => return None,
    })
}

/// A labelled part of a form row, named for the agent tree.
fn sub_row(label: &str, control: impl IntoElement) -> impl IntoElement {
    arg_row(label, control).agent_node(Role::Row, label)
}

/// A space source's rows: the axis, the gradient or the curve along it and,
/// for a number, whether it moves and how.
#[allow(clippy::too_many_arguments)]
fn space_control(
    state: &Editor,
    app: &Entity<Luma>,
    index: usize,
    name: &str,
    def: PatternArgDef,
    spec: &'static p::Input,
    space: &p::SpaceSource,
    fields: &SpaceFields,
    editing: bool,
) -> Div {
    let column = || div().w_full().flex().flex_col().gap(px(6.));
    let axis = sub_row(
        "Axis",
        axis_control(
            state,
            app,
            index,
            &format!("{name} axis"),
            def.clone(),
            spec,
            space.axis.clone(),
            &fields.axis,
        ),
    );
    let mut rows = column().child(axis);
    if space.gradient.is_some() {
        return rows.child(sub_row(
            "Colors",
            column().child(fields.along.clone()).child(luma_ui::caption(
                "From the start of the axis to its end · a tick per head".to_string(),
            )),
        ));
    }
    let movement = space.movement.as_deref();
    let moving = movement.is_some();
    // A still curve reads along the axis; a moving one is the stroke.
    let (label, curves, caption) = if moving {
        (
            "Shape",
            curves_of(p::shape_presets()),
            "Across the stroke, from its tail to its head",
        )
    } else {
        (
            "Along the axis",
            along_curves(),
            "From the start of the axis to its end",
        )
    };
    if let Some(curve) = &space.curve {
        let editor = &fields.along;
        let current = curve_at(&curves, curve);
        let pick_def = def.clone();
        rows = rows.child(sub_row(
            label,
            column()
                .child(choice_presets(
                    state,
                    app,
                    index,
                    Menu::Shape(index),
                    format!("{name}: {label}"),
                    curve,
                    current,
                    &thumbs(&curves),
                    move |picked, this, cx| {
                        let curves = if moving {
                            curves_of(p::shape_presets())
                        } else {
                            along_curves()
                        };
                        let Some((_, envelope)) = curves.into_iter().nth(picked) else {
                            return;
                        };
                        this.form_edit(&pick_def, spec, cx, |value| {
                            edit_space(value, |space| space.curve = Some(envelope))
                        });
                    },
                ))
                .when(editing || current.is_none(), |el| el.child(editor.clone()))
                .child(luma_ui::caption(caption.to_string())),
        ));
    }
    let move_def = def.clone();
    rows = rows.child(sub_row(
        "Move",
        segments(
            app,
            &format!("{name} move"),
            &["Still", "Moving"],
            usize::from(moving),
            move |picked, this, cx| {
                this.form_edit(&move_def, spec, cx, |value| {
                    edit_space(value, |space| {
                        space.movement = (picked == 1).then(|| Box::new(new_movement()));
                    })
                });
            },
        ),
    ));
    let Some(movement) = movement else {
        return rows;
    };
    let paths = curves_of(p::path_presets());
    let current = curve_at(&paths, &movement.path);
    let path_def = def.clone();
    let follows = |value: &p::Value| {
        value
            .source_kind()
            .map(|_| luma_ui::caption("Follows a curve; a number here replaces it".to_string()))
    };
    let relative_def = def.clone();
    let ends_def = def;
    rows.child(sub_row(
        "Path",
        column()
            .child(choice_presets(
                state,
                app,
                index,
                Menu::Path(index),
                format!("{name}: Path"),
                &movement.path,
                current,
                &thumbs(&paths),
                move |picked, this, cx| {
                    let Some((_, envelope)) = curves_of(p::path_presets()).into_iter().nth(picked)
                    else {
                        return;
                    };
                    this.form_edit(&path_def, spec, cx, |value| {
                        edit_movement(value, |movement| movement.path = envelope)
                    });
                },
            ))
            .when(editing || current.is_none(), |el| {
                el.child(fields.path.clone())
            }),
    ))
    .child(sub_row(
        "Travel",
        column()
            .child(fields.travel.clone())
            .children(follows(&movement.travel))
            .child(luma_ui::caption(
                "Beats to cross the axis; 0 is the whole clip".to_string(),
            )),
    ))
    .child(sub_row(
        "Width",
        column()
            .child(fields.width.clone())
            .children(follows(&movement.width))
            .child(segments(
                app,
                &format!("{name} width"),
                &["Of gap", "Of axis"],
                usize::from(!movement.width_relative),
                move |picked, this, cx| {
                    this.form_edit(&relative_def, spec, cx, |value| {
                        edit_movement(value, |movement| movement.width_relative = picked == 0)
                    });
                },
            )),
    ))
    .child(sub_row(
        "Ends",
        segments(
            app,
            &format!("{name} ends"),
            &["Clip", "Wrap"],
            usize::from(movement.boundary == p::Boundary::Wrap),
            move |picked, this, cx| {
                let boundary = if picked == 0 {
                    p::Boundary::Clip
                } else {
                    p::Boundary::Wrap
                };
                this.form_edit(&ends_def, spec, cx, |value| {
                    edit_movement(value, |movement| movement.boundary = boundary)
                });
            },
        ),
    ))
}

/// A turn or a tilt: a scrub in whole degrees, half the column wide, so the
/// two sit side by side.
fn direction_scrub(
    id: &str,
    value: f64,
    [min, max]: [f64; 2],
    on_change: impl Fn(f64, &mut App) + 'static,
) -> Stateful<Div> {
    luma_ui::float::scrub_with_unit(
        id.to_string(),
        value,
        min,
        max,
        1.,
        (FIELD_W - 8.) / 2.,
        "°",
        move |value, _, cx| on_change(value, cx),
    )
}

fn range_row(low: &Entity<DraftedNumber>, high: &Entity<DraftedNumber>) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.))
        .child(low.clone())
        .child(high.clone())
}

#[cfg(test)]
mod tests {
    use super::{
        along_curves, curve_options, curve_preset, curves_of, direction_at, edit_vector_end,
        envelope_of, envelope_options, keyframes_of, pattern_gradient, promote, same_curve, scaled,
        turn_tilt, ui_gradient, vector_ends, vector_text, Slot, Thumb, CURVE_BEATS, MIN_BEATS,
    };
    use luma_lib::models::node_graph::{PatternArgDef, PatternArgType};
    use luma_patterns as p;

    fn slot(form: &str, key: &str) -> Slot {
        let def = PatternArgDef {
            id: key.into(),
            name: key.into(),
            arg_type: PatternArgType::Scalar,
            default_value: serde_json::Value::Null,
        };
        Slot::new(form, &def, &serde_json::json!(1.0)).expect("a form input")
    }

    #[test]
    fn promoting_starts_flat_at_the_fixed_value_and_returns_to_it() {
        let every = slot("color@1", "every");
        let time = promote(&every, &p::Value::Beats(2.), Some(p::SourceKind::Time));
        assert_eq!(
            time,
            p::Value::Time(p::Keyframes::numbers(&[[0., 2.], [1., 2.]], &[]).into())
        );
        let p::Value::Time(p::SourceCurve::Keys(mut curve)) = time else {
            unreachable!()
        };
        curve.points[0].value = p::Key::Number(3.);
        assert_eq!(
            promote(&every, &p::Value::Time(curve.into()), None),
            p::Value::Beats(3.)
        );

        let color = slot("color@1", "color");
        let red = p::Value::Color([1., 0., 0.]);
        let curve = promote(&color, &red, Some(p::SourceKind::Time));
        assert!(matches!(&curve, p::Value::Time(curve) if curve.is_color()));
        assert_eq!(promote(&color, &curve, None), red);

        let alpha = slot("color@1", "alpha");
        assert_eq!(
            promote(
                &alpha,
                &p::Value::Proportion(0.8),
                Some(p::SourceKind::Audio)
            ),
            p::Value::Audio(p::AudioLevel {
                from_hz: 40.,
                to_hz: 100.,
                floor: 0.,
                threshold: 0.,
            })
        );
    }

    #[test]
    fn a_curve_round_trips_through_the_envelope_box() {
        let every = slot("color@1", "every");
        let curve = p::Keyframes::numbers(
            &[[0., 2.], [0.5, 4.], [1., 0.5]],
            &[p::Ease::Hold, p::Ease::EaseInOut],
        );
        let envelope = envelope_of(&every, &curve);
        assert!(envelope.validate().is_ok());
        assert_eq!(
            envelope,
            p::Envelope::eased(
                vec![[0., 0.25], [0.5, 0.5], [1., 0.0625]],
                &[p::Ease::Hold, p::Ease::EaseInOut]
            )
        );
        assert!(same_curve(&keyframes_of(&every, &envelope), &curve));
    }

    #[test]
    fn drawn_handles_are_stored_and_read_back_as_drawn() {
        // Handles far from any standard ease, on alpha (0–1) and on a speed.
        let drawn = p::Envelope::eased(
            vec![[0., 0.1], [0.4, 0.9], [1., 0.3]],
            &[
                p::Ease::Bezier([0.125, 0.875, 0.75, 0.125]),
                p::Ease::Bezier([0.5, 0., 0.75, 1.]),
            ],
        );
        let alpha = slot("color@1", "alpha");
        let stored = keyframes_of(&alpha, &drawn);
        assert_eq!(stored.ease(0), drawn.ease(0));
        // Stored, read back, and shown again: the same handles.
        let wire = super::document::wire_value(&p::Value::Time(stored.clone().into()));
        let Ok(p::Value::Time(p::SourceCurve::Keys(read))) =
            super::decode(alpha.spec.value_type, &wire)
        else {
            panic!("reads back")
        };
        assert_eq!(read, stored);
        assert_eq!(envelope_of(&alpha, &read), drawn);
        // What plays is what the editor draws.
        for i in 0..=40 {
            let x = f64::from(i) / 40.;
            assert!((read.sample(x)[0] - drawn.sample(x)).abs() < 1e-12, "{x}");
        }
        // On a speed the values scale and the eases stay as drawn.
        let travel = slot("color@1", "every");
        let back = envelope_of(&travel, &keyframes_of(&travel, &drawn));
        for (a, b) in back.points.iter().zip(&drawn.points) {
            assert_eq!(a.ease, b.ease);
        }
    }

    #[test]
    fn curve_presets_scale_to_the_input() {
        let travel = slot("color@1", "every");
        let up = p::presets().curve("Ramp up").unwrap();
        let curve = scaled(&travel, up);
        assert_eq!(
            curve.points.first().unwrap().value,
            p::Key::Number(MIN_BEATS)
        );
        assert_eq!(
            curve.points.last().unwrap().value,
            p::Key::Number(CURVE_BEATS)
        );
    }

    #[test]
    fn the_curve_picker_finds_the_preset_a_curve_came_from() {
        let travel = slot("color@1", "every");
        let options = curve_options(&travel);
        assert_eq!(options.len(), travel.curves().count());
        assert!(options
            .iter()
            .all(|(_, thumb)| matches!(thumb, Thumb::Curve(e) if e.validate().is_ok())));
        for (at, preset) in travel.curves().enumerate() {
            assert_eq!(
                curve_preset(&travel, &scaled(&travel, &preset.curve)),
                Some(at)
            );
        }
        let custom = p::Keyframes::numbers(&[[0., 3.], [1., 5.]], &[]);
        assert_eq!(curve_preset(&travel, &custom), None);
    }

    #[test]
    fn alpha_curves_match_after_a_round_trip() {
        let alpha = slot("color@1", "alpha");
        let names: Vec<_> = alpha.curves().map(|c| c.name.as_str()).collect();
        assert_eq!(names[0], "Full");
        assert!(!names.contains(&"Ramp up"), "{names:?}");
        // A fixed alpha promoted to a curve is the flat "Full" curve.
        let p::Value::Time(p::SourceCurve::Keys(flat)) =
            promote(&alpha, &p::Value::Proportion(1.), Some(p::SourceKind::Time))
        else {
            unreachable!()
        };
        assert_eq!(curve_preset(&alpha, &flat), Some(0));
        for (at, preset) in alpha.curves().enumerate() {
            // Stored, read back, and passed through the strip.
            let value = p::Value::Time(scaled(&alpha, &preset.curve).into());
            let wire = super::document::wire_value(&value);
            let Ok(p::Value::Time(p::SourceCurve::Keys(read))) =
                super::decode(alpha.spec.value_type, &wire)
            else {
                panic!("{} reads back", preset.name)
            };
            assert_eq!(curve_preset(&alpha, &read), Some(at), "{}", preset.name);
            let edited = keyframes_of(&alpha, &envelope_of(&alpha, &read));
            assert_eq!(curve_preset(&alpha, &edited), Some(at), "{}", preset.name);
        }
    }

    #[test]
    fn space_shapes_paths_and_curves_are_valid_curves() {
        for curves in [
            curves_of(p::shape_presets()),
            curves_of(p::path_presets()),
            curves_of(p::progress_presets()),
            along_curves(),
        ] {
            assert!(!curves.is_empty());
            assert!(curves.iter().all(|(_, curve)| curve.validate().is_ok()));
        }
        let paths = curves_of(p::path_presets());
        assert!(paths.iter().any(|(name, _)| name == "Steps (4)"));
        // The aim's axis stays a select, not a curve.
        let axis = slot("aim@1", "axis").spec;
        if let Some(p::Author::Choice { options, .. }) = &axis.author {
            assert!(envelope_options(options).is_none(), "axis stays a select");
        }
    }

    #[test]
    fn promoting_across_space_starts_from_the_fixed_value_and_returns_to_it() {
        let color = slot("color@1", "color");
        let red = p::Value::Color([1., 0., 0.]);
        let space = promote(&color, &red, Some(p::SourceKind::Space));
        let p::Value::Space(source) = &space else {
            panic!("space")
        };
        assert!(source.curve.is_none() && source.movement.is_none());
        assert_eq!(
            source.gradient.as_ref().unwrap().stops[0].color,
            [1., 0., 0.]
        );
        space.validate().unwrap();
        assert_eq!(promote(&color, &space, None), red);

        let brightness = slot("color@1", "brightness");
        let half = p::Value::Proportion(0.5);
        let space = promote(&brightness, &half, Some(p::SourceKind::Space));
        let p::Value::Space(source) = &space else {
            panic!("space")
        };
        assert!(source.gradient.is_none());
        space.validate().unwrap();
        assert_eq!(promote(&brightness, &space, None), half);
    }

    #[test]
    fn a_gradient_survives_the_strip() {
        let gradient = p::Gradient {
            stops: vec![
                p::ColorStop {
                    t: 0.,
                    color: [1., 0.5, 0.],
                    alpha: 0.25,
                },
                p::ColorStop {
                    t: 1.,
                    color: [0., 0., 1.],
                    alpha: 1.,
                },
            ],
        };
        let back = pattern_gradient(&ui_gradient(&gradient));
        for (a, b) in back.stops.iter().zip(&gradient.stops) {
            assert!((a.t - b.t).abs() < 1e-6 && (a.alpha - b.alpha).abs() < 1e-6);
            for (x, y) in a.color.iter().zip(b.color) {
                assert!((x - y).abs() < 1e-6);
            }
        }
    }

    #[test]
    fn a_direction_reads_as_turn_and_tilt() {
        // The resting aim: 40° down toward downstage.
        let rest = [0., 0.766, -0.643];
        let (turn, tilt) = turn_tilt(rest);
        assert!(
            turn.unwrap().abs() < 1e-9 && (tilt + 40.).abs() < 0.02,
            "{tilt}"
        );
        assert_eq!(direction_at(0., -40.), [0., 0.766, -0.6428]);
        // Turn grows toward stage right.
        assert_eq!(direction_at(90., 0.), [1., 0., 0.]);
        // Straight down says no turn.
        assert_eq!(turn_tilt([0., 0., -1.]), (None, -90.));
        assert_eq!(vector_text(rest), "U 0.00 · V 0.77 · Z −0.64");
    }

    #[test]
    fn a_vector_promotes_to_a_curve_whose_ends_edit_apart() {
        let direction = slot("aim@1", "direction");
        let rest = p::Value::Vector([0., 0.766, -0.643]);
        let curve = promote(&direction, &rest, Some(p::SourceKind::Time));
        assert_eq!(vector_ends(&curve), vec![[0., 0.766, -0.643]; 2]);
        let moved = edit_vector_end(&curve, 1, |_| [0., 0., -1.]).unwrap();
        assert_eq!(
            vector_ends(&moved),
            vec![[0., 0.766, -0.643], [0., 0., -1.]]
        );
        assert_eq!(promote(&direction, &moved, None), rest);
        let p::Value::Noise(noise) = promote(&direction, &rest, Some(p::SourceKind::Noise)) else {
            panic!("noise")
        };
        assert_eq!(noise.range, [-1., 1.]);
        // Audio reads 0–1 as 0 to the most degrees; fixed again, it is the top.
        let fan = slot("aim@1", "fan");
        let audio = promote(&fan, &p::Value::Number(30.), Some(p::SourceKind::Audio));
        assert_eq!(promote(&fan, &audio, None), p::Value::Number(90.));
    }
}
